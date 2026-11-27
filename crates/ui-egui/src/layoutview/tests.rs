use super::{
    FRAMES_REDRAW_S, PageWait, RULER, first_ink, handle_at, landing, page_rect, redraw_due, resized, rotate_handle, snap_to, to_screen,
    turned,
};
use crate::RustGisApp;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};
use serde_json::json;
use std::{cell::Cell, rc::Rc};

/// A page or preview says each kind of approximation once, as the map view does, though its
/// count changes as the page is drawn again (#2657).
#[test]
fn frame_approximations_are_said_once_a_kind() {
    let mut app = RustGisApp::with_sample();
    let mark = app.session.notice_mark();
    let said = |n: u32| rustgis_engine::render::ShadowsSaid {
        notices: vec![format!("Roads: {n} dashed line(s) drawn solid")],
        keys: vec!["approximated\0Roads\0dashes".to_string()],
        to_come: false,
    };
    super::say_frame_shadows(&mut app, &said(5));
    super::say_frame_shadows(&mut app, &said(7));
    let n = app.session.notices_since(mark).iter().filter(|n| n.text.starts_with("Roads: ")).count();
    assert_eq!(n, 1);
}

/// Ink starts white only over a dark background, so turning it on never hides the text (#2257).
#[test]
fn ink_starts_readable_on_its_background() {
    use rustgis_engine::map::Rgba;
    use rustgis_engine::map::layout::{Background, INK};
    let white = Rgba::rgb(0xFF, 0xFF, 0xFF);
    let on = |fill: Rgba| first_ink(Some(&Background { fill, ..serde_json::from_value(json!({})).unwrap() }), white);
    assert_eq!(first_ink(None, white), INK);
    assert_eq!(on(white), INK);
    assert_eq!(on(Rgba::rgb(0x1E, 0x29, 0x3B)), white);
    // A dark fill seen through mostly: the page shows, so it reads as light.
    assert_eq!(on(Rgba::new(0x1E, 0x29, 0x3B, 0x20)), INK);
    assert_eq!(on(Rgba::new(0x1E, 0x29, 0x3B, 0xE0)), white);
    // On a dark page (#2855): white with no background, or over a fill the page shows through;
    // dark grey over a light fill.
    let navy = Rgba::rgb(0x0B, 0x1B, 0x2B);
    let on_navy = |fill: Rgba| first_ink(Some(&Background { fill, ..serde_json::from_value(json!({})).unwrap() }), navy);
    assert_eq!(first_ink(None, navy), white);
    assert_eq!(on_navy(Rgba::new(0xFF, 0xFF, 0xFF, 0x20)), white);
    assert_eq!(on_navy(white), INK);
}

/// In the browser, a page drawn while its frames' tiles were on their way is drawn again as
/// they land, at most every `FRAMES_REDRAW_S`, without asking about legends.
#[test]
fn a_page_missing_tiles_is_drawn_again_as_they_land() {
    let w = PageWait { landed: (7, 3), frames: true, drawn: 10.0 };
    let legends = || panic!("the frames' wait doesn't ask about legends");
    assert!(redraw_due(w, (8, 3), 10.0 + FRAMES_REDRAW_S, legends), "a tile landed, long enough since the last drawing");
    assert!(!redraw_due(w, (8, 3), 10.0 + FRAMES_REDRAW_S / 2.0, legends), "a tile landed, but too soon after the last drawing");
    assert!(!redraw_due(w, (7, 3), 60.0, legends), "nothing landed");
}

/// A streamed layer's read, or a source opened by its address, lands outside the tile cache:
/// a frame drawn short of it is drawn again too (#1534).
#[test]
fn a_page_missing_a_streamed_read_is_drawn_again_as_it_lands() {
    let w = PageWait { landed: (7, 3), frames: true, drawn: 10.0 };
    assert!(redraw_due(w, (7, 4), 10.0 + FRAMES_REDRAW_S, || false), "a read landed");
    assert!(!redraw_due(w, (7, 4), 10.0 + FRAMES_REDRAW_S / 2.0, || false), "a read landed, but too soon");
}

/// A page waiting only for legend pictures is drawn again once they have all landed, not on
/// every fetch: the map's own tiles land too (#1177).
#[test]
fn a_page_missing_legend_pictures_is_drawn_again_once_they_land() {
    let w = PageWait { landed: (7, 3), frames: false, drawn: 10.0 };
    assert!(redraw_due(w, (8, 3), 10.0, || false), "the pictures landed");
    assert!(!redraw_due(w, (8, 3), 60.0, || true), "a fetch landed, but a picture is still on its way");
    assert!(!redraw_due(w, (7, 3), 60.0, || false), "nothing landed");
}

#[test]
fn the_desktop_page_waits_for_its_legend_not_unrelated_fetches() {
    use rustgis_engine::map::{TileKind, TileLayer, layout::ElementKind};
    use rustgis_engine::render::rustgis_tiles::cache::{self, Want};
    let _turn = crate::fetching::turn();
    let (tx, rx) = std::sync::mpsc::channel();
    cache::set_fetcher(move |url, done| {
        tx.send((url, done)).unwrap();
    });
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(24, 24, image::Rgba([220, 20, 20, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = png.into_inner();
    let url = "https://layout-wait.test/legend.png";
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Pending legend"})).unwrap();
    let li = app.session.active_layout.unwrap();
    for e in &mut app.session.project.layouts[li].elements {
        match &mut e.kind {
            ElementKind::MapFrame { .. } => e.visible = false,
            ElementKind::Legend { all_classes, .. } => *all_classes = true,
            _ => {}
        }
    }
    let mut tile = TileLayer::new("Walls", TileKind::Raster, "https://layout-wait.test/{z}/{x}/{y}.png");
    tile.legends = vec![rustgis_engine::data::ogc::Legend { url: url.into(), ..Default::default() }];
    app.session.map_mut().tiles.push(tile);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let (asked, legend) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(asked, url);
    let wait = h.state().layout.page_wait.expect("the desktop keeps the incomplete page pending");
    assert!(!wait.frames, "legend-only loading must not use progressive frame redraws");
    let _ = cache::get("https://layout-wait.test/unrelated.png", Want::Image);
    let (_, unrelated) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    unrelated(Ok(png.clone()));
    h.step();
    assert_eq!(h.state().layout.page_wait.unwrap().drawn, wait.drawn, "an unrelated fetch does not redraw the page");
    legend(Ok(png));
    h.step();
    assert!(h.state().layout.page_wait.is_none(), "the legend's completion redraws and settles the page");
    h.step();
    assert!(h.state().layout.page_wait.is_none(), "the completed page stays settled");
}

#[test]
fn turns_snap_to_level_upright_and_diagonal() {
    assert_eq!(turned(0.0, 90.0, 0.0, false), -90.0, "a quarter turn clockwise");
    assert_eq!(turned(10.0, 0.0, 33.0, false), 45.0, "43° snaps to 45°");
    assert!((turned(10.0, 0.0, 20.0, false) - 30.0).abs() < 1e-9, "30° is left alone");
    assert_eq!(turned(0.0, 0.0, 20.0, true), 15.0, "Shift: 15° steps");
    assert!((turned(170.0, 0.0, 30.0, false) + 160.0).abs() < 1e-9, "200° is -160°");
}

/// A text turned 90° shows its handles on its turned box (#1763): its right-hand handle sits
/// at the bottom, and dragging it down lengthens the text, its top staying put.
#[test]
fn a_turned_text_resizes_by_its_turned_handles() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let p = json!({"layout": li, "kind": "text", "text": "Fonte: IBGE", "rotation": 90, "x": 60, "y": 100, "w": 80, "h": 10});
    let id = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.selected = Some(id);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    // The box's right-hand end, 40 mm right of its centre (100, 105), turned to 40 mm below it.
    let from = to_screen(page, s, (100.0, 145.0));
    assert_eq!(handle_at(h.state(), &layout, page, s, from), Some((id, 3)), "the right-hand handle, turned");
    let to = to_screen(page, s, (100.0, 165.0));
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(button(from, true));
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
        h.step();
    }
    h.event(button(to, false));
    h.step();
    let e = h.state().session.project.layouts[li].element(id).unwrap();
    assert!((e.w - 100.0).abs() < 1.0 && (e.h - 10.0).abs() < 1e-9, "20 mm longer: {} × {}", e.w, e.h);
    let (bx, by, bw, bh) = e.bounds();
    assert!((by - 65.0).abs() < 1e-6 && (bx - 95.0).abs() < 1e-6, "its top and side stay: {bx}, {by}");
    assert!((bh - e.w).abs() < 1e-6 && (bw - 10.0).abs() < 1e-6);
}

/// Navigating map frames, a selected text on a map frame keeps its handles, where they stick
/// out over the map too: dragging one resizes the text and leaves the map be. Another
/// element's handle over the frame still pans the map (#1763).
#[test]
fn a_text_on_a_map_frame_keeps_its_handles_while_navigating() {
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Nav"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let frame = app.session.project.layouts[li].first_frame().unwrap();
    let f = app.session.project.layouts[li].element(frame).unwrap().clone();
    let (x, y) = (f.x + 10.0, f.y + 10.0);
    let p = json!({"layout": li, "kind": "text", "text": "Fonte: IBGE", "x": x, "y": y, "w": 40, "h": 8});
    let text = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    let p = json!({"layout": li, "kind": "rectangle", "x": x, "y": y + 20.0, "w": 40, "h": 20, "fill": "#00000000"});
    let rect = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.navigate = true;
    app.layout.selected = Some(text);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let center = |l: &rustgis_engine::map::layout::Layout| match &l.element(frame).unwrap().kind {
        ElementKind::MapFrame { center, .. } => *center,
        k => panic!("{k:?}"),
    };
    let before = center(&layout);
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    // Just right of the text's right-hand handle: off the text, over the map.
    let from = to_screen(page, s, (x + 40.0, y + 4.0)) + vec2(3.0, 0.0);
    assert_eq!(layout.hit(x + 40.0 + f64::from(3.0 / s), y + 4.0), Some(frame), "the point is on the map frame");
    assert_eq!(handle_at(h.state(), &layout, page, s, from), Some((text, 3)), "the text's handle comes first");
    let to = to_screen(page, s, (x + 60.0, y + 4.0)) + vec2(3.0, 0.0);
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(button(from, true));
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
        h.step();
    }
    h.event(button(to, false));
    h.run_steps(2);
    let after = h.state().session.project.layouts[li].clone();
    let e = after.element(text).unwrap();
    assert!((e.w - 60.0).abs() < 1.0 && (e.x - x).abs() < 1e-9, "20 mm wider: {} at {}", e.w, e.x);
    assert_eq!(center(&after), before, "the map stayed put");
    // A rectangle's handle there is the map's, as before.
    h.state_mut().layout.selected = Some(rect);
    let by = to_screen(page, s, (x + 40.0, y + 30.0)) + vec2(3.0, 0.0);
    assert_eq!(handle_at(h.state(), &after, page, s, by), None, "the rectangle's handle gives way to panning");
}

/// Dragging an arrow's round handle from above its centre round to its right turns it a
/// quarter turn clockwise, about its centre, keeping its length.
#[test]
fn an_arrow_turns_by_its_rotation_handle() {
    use rustgis_engine::map::layout::{ElementKind, line_angle, line_length};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let id = app.run("layout.addElement", json!({"layout": li, "kind": "arrow", "angle": 0, "length": 60})).unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    app.layout.selected = Some(id);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let (_, knob, c) = rotate_handle(h.state(), &layout, page, s).unwrap();
    let to = c + vec2((knob - c).length(), 0.0);
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(knob));
    h.step();
    h.event(button(knob, true));
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(knob + (to - knob) * i as f32 / 6.0));
        h.step();
    }
    h.event(button(to, false));
    h.step();
    let app = h.state();
    let e = app.session.project.layouts[li].element(id).unwrap();
    let ElementKind::Line { dir, head, .. } = e.kind else { panic!("not a line") };
    let r = (e.x, e.y, e.w, e.h);
    assert!((line_angle(r, dir, head) + 90.0).abs() < 1e-9, "points down: {}°", line_angle(r, dir, head));
    assert!((line_length(r, dir) - 60.0).abs() < 1e-6);
    let old = layout.element(id).unwrap();
    assert!(((e.x + e.w / 2.0) - (old.x + old.w / 2.0)).abs() < 1e-9 && ((e.y + e.h / 2.0) - (old.y + old.h / 2.0)).abs() < 1e-9);
    assert_eq!(app.layout.selected, Some(id), "still selected");
}

/// While a map frame is panned, its map is drawn past the frame's sides and slid with the
/// pointer, so what comes into view is map rather than the page beside the frame; the drawing
/// is let go when the pan ends, which moves the frame's map as before.
#[test]
fn a_panned_frame_shows_map_beyond_its_edges() {
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Pan"})).unwrap();
    app.layout.navigate = true;
    let li = app.session.active_layout.unwrap();
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let id = layout.first_frame().unwrap();
    let center = |l: &rustgis_engine::map::layout::Layout| match &l.element(id).unwrap().kind {
        ElementKind::MapFrame { center, .. } => *center,
        k => panic!("{k:?}"),
    };
    let before = center(&layout);
    let e = layout.element(id).unwrap();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let from = to_screen(page, s, (e.x + e.w / 2.0, e.y + e.h / 2.0));
    let to = from + vec2(120.0, 0.0);
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(button(from, true));
    h.step();
    for i in 1..=4 {
        h.event(Event::PointerMoved(from + (to - from) * i as f32 / 4.0));
        h.step();
    }
    // Mid-pan: the frame's map with its overscan is drawn, three frames wide and high.
    let (pid, map) = h.state().layout.pan_preview.clone().expect("a preview while panning");
    assert_eq!(pid, id);
    let size = map.expect("the frame's map was drawn").size();
    let frame_px = (e.w as f32 * s * h.ctx.pixels_per_point(), e.h as f32 * s * h.ctx.pixels_per_point());
    assert!(
        (size[0] as f32 - 3.0 * frame_px.0).abs() < 3.0 && (size[1] as f32 - 3.0 * frame_px.1).abs() < 3.0,
        "{size:?} for a frame of {frame_px:?} px"
    );
    h.event(button(to, false));
    h.step();
    assert!(h.state().layout.pan_preview.is_none(), "let go after the pan");
    assert_ne!(center(&h.state().session.project.layouts[li]), before, "the pan moved the frame's map");
}

/// Pressing a corner handle and dragging away fast resizes the element, though by the time
/// egui starts the drag the pointer is already off the handle; it used to move the element.
#[test]
fn a_fast_drag_from_a_corner_handle_resizes() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Resize"})).unwrap();
    app.layout.no_snap = true;
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    app.layout.selected = Some(title.id);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    // A little inside the bottom-right handle, not on its centre: no bull's-eye needed.
    let corner = to_screen(page, s, (title.x + title.w, title.y + title.h)) + vec2(-4.0, -3.0);
    let to = corner + vec2(60.0, 30.0);
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(corner));
    h.step();
    h.event(button(corner, true));
    h.step();
    // Three big steps: the first already leaves the handle behind.
    for i in 1..=3 {
        h.event(Event::PointerMoved(corner + (to - corner) * i as f32 / 3.0));
        h.step();
    }
    h.event(button(to, false));
    h.step();
    let app = h.state();
    let (_, s) = app.layout.drag_page.unwrap();
    let e = app.session.project.layouts[li].element(title.id).unwrap();
    assert!(
        (e.x - title.x).abs() < 0.01 && (e.y - title.y).abs() < 0.01,
        "the top-left stays: {:?} → {:?}",
        (title.x, title.y),
        (e.x, e.y)
    );
    let (dw, dh) = ((e.w - title.w) * s as f64, (e.h - title.h) * s as f64);
    assert!((59.0..=61.0).contains(&dw) && (29.0..=31.0).contains(&dh), "grew by {dw} × {dh} points, not 60 × 30");
}

/// A click that starts on the selected element's handle keeps the selection, though the
/// pointer moved off the handle (and off the element) before the release.
#[test]
fn a_click_started_on_a_handle_keeps_the_selection() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Click"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    app.layout.selected = Some(title.id);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    // Pressed at the edge of the bottom-right handle's reach, released a few points further
    // out: still a click, but on nothing.
    let corner = to_screen(page, s, (title.x + title.w, title.y + title.h));
    let (from, to) = (corner + vec2(8.0, 8.0), corner + vec2(12.0, 12.0));
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(button(from, true));
    h.step();
    h.event(Event::PointerMoved(to));
    h.step();
    h.event(button(to, false));
    h.run_steps(2);
    let (x, y) = super::to_mm(page, s, to);
    assert!(layout.hit(x, y).is_none(), "the release is off every element");
    assert_eq!(h.state().layout.selected, Some(title.id));
}

/// Dragging an element redraws the page with it where it was dropped (#325).
#[test]
fn dropped_element_is_drawn_where_it_lands() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Drag"})).unwrap();
    // The drop itself, not snapping (tested below).
    app.layout.no_snap = true;
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    // Fonts take effect on the frame after they are installed.
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let li = h.state().session.active_layout.unwrap();
    let layout = h.state().session.project.layouts[li].clone();
    let title = layout.elements.iter().find(|e| e.name == "Title").unwrap().clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let from = to_screen(page, s, (title.x + title.w / 2.0, title.y + title.h / 2.0));
    let to = from + vec2(0.0, 120.0);
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(button(from, true));
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
        h.step();
    }
    let before = h.state().session.revision;
    h.event(button(to, false));
    h.step();
    let app = h.state();
    assert!(app.session.revision > before, "the drop should update the layout");
    let moved = app.session.project.layouts[li].element(title.id).unwrap();
    // A vertical drag moves it straight down by the pointer's motion, although the
    // properties opened beside the page and re-fitted it mid-drag.
    let (_, s) = app.layout.drag_page.unwrap();
    assert!((moved.x - title.x).abs() < 0.01, "x moved from {} to {}", title.x, moved.x);
    // The drag runs from where the button went down, so the element moved with the pointer.
    let dy = (moved.y - title.y) * s as f64;
    assert!((119.0..=120.5).contains(&dy), "y moved {dy} points");
    // The page was drawn, and keyed, from the layout as it is after the drop.
    let (i, rev, drawn) = app.layout.shown.as_ref().unwrap();
    assert_eq!((*i, *rev), (li, app.session.revision));
    assert_eq!(drawn.element(title.id).map(|e| e.y), Some(moved.y));
    assert_eq!(app.layout.key.map(|k| k.0), Some(app.session.revision));
}

/// Dragging the map in a turned frame moves it the way the pointer goes, as in the map view.
#[test]
fn panning_a_turned_frame_follows_the_pointer() {
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Turned"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let id = app.session.project.layouts[li].first_frame().unwrap();
    // East at the top: the page's right is south, its left north.
    app.run("layout.update", json!({"layout": li, "element": id, "bearing": 90, "center": [10.0, 20.0]})).unwrap();
    let layout = app.session.project.layouts[li].clone();
    super::pan_frame(&mut app, &layout, li, id, 10.0, 0.0);
    let Some(ElementKind::MapFrame { center, .. }) = app.session.project.layouts[li].element(id).map(|e| e.kind.clone()) else {
        panic!("not a map frame")
    };
    // Dragged right, the map brings in what lay to the left: the centre moves north.
    assert!(center.y > 20.5 && (center.x - 10.0).abs() < 0.01, "{center:?}");
}

/// Zooming a map frame about a point keeps the map under that point where it was.
#[test]
fn zooming_a_frame_keeps_the_map_under_the_pointer() {
    use rustgis_engine::map::layout::{ElementKind, PT_PER_MM, frame_camera};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Zoom"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let layout = app.session.project.layouts[li].clone();
    let frame = layout.elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().clone();
    // A point a quarter of the way into the frame.
    let anchor = (frame.x + frame.w / 4.0, frame.y + frame.h / 4.0);
    // The map point (map units) at the anchor, the frame's scale, and map units per point.
    let map_at = |app: &RustGisApp| {
        let e = app.session.project.layouts[li].element(frame.id).unwrap().clone();
        let ElementKind::MapFrame { map, center, scale, .. } = &e.kind else { panic!("not a map frame") };
        let m = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).unwrap();
        let cam = frame_camera(m, *center, *scale, 0.0, 0.0);
        let x = cam.center.x + (anchor.0 - (e.x + e.w / 2.0)) * PT_PER_MM * cam.res;
        let y = cam.center.y - (anchor.1 - (e.y + e.h / 2.0)) * PT_PER_MM * cam.res;
        (x, y, *scale, cam.res)
    };
    let before = map_at(&app);
    let undo = app.session.undo_depth();
    super::zoom_frame(&mut app, &layout, li, frame.id, 0.5, anchor);
    let after = map_at(&app);
    assert!((after.2 - before.2 * 0.5).abs() < 1e-6 * before.2, "the scale halves: {} → {}", before.2, after.2);
    assert!((after.0 - before.0).hypot(after.1 - before.1) < after.3, "the anchor's map point moved: {before:?} → {after:?}");
    assert_eq!(app.session.undo_depth(), undo + 1, "one undo step");
}

/// A wheel zoom still waiting is applied, not lost, when another frame is zoomed; one waiting
/// for a frame of another layout is dropped, never applied to the open layout.
#[test]
fn waiting_frame_zooms_are_applied_or_dropped_never_misapplied() {
    use rustgis_engine::map::layout::ElementKind;
    let scale_of = |app: &RustGisApp, li: usize, id: u32| match app.session.project.layouts[li].element(id).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { scale, .. }) => *scale,
        k => panic!("not a map frame: {k:?}"),
    };
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Two"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let name = app.session.project.layouts[li].name.clone();
    let a = app.session.project.layouts[li].first_frame().unwrap();
    let b =
        app.run("layout.addFrame", json!({"layout": li, "x": 20, "y": 20, "w": 60, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.navigate = true;
    let (sa, sb) = (scale_of(&app, li, a), scale_of(&app, li, b));
    // Frame A's zoom waits: its wheel turned "later than ever", so it can't settle by itself.
    app.layout.frame_zoom = Some((name.clone(), a, 0.5, (100.0, 100.0), f64::MAX));
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(2);
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    // Zoom in over frame B.
    h.event(Event::PointerMoved(to_screen(page, s, (50.0, 40.0))));
    h.step();
    h.event(Event::Zoom(1.25));
    h.step();
    assert!((scale_of(h.state(), li, a) - sa * 0.5).abs() < 1e-6 * sa, "A's waiting zoom was applied");
    assert_eq!(h.state().layout.frame_zoom.as_ref().map(|z| z.1), Some(b), "B's zoom waits for the wheel to stop");
    h.run_steps(3);
    assert!(h.state().layout.frame_zoom.is_none());
    assert!(scale_of(h.state(), li, b) < sb, "B zoomed in once the wheel stopped");

    // A zoom left waiting by another layout is dropped without touching this one.
    h.state_mut().layout.frame_zoom = Some(("Another layout".into(), a, 0.5, (100.0, 100.0), 0.0));
    let rev = h.state().session.revision;
    h.run_steps(2);
    assert!(h.state().layout.frame_zoom.is_none());
    assert_eq!(h.state().session.revision, rev, "nothing was applied");
}

/// Arrow keys nudge the selected element 1 mm, 10 mm with Shift; a run of them, held or
/// tapped, is one step to undo; a locked element stays put.
#[test]
fn arrow_keys_nudge_the_selected_element_and_undo_as_one() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Nudge"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    app.layout.selected = Some(title.id);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            let ctx = ui.ctx().clone();
            crate::chrome::shortcuts(app, &ctx);
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let key = |key, modifiers, repeat| Event::Key { key, physical_key: None, pressed: true, repeat, modifiers };
    let at = |h: &egui_kittest::Harness<'_, RustGisApp>| {
        let e = h.state().session.project.layouts[li].element(title.id).unwrap();
        (e.x - title.x, e.y - title.y)
    };
    let depth = h.state().session.undo_depth();
    h.event(key(Key::ArrowRight, Modifiers::NONE, false));
    h.step();
    assert_eq!(at(&h), (1.0, 0.0), "→ nudges 1 mm");
    h.event(key(Key::ArrowDown, Modifiers::SHIFT, false));
    h.step();
    assert_eq!(at(&h), (1.0, 10.0), "Shift-↓ nudges 10 mm");
    // A held key repeats, several times in a frame.
    for _ in 0..3 {
        h.event(key(Key::ArrowLeft, Modifiers::NONE, true));
    }
    h.step();
    assert_eq!(at(&h), (-2.0, 10.0));
    assert_eq!(h.state().session.undo_depth(), depth + 1, "one step to undo, not five");
    h.state_mut().run("edit.undo", serde_json::Value::Null).unwrap();
    assert_eq!(at(&h), (0.0, 0.0), "one undo puts it back");

    // A locked element stays put.
    h.state_mut().run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
    h.event(key(Key::ArrowUp, Modifiers::NONE, false));
    h.step();
    assert_eq!(at(&h), (0.0, 0.0));
}

/// A rectangle's outline is dashed from the Format pane's Dash list, the Symbology pane's
/// presets scaled by its width, and made solid again (#1763).
#[test]
fn the_format_pane_dashes_an_outline() {
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Dashes", "template": "blank"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let p = json!({"layout": li, "kind": "rectangle", "x": 20, "y": 20, "w": 60, "h": 40, "width": 2});
    let id = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.selected = Some(id);
    let mut h = egui_kittest::Harness::builder().with_size(vec2(420.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            let layout = app.session.project.layouts[li].clone();
            egui::ScrollArea::vertical().show(ui, |ui| super::properties(app, ui, &layout, li));
        },
        app,
    );
    h.run_steps(2);
    let dash = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(id).unwrap().kind {
        ElementKind::Rectangle { dash, .. } => dash.map(|d| d.steps().to_vec()),
        k => panic!("{k:?}"),
    };
    h.get_by_label("Dash").click();
    h.run_steps(2);
    h.get_by_label("Dashed").click();
    h.run_steps(2);
    assert_eq!(dash(&h), Some(vec![8.0, 4.0]), "Dashed, [4, 2] at a 2-point width");
    h.get_by_label("Dash").click();
    h.run_steps(2);
    h.get_by_label("Solid").click();
    h.run_steps(2);
    assert_eq!(dash(&h), None);
}

/// The selected element's Bring to Front and Send to Back buttons move it in the drawing
/// order; a step it can't take is greyed.
#[test]
fn the_arrange_buttons_move_an_element_up_and_down() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Arrange"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let frame = app.session.project.layouts[li].first_frame().unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 200.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            let layout = app.session.project.layouts[li].clone();
            super::element_actions(app, ui, &layout, li, frame);
        },
        app,
    );
    h.run_steps(2);
    let at = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].draw_order(frame).unwrap();
    let n = at(&h).1;
    h.get_by_label("Bring to Front").click();
    h.run_steps(2);
    assert_eq!(at(&h), (n - 1, n));
    // On top, it can't go further up.
    assert!(h.get_by_label("Bring Forward").accesskit_node().is_disabled());
    h.get_by_label("Send Backward").click();
    h.run_steps(2);
    assert_eq!(at(&h).0, n - 2);
    h.get_by_label("Send to Back").click();
    h.run_steps(2);
    assert_eq!(at(&h).0, 0);
    assert!(h.get_by_label("Send to Back").accesskit_node().is_disabled());
}

/// Ctrl+D (Cmd+D) duplicates the selected element and selects the copy; Backspace, the Mac's
/// delete key, deletes it.
#[test]
fn keys_duplicate_and_delete_the_selected_element() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Keys"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    app.layout.selected = Some(title.id);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            let ctx = ui.ctx().clone();
            crate::chrome::shortcuts(app, &ctx);
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    let n = h.state().session.project.layouts[li].elements.len();

    h.event(key(Key::D, Modifiers::COMMAND));
    h.step();
    let app = h.state();
    let layout = &app.session.project.layouts[li];
    assert_eq!(layout.elements.len(), n + 1, "Ctrl+D added a copy");
    let copy = layout.element(app.layout.selected.unwrap()).unwrap();
    assert_ne!(copy.id, title.id, "the copy is selected");
    assert_eq!((copy.name.as_str(), copy.x, copy.y, copy.w), ("Title 2", title.x + 5.0, title.y + 5.0, title.w));
    let copy = copy.id;

    h.event(key(Key::Backspace, Modifiers::NONE));
    h.step();
    let app = h.state();
    let layout = &app.session.project.layouts[li];
    assert_eq!(layout.elements.len(), n, "Backspace deleted the copy");
    assert!(layout.element(copy).is_none() && layout.element(title.id).is_some());
    assert_eq!(app.layout.selected, None);
}

/// A right-click on the selected element's handle, where it sticks out over another element,
/// keeps the selection and opens the menu of the element the handle belongs to, as a click
/// there keeps it, also when the pointer moves off the handle before the release.
#[test]
fn right_clicking_a_handle_keeps_the_selection() {
    use egui_kittest::kittest::Queryable;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Handles"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let start = app.session.project.layouts[li].clone();
    let title = start.elements.iter().find(|e| e.name == "Title").unwrap().id;
    let frame = start.first_frame().unwrap();
    let top = start.element(frame).unwrap().y;
    let h_mm = start.element(title).unwrap().h;
    // The title's lower edge on the frame's upper one, so its lower handles stick out over the map.
    app.run("layout.update", json!({"layout": li, "element": title, "y": top - h_mm})).unwrap();
    app.layout.selected = Some(title);
    let layout = app.session.project.layouts[li].clone();
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let e = layout.element(title).unwrap();
    // Just below the lower middle handle: on the map frame, off the title, within the handle's reach.
    let at = to_screen(page, s, (e.x + e.w / 2.0, e.y + e.h)) + vec2(0.0, 4.0);
    let (x, y) = super::to_mm(page, s, at);
    assert_eq!(layout.hit(x, y), Some(frame), "the point is on the map frame");
    assert!(handle_at(h.state(), &layout, page, s, at).is_some(), "and on the title's handle");
    let secondary =
        |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
    h.event(Event::PointerMoved(at));
    h.step();
    h.event(secondary(at, true));
    h.step();
    h.event(secondary(at, false));
    h.run_steps(3);
    assert_eq!(h.state().layout.selected, Some(title), "the title stays selected");
    assert!(h.query_by_label_contains("Duplicate").is_some(), "its menu opens");
    assert!(h.query_by_label("Zoom Frame").is_none(), "the title's menu, not the frame's");

    // Pressed on the handle, released a little off it (still a click): where it was pressed
    // decides, as for a click.
    h.event(Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
    let handle = to_screen(page, s, (e.x + e.w / 2.0, e.y + e.h));
    let (press, release) = (handle + vec2(0.0, 10.0), handle + vec2(0.0, 15.0));
    assert!(handle_at(h.state(), &layout, page, s, press).is_some(), "pressed on the handle");
    assert!(handle_at(h.state(), &layout, page, s, release).is_none(), "released off it");
    let (x, y) = super::to_mm(page, s, release);
    assert_eq!(layout.hit(x, y), Some(frame), "on the map frame");
    h.event(Event::PointerMoved(press));
    h.step();
    h.event(secondary(press, true));
    h.step();
    h.event(Event::PointerMoved(release));
    h.step();
    h.event(secondary(release, false));
    h.run_steps(3);
    assert_eq!(h.state().layout.selected, Some(title), "the title stays selected after the drift");
}

/// Right-clicking an element selects it and opens its menu: Send Backward moves it down the
/// drawing order in one Undo step, a locked element's Delete row is greyed, and a right-click
/// on empty page opens no menu.
#[test]
fn right_clicking_an_element_opens_its_menu() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Menu"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let layout = app.session.project.layouts[li].clone();
    let title = layout.elements.iter().find(|e| e.name == "Title").unwrap().clone();
    let frame = layout.first_frame().unwrap();
    app.layout.selected = Some(frame);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let (cx, cy) = (title.x + title.w / 2.0, title.y + title.h / 2.0);
    assert_eq!(layout.hit(cx, cy), Some(title.id), "the point is on the title");
    let at = to_screen(page, s, (cx, cy));
    let secondary =
        |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
    let order = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].draw_order(title.id).unwrap().0;
    let right_click = |h: &mut egui_kittest::Harness<'_, RustGisApp>, pos: Pos2| {
        h.event(Event::PointerMoved(pos));
        h.step();
        h.event(secondary(pos, true));
        h.step();
        h.event(secondary(pos, false));
        h.run_steps(3);
    };

    right_click(&mut h, at);
    assert_eq!(h.state().layout.selected, Some(title.id), "the right-click selected the title");
    let before = order(&h);
    assert!(before > 0, "the title has an element under it to go behind");
    for row in ["Duplicate", "Delete Element", "Bring to Front", "Bring Forward", "Send Backward", "Send to Back"] {
        assert!(h.query_by_label_contains(row).is_some(), "{row} is in the menu");
    }
    assert!(h.query_by_label("Zoom Frame").is_none(), "Zoom Frame is for a map frame");
    assert!(!h.get_by_label_contains("Delete Element").accesskit_node().is_disabled());
    h.get_by_label("Send Backward").click();
    h.step();
    // The page is drawn, in the frame the row changed the layout, from the layout as it is
    // now: a snapshot taken before would be keyed to the new revision but show the old order.
    assert_eq!(
        h.state().layout.shown.as_ref().map(|(_, rev, _)| *rev),
        Some(h.state().session.revision),
        "the page drawn after the menu's change is the changed layout"
    );
    h.run_steps(2);
    assert_eq!(order(&h), before - 1, "Send Backward moved it one step down");
    assert!(h.query_by_label("Send Backward").is_none(), "choosing a row closes the menu");
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(order(&h), before, "one Undo restores the order");

    // A locked element can be copied but not deleted.
    h.state_mut().run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
    h.run_steps(2);
    right_click(&mut h, at);
    assert!(h.get_by_label_contains("Delete Element").accesskit_node().is_disabled(), "a locked element isn't deleted");
    assert!(!h.get_by_label_contains("Duplicate").accesskit_node().is_disabled());
    assert!(h.get_by_label("Send Backward").accesskit_node().is_disabled(), "nor moved in the order");

    // On empty page, the selection is cleared and no menu opens.
    let bare = to_screen(page, s, (layout.width - 1.0, layout.height - 1.0));
    assert_eq!(layout.hit(layout.width - 1.0, layout.height - 1.0), None, "the corner is empty page");
    right_click(&mut h, bare);
    assert_eq!(h.state().layout.selected, None);
    assert!(h.query_by_label_contains("Duplicate").is_none(), "no menu on empty page");
}

/// A locked element is selected, but a drag from inside it or from where its corner handle
/// would be doesn't move or resize it, and Delete keeps it and says why (#536).
#[test]
fn a_locked_element_stays_put() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Locked"})).unwrap();
    app.layout.no_snap = true;
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    app.run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
    let title = app.session.project.layouts[li].element(title.id).unwrap().clone();
    app.layout.selected = Some(title.id);
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            let ctx = ui.ctx().clone();
            crate::chrome::shortcuts(app, &ctx);
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    let middle = to_screen(page, s, (title.x + title.w / 2.0, title.y + title.h / 2.0));
    let corner = to_screen(page, s, (title.x + title.w, title.y + title.h)) + vec2(-4.0, -3.0);
    for from in [middle, corner] {
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=3 {
            h.event(Event::PointerMoved(from + vec2(20.0, 15.0) * i as f32));
            h.step();
        }
        h.event(button(from + vec2(60.0, 45.0), false));
        h.step();
        let app = h.state();
        assert_eq!(app.session.project.layouts[li].element(title.id), Some(&title), "dragged from {from:?}");
        assert_eq!(app.layout.selected, Some(title.id), "it stays selected");
    }
    let key = Event::Key { key: Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    h.event(key);
    h.step();
    let app = h.state();
    assert!(app.session.project.layouts[li].element(title.id).is_some(), "Backspace kept it");
    assert_eq!(app.layout.selected, Some(title.id));
    assert!(app.session.notices.iter().any(|n| n.text.contains(super::locked_delete())), "it says why");
}

/// A text element's properties set italic, alignment and letter spacing (#213, point c).
#[test]
fn a_text_element_takes_italic_alignment_and_spacing() {
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::map::layout::{ElementKind, TextAlign};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Type"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    use egui::accesskit::{Action, ActionData, ActionRequest, Role};
    assert!(h.query_by_label("Alignment").is_some());
    // Ticking Italic, and setting the spacing as assistive technology does, store them.
    h.get_by_role_and_label(Role::CheckBox, "Italic").click();
    h.run_steps(2);
    let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Letter spacing").accesskit_node().locate();
    h.event(egui::Event::AccessKitActionRequest(ActionRequest {
        action: Action::SetValue,
        target_tree: tree,
        target_node: node,
        data: Some(ActionData::NumericValue(3.5)),
    }));
    h.run_steps(3);
    let kind = &h.state().session.project.layouts[li].element(title).unwrap().kind;
    assert!(
        matches!(kind, ElementKind::Text { italic: true, align: TextAlign::Left, letter_spacing, .. } if *letter_spacing == 3.5),
        "{kind:?}"
    );
    // The family (#517): Serif picked from the Font list is stored.
    h.get_by_role_and_label(Role::ComboBox, "Font").click();
    h.run_steps(2);
    h.get_by_label("Serif").click();
    h.run_steps(3);
    let kind = &h.state().session.project.layouts[li].element(title).unwrap().kind;
    assert!(matches!(kind, ElementKind::Text { font: rustgis_engine::map::layout::FontFamily::Serif, .. }), "{kind:?}");
    // Said where no serif is installed, and only there.
    let said = h.query_by_label_contains("No serif font is installed").is_some();
    assert_eq!(said, !rustgis_engine::render::fonts::serif_available());
}

/// A text element's "Wrap to the box" (#2346): off as it is added, on when ticked, as one
/// undoable step, and off again.
#[test]
fn a_text_element_wraps_to_its_box_when_ticked() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Notes"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    let wraps =
        |app: &RustGisApp| matches!(app.session.project.layouts[li].element(title).unwrap().kind, ElementKind::Text { wrap: true, .. });
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    assert!(!wraps(h.state()));
    h.get_by_role_and_label(Role::CheckBox, "Wrap to the box").click();
    h.run_steps(3);
    assert!(wraps(h.state()));
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert!(!wraps(h.state()), "one step undoes it");
    h.run_steps(3);
    h.get_by_role_and_label(Role::CheckBox, "Wrap to the box").click();
    h.run_steps(3);
    assert!(wraps(h.state()));
    h.get_by_role_and_label(Role::CheckBox, "Wrap to the box").click();
    h.run_steps(3);
    assert!(!wraps(h.state()));
}

/// A text element's Vertical alignment, Line spacing and Case in the Format pane: shown at their
/// defaults on opening, each change one undo step, the same value picked again none, a spacing
/// past the widest set at the widest.
#[test]
fn a_texts_block_alignment_line_spacing_and_case_are_one_undo_step_each() {
    use egui::accesskit::{Action, ActionData, ActionRequest, Role};
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::map::layout::{ElementKind, TextCase, TextVAlign};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Notes"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    let block = |app: &RustGisApp| match &app.session.project.layouts[li].element(title).unwrap().kind {
        ElementKind::Text { valign, line_spacing, case, .. } => (*valign, *line_spacing, *case),
        other => panic!("{other:?}"),
    };
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    let depth = h.state().session.undo_depth();
    h.run_steps(3);
    // Opening the pane sends nothing, and shows a plain text's defaults.
    assert_eq!(h.state().session.undo_depth(), depth, "opening the pane changed nothing");
    assert_eq!(block(h.state()), (TextVAlign::Top, 1.0, TextCase::None));
    assert_eq!(h.get_by_role_and_label(Role::ComboBox, "Vertical alignment").value().as_deref(), Some("Top"));
    assert_eq!(h.get_by_role_and_label(Role::ComboBox, "Case").value().as_deref(), Some("As typed"));
    assert_eq!(h.get_by_role_and_label(Role::SpinButton, "Line spacing").accesskit_node().numeric_value(), Some(1.0));
    let pick = |h: &mut egui_kittest::Harness<'_, RustGisApp>, list: &str, item: &str| {
        h.get_by_role_and_label(Role::ComboBox, list).click();
        h.run_steps(2);
        h.get_by_label(item).click();
        h.run_steps(3);
    };
    pick(&mut h, "Vertical alignment", "Middle");
    assert_eq!(block(h.state()).0, TextVAlign::Middle);
    assert_eq!(h.state().session.undo_depth(), depth + 1, "one step");
    pick(&mut h, "Vertical alignment", "Middle");
    assert_eq!(h.state().session.undo_depth(), depth + 1, "the same item again is no step");
    pick(&mut h, "Case", "Lower case");
    assert_eq!(block(h.state()).2, TextCase::Lower);
    assert_eq!(h.state().session.undo_depth(), depth + 2);
    // The spacing set as assistive technology does.
    let set_spacing = |h: &mut egui_kittest::Harness<'_, RustGisApp>, v: f64| {
        let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Line spacing").accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_tree: tree,
            target_node: node,
            data: Some(ActionData::NumericValue(v)),
        }));
        h.run_steps(3);
    };
    set_spacing(&mut h, 1.5);
    assert_eq!(block(h.state()), (TextVAlign::Middle, 1.5, TextCase::Lower));
    assert_eq!(h.state().session.undo_depth(), depth + 3);
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(block(h.state()), (TextVAlign::Middle, 1.0, TextCase::Lower), "one step undoes the spacing alone");
    // Past the widest: set at the widest, in one step.
    h.run_steps(3);
    let before = h.state().session.undo_depth();
    set_spacing(&mut h, 5.0);
    assert_eq!(block(h.state()), (TextVAlign::Middle, 3.0, TextCase::Lower));
    assert_eq!(h.state().session.undo_depth(), before + 1, "one step");
}

/// The Format pane tells lines below a text's box from lines past the 200 a text draws, and
/// says both when it loses both, as the command's notice does (#2346).
#[test]
fn the_format_pane_tells_the_box_from_the_200_line_limit() {
    use crate::layoutview::lines_left_out_label;
    use rustgis_engine::render::layout::TextLoss;
    let loss = |left_out, below_box, past_limit| TextLoss {
        id: 1,
        name: "Note".into(),
        left_out,
        at_least: false,
        below_box,
        past_limit,
        off_page: false,
        unit_label: None,
    };
    assert_eq!(lines_left_out_label(&loss(0, false, false)), None);
    assert_eq!(lines_left_out_label(&loss(3, true, false)).as_deref(), Some("3 lines don't fit the box"));
    assert_eq!(lines_left_out_label(&loss(50, false, true)).as_deref(), Some("50 lines aren't drawn: a text draws 200 at most"));
    assert_eq!(lines_left_out_label(&loss(200, true, true)).as_deref(), Some("200 lines don't fit the box or the 200 a text draws"));
}

/// The Format pane says how many lines of the text selected don't fit its box, as the page
/// drawn counts them, and says nothing once they fit (#2346).
#[test]
fn the_format_pane_says_the_lines_left_out_of_a_text() {
    use egui_kittest::kittest::Queryable;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Notes", "template": "blank"})).unwrap();
    let out = app.run("layout.addElement", json!({"kind": "text", "text": "one\ntwo\nthree", "x": 10, "y": 10, "w": 60, "h": 2})).unwrap();
    let id = out["element"].as_u64().unwrap() as u32;
    app.run("ui.pane.format", json!({})).unwrap();
    app.layout.selected = Some(id);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    assert!(h.query_by_label("2 lines don't fit the box").is_some());
    h.state_mut().run("layout.update", json!({"element": id, "h": 40})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("2 lines don't fit the box").is_none(), "they fit");
}

/// A scale bar's details are set in the Format pane through layout.update, each one undo step.
#[test]
fn a_scale_bar_takes_its_details_in_the_format_pane() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::map::layout::{ElementKind, ScaleBarFit, ScaleLabelPosition};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Bar", "template": "map"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let bar = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::ScaleBar { .. })).unwrap().id;
    app.layout.selected = Some(bar);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let set = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(bar).unwrap().kind {
        ElementKind::ScaleBar { label_position, unit_label, fit, .. } => (*label_position, unit_label.clone(), *fit),
        _ => panic!("a scale bar"),
    };
    assert_eq!(set(&h), (ScaleLabelPosition::Above, None, ScaleBarFit::AdjustDivisionValue));
    for label in
        ["Subdivisions", "Divisions before zero", "Decimals", "Text size", "Label position", "Unit label", "Unit label position", "Fit"]
    {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    // Left empty, the field shows what the bar prints: an automatic unit's abbreviation
    // depends on the frame's scale, a fixed unit's is its own.
    let hint = |h: &egui_kittest::Harness<'_, RustGisApp>| h.get_by_label("Unit label").accesskit_node().placeholder().map(str::to_owned);
    assert_eq!(hint(&h).as_deref(), Some("Unit's abbreviation"));
    h.state_mut().run("layout.update", json!({"element": bar, "units": "kilometers"})).unwrap();
    h.run_steps(2);
    assert_eq!(hint(&h).as_deref(), Some("km"));
    h.get_by_label("Label position").click();
    h.run_steps(2);
    h.get_by_label("Below the bar").click();
    h.run_steps(2);
    assert_eq!(set(&h).0, ScaleLabelPosition::Below);
    h.get_by_label("Fit").click();
    h.run_steps(2);
    h.get_by_label("Adjust width").click();
    h.run_steps(2);
    assert_eq!(set(&h).2, ScaleBarFit::AdjustWidth);
    // Decimals as a chart's: automatic at first, fixed when picked, automatic again.
    let decimals = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(bar).unwrap().kind {
        ElementKind::ScaleBar { decimals, .. } => *decimals,
        _ => panic!("a scale bar"),
    };
    assert_eq!(decimals(&h), None);
    h.get_by_label("Decimals").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "2").click();
    h.run_steps(2);
    assert_eq!(decimals(&h), Some(2));
    h.get_by_label("Decimals").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "Automatic").click();
    h.run_steps(2);
    assert_eq!(decimals(&h), None);
    // Typed, the unit label is set once left.
    h.get_by_label("Unit label").focus();
    h.run_steps(2);
    h.get_by_label("Unit label").type_text("Feet");
    h.run_steps(2);
    h.get_by_label("Subdivisions").focus();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some("Feet"));
    // Unticked, no unit label prints; one undo step brings the text back.
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some(""));
    assert!(h.query_by_label("Unit label position").is_none());
    h.state_mut().run("edit.undo", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some("Feet"));
    // Hidden and shown again, the typed label comes back, not the abbreviation; one undo
    // step hides it again.
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some(""));
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some("Feet"));
    h.state_mut().run("edit.undo", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some(""));
    // A show that was undone can be shown again: the kept text is read, not taken.
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some("Feet"));
    h.state_mut().run("edit.undo", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some(""));
    h.state_mut().run("edit.undo", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some("Feet"));
    // Back to the abbreviation, then hidden and shown: the old "Feet" is forgotten.
    h.state_mut().run("layout.update", json!({"layout": li, "element": bar, "unitLabel": null})).unwrap();
    h.run_steps(3);
    assert_eq!(set(&h).1, None);
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some(""));
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1, None);
    // Hidden while it is being typed, the label isn't written back as the field loses its
    // focus; the typed text is the one kept.
    h.get_by_label("Unit label").focus();
    h.run_steps(2);
    h.get_by_label("Unit label").type_text("Yards");
    h.run_steps(2);
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert_eq!(set(&h).1.as_deref(), Some(""));
    assert!(h.state().layout.typing.is_empty());
    h.get_by_role_and_label(Role::CheckBox, "Show unit label").click();
    h.run_steps(3);
    assert!(set(&h).1.is_some_and(|t| t.ends_with("Yards")), "{:?}", set(&h).1);
    // One subdivision is none, as the bar already has: no change, no undo step.
    let subdivisions = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(bar).unwrap().kind {
        ElementKind::ScaleBar { subdivisions, .. } => *subdivisions,
        _ => panic!("a scale bar"),
    };
    let depth = h.state().session.undo_depth();
    for (typed, expected, steps) in [("1", 0, 0), ("3", 3, 1)] {
        h.get_by_label("Subdivisions").focus();
        h.run_steps(2);
        h.get_by_label("Subdivisions").type_text(typed);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(subdivisions(&h), expected, "{typed}");
        assert_eq!(h.state().session.undo_depth(), depth + steps, "{typed}");
    }
    // A unit label being typed when the bar is no longer shown is saved, and an emptied one
    // gives the abbreviation back.
    for (draft, expected) in [("Miles", Some("Miles")), ("", None)] {
        h.state_mut().layout.selected = Some(bar);
        h.run_steps(2);
        h.state_mut().layout.typing.insert((li, bar, "unitLabel"), draft.into());
        h.state_mut().layout.selected = None;
        h.run_steps(3);
        assert!(h.state().layout.typing.is_empty(), "{draft:?}");
        assert_eq!(set(&h).1.as_deref(), expected, "{draft:?}");
    }
}

/// A number dragged in the Format pane and let go is applied (#607): X moves the element by
/// the drag, though the pane lends the saved X each frame (it used to end where it started).
#[test]
fn dragging_x_in_the_format_pane_moves_the_element() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Drag"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    app.layout.selected = Some(title.id);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    // X is the pane's first number, the topmost: Name, X, Y, Width, Height.
    let at = h.query_all_by_role(Role::SpinButton).map(|n| n.rect()).min_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap().center();
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    let undo = h.state().session.undo_depth();
    h.event(Event::PointerMoved(at));
    h.step();
    h.event(button(at, true));
    h.step();
    // Dragged right over several frames (0.5 mm a point), then let go without moving.
    for i in 1..=4 {
        h.event(Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
        h.step();
    }
    h.event(button(at + vec2(40.0, 0.0), false));
    h.run_steps(3);
    let e = h.state().session.project.layouts[li].element(title.id).unwrap().clone();
    assert!(e.x - title.x > 10.0, "moved from {} to {}", title.x, e.x);
    assert_eq!(e.y, title.y, "only X changed");
    // Applied once, on release: one undo step, not one a frame of the drag.
    assert_eq!(h.state().session.undo_depth(), undo + 1, "one undo step");
    assert_eq!(h.state().session.undo_label(), Some("Element Properties"));
}

#[test]
fn dragging_the_angle_in_the_format_pane_turns_the_arrow() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::{ElementKind, line_angle};
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let id = app.run("layout.addElement", json!({"kind": "arrow", "angle": 0, "length": 60})).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.selected = Some(id);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    // Angle is the line's last number, the bottommost.
    let at = h.query_all_by_role(Role::SpinButton).map(|n| n.rect()).max_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap().center();
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    let undo = h.state().session.undo_depth();
    h.event(Event::PointerMoved(at));
    h.step();
    h.event(button(at, true));
    h.step();
    // Dragged right over several frames, then let go without moving: the turn is kept.
    for i in 1..=4 {
        h.event(Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
        h.step();
    }
    h.event(button(at + vec2(40.0, 0.0), false));
    h.run_steps(3);
    let e = h.state().session.project.layouts[li].element(id).unwrap().clone();
    let ElementKind::Line { dir, head, .. } = e.kind else { panic!("{:?}", e.kind) };
    let a = line_angle((e.x, e.y, e.w, e.h), dir, head);
    assert!(a > 10.0, "turned to {a}°");
    assert_eq!(h.state().session.undo_depth(), undo + 1, "one undo step");
}

/// A text size typed in the Format pane is set once, on Enter; Escape brings back the size
/// from before, without a step (#1194).
#[test]
fn a_size_typed_in_the_format_pane_is_one_undo_step_and_escape_brings_it_back() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Sizes"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            // As the app does each frame: Escape noted, then the shortcuts, which take it.
            crate::widgets::note_escape(ui.ctx());
            crate::chrome::shortcuts(app, &ui.ctx().clone());
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let size = |app: &RustGisApp| match &app.session.project.layouts[li].element(title).unwrap().kind {
        ElementKind::Text { size, .. } => *size,
        kind => panic!("{kind:?}"),
    };
    let before = size(h.state());
    let undo = h.state().session.undo_depth();
    let type_size = |h: &mut egui_kittest::Harness<'_, RustGisApp>, text: &str, key: egui::Key, steps: usize| {
        h.get_by_role_and_label(Role::SpinButton, "Size").focus();
        h.run();
        h.event(egui::Event::Text(text.into()));
        h.run();
        assert_eq!(h.state().session.undo_depth(), steps, "nothing is set while typing");
        h.key_press(key);
        h.run();
    };
    type_size(&mut h, "23", egui::Key::Enter, undo);
    assert_eq!(size(h.state()), 23.0);
    assert_eq!(h.state().session.undo_depth(), undo + 1);
    type_size(&mut h, "30", egui::Key::Escape, undo + 1);
    assert_eq!(size(h.state()), 23.0, "Escape brings back the size from before");
    assert_eq!(h.state().session.undo_depth(), undo + 1);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(size(h.state()), before);
}

/// A drag in the picker of a text's colour, in the shared text form, is one undo step (#1194).
#[test]
fn a_colour_drag_in_the_format_pane_is_one_undo_step() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Colour"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let color = |app: &RustGisApp| match &app.session.project.layouts[li].element(title).unwrap().kind {
        ElementKind::Text { color, .. } => *color,
        kind => panic!("{kind:?}"),
    };
    let before = color(h.state());
    let undo = h.state().session.undo_depth();
    let pane_numbers = h.query_all_by_role(Role::SpinButton).count();
    h.get_by_role(Role::ColorWell).click();
    h.run();
    // The picker's first number (red) follows the pane's own.
    let at = h.query_all_by_role(Role::SpinButton).nth(pane_numbers).unwrap().rect().center();
    h.hover_at(at);
    h.drag_at(at);
    h.step();
    for dx in [16.0, 32.0, 48.0] {
        h.hover_at(at + vec2(dx, 0.0));
        h.step();
        assert_ne!(color(h.state()), before, "the page follows the picker");
        assert_eq!(h.state().session.undo_depth(), undo + 1, "one step however many frames");
    }
    h.drop_at(at + vec2(48.0, 0.0));
    h.run();
    assert_eq!(h.state().session.undo_depth(), undo + 1);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(color(h.state()), before);
}

/// A layout's text has the shared text form (#516): the page's dynamic text as well as the
/// map's, each added to the text and saved; the size in the range text graphics have.
#[test]
fn a_layout_text_offers_the_page_s_dynamic_text() {
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Tokens"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let text = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(title).unwrap().kind {
        ElementKind::Text { text, .. } => text.clone(),
        _ => panic!("a text"),
    };
    let before = text(&h);
    for item in ["Page number  {page}", "Map scale  {scale}"] {
        h.get_by_label("Insert Dynamic Text").click();
        h.run_steps(2);
        h.get_by_label(item).click();
        h.run_steps(2);
    }
    assert_eq!(text(&h), format!("{before}{{page}}{{scale}}"));
    // Larger than text graphics drew before, as a layout's text could already be.
    h.state_mut().run("layout.update", json!({"layout": li, "element": title, "size": 999})).unwrap();
    let size = match &h.state().session.project.layouts[li].element(title).unwrap().kind {
        ElementKind::Text { size, .. } => *size,
        _ => panic!("a text"),
    };
    assert_eq!(size, *rustgis_engine::map::graphics::FONT_SIZE.end());
}

/// An overview frame's properties show its extent indicator's colour and width (#213), and
/// its Remove button takes the indicator away.
#[test]
fn an_overview_frame_shows_its_extent_indicator() {
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Overview"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let main = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().id;
    let inset = app.run("layout.addFrame", json!({"x": 200, "y": 150, "w": 60, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;
    app.run("layout.addElement", json!({"kind": "extentIndicator", "frame": inset, "of": main, "width": 1.5})).unwrap();
    app.layout.selected = Some(inset);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    assert!(h.query_by_label("Extent Indicator").is_some(), "the section shows for an overview frame");
    h.get_by_label("Remove Extent Indicator").click();
    h.run_steps(2);
    let kind = &h.state().session.project.layouts[li].element(inset).unwrap().kind;
    assert!(matches!(kind, ElementKind::MapFrame { extent_of: None, .. }), "removed");
    h.run_steps(2);
    assert!(h.query_by_label("Remove Extent Indicator").is_none(), "and its section with it");
}

/// A frame's Border section (#2557): Default Border shows only for a border of its own, and
/// gives the frame the default one back.
#[test]
fn a_frame_border_goes_back_to_the_default() {
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Borders"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let frame = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().id;
    app.layout.selected = Some(frame);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 1400.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    h.get_by_label("Border").click();
    h.run_steps(3);
    assert!(h.query_by_label("Default Border").is_none(), "the default border has nothing to go back to");
    h.state_mut().run("layout.update", json!({"layout": li, "element": frame, "border": {"width": 0}})).unwrap();
    h.run_steps(3);
    h.get_by_label("Default Border").click();
    h.run_steps(2);
    let kind = &h.state().session.project.layouts[li].element(frame).unwrap().kind;
    assert!(!kind.has_custom_border(), "the default border again");
    h.run_steps(2);
    assert!(h.query_by_label("Default Border").is_none(), "and its button gone");
}

/// A legend's properties list the layers of its frame's map; unticking one leaves it out of
/// the legend.
#[test]
fn a_legend_leaves_out_the_layers_unticked() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Legend"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let legend = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::Legend { .. })).unwrap().id;
    // A layer the section doesn't list (graphics here), left out through the command, stays
    // left out whatever is ticked.
    app.session.map_mut().layers.iter_mut().find(|l| l.name == "Cities").unwrap().graphics = true;
    app.run("layout.update", json!({"element": legend, "exclude": ["Cities"]})).unwrap();
    app.layout.selected = Some(legend);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    assert!(h.query_by_role_and_label(Role::CheckBox, "Cities").is_none());
    h.get_by_role_and_label(Role::CheckBox, "Rivers").click();
    h.run_steps(2);
    let left_out = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(legend).unwrap().kind {
        ElementKind::Legend { exclude, .. } => exclude.clone(),
        _ => panic!("a legend"),
    };
    assert_eq!(left_out(&h), ["Cities", "Rivers"]);
    // Ticked again, it's listed again.
    h.get_by_role_and_label(Role::CheckBox, "Rivers").click();
    h.run_steps(2);
    assert_eq!(left_out(&h), ["Cities"]);
}

/// The layout view with the Format pane beside it, as the app draws them: after following
/// what agents ran (`RustGisApp::logic`), and saving drafts at the end.
fn with_format(app: &mut RustGisApp, ui: &mut egui::Ui) {
    super::follow_project(app);
    let t = crate::theme::Tokens::get(ui.ctx());
    if app.ui.right == Some(crate::RightPane::Format) {
        egui::Panel::right("format_pane").exact_size(320.0).show(ui, |ui| super::format(app, ui, &t));
    }
    super::show(app, ui);
    super::save_drafts(app);
}

/// A frame's grid in the Format pane: a graticule's degree rows, or a measured grid's
/// coordinate system (typed, set once left), short labels and MGRS labels.
#[test]
fn a_measured_grid_is_set_in_the_format_pane() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Hike"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let frame = app.session.project.layouts[li].elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap().id;
    app.run("layout.addElement", json!({"kind": "grid"})).unwrap();
    app.run("ui.pane.format", json!({})).unwrap();
    app.layout.selected = Some(frame);
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 1400.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let grid = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(frame).unwrap().kind {
        ElementKind::MapFrame { grid, .. } => grid.clone().unwrap(),
        _ => panic!("not a frame"),
    };
    assert!(h.query_by_label("N/S/E/W").is_some() && h.query_by_label("Coordinate system").is_none(), "a graticule's rows");
    h.state_mut().run("layout.update", json!({"element": frame, "grid": {"type": "measured"}})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("N/S/E/W").is_none(), "no degrees for a measured grid");
    assert!(h.query_by_label("Short labels").is_some());
    // A system typed is set once the field is left; until then, the grid's stays.
    h.get_by_role_and_label(Role::TextInput, "Coordinate system").focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Coordinate system").type_text("EPSG:2056");
    h.run_steps(2);
    assert_eq!(grid(&h).crs, None, "not while typing");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(grid(&h).crs.as_deref(), Some("EPSG:2056"));
    h.get_by_label("Short labels").click();
    h.run_steps(2);
    assert!(grid(&h).short);
    // MGRS labels need a WGS 84 UTM zone: refused over LV95, nothing changed; with the map's
    // or the frame's zone they're set, and short labels go, as MGRS's are short already.
    h.get_by_label("MGRS labels").click();
    h.run_steps(2);
    assert!(!grid(&h).mgrs && grid(&h).crs.as_deref() == Some("EPSG:2056"));
    h.state_mut().run("layout.update", json!({"element": frame, "grid": {"crs": ""}})).unwrap();
    h.run_steps(2);
    h.get_by_label("MGRS labels").click();
    h.run_steps(3);
    assert!(grid(&h).mgrs);
    assert!(h.query_by_label("Short labels").is_none());
}

/// Selecting an element opens the Format pane, a tab like the others (#516). Closing it keeps
/// the selection, and selecting another element brings it back.
#[test]
fn a_selected_element_is_formatted_in_its_pane() {
    use crate::RightPane::{Bookmarks, Format};
    use egui_kittest::kittest::Queryable;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Format"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let ids: Vec<u32> = app.session.project.layouts[li].elements.iter().map(|e| e.id).collect();
    app.run("ui.pane.bookmarks", json!({})).unwrap();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(2);
    assert_eq!(h.state().ui.right, Some(Bookmarks), "nothing selected, nothing opened");
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.run_steps(2);
    assert!(h.query_by_label("Select an element to format it.").is_some(), "opened by hand: nothing to format");
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.state_mut().layout.selected = ids.first().copied();
    h.run_steps(2);
    assert_eq!((h.state().ui.right, h.state().ui.right_open.clone()), (Some(Format), vec![Bookmarks, Format]), "a tab beside the others");
    assert!(h.query_all_by_label("Name").next().is_some(), "the element's properties");
    // Closed: the selection stays, and the pane doesn't come back by itself.
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.run_steps(2);
    assert_eq!((h.state().ui.right, h.state().layout.selected), (Some(Bookmarks), ids.first().copied()));
    // Another element selected: it is back.
    h.state_mut().layout.selected = ids.get(1).copied();
    h.run_steps(2);
    assert_eq!(h.state().ui.right, Some(Format));
    // A name being typed when the pane closes is kept, the element still selected.
    use egui::accesskit::Role;
    let name = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].element(ids[1]).unwrap().name.clone();
    let before = name(&h);
    h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(before.as_str())).unwrap().focus();
    h.run_steps(2);
    h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text("Draft");
    h.run_steps(2);
    assert_eq!(name(&h), before, "still being typed");
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.run_steps(2);
    assert!(name(&h).contains("Draft"), "saved when the pane closed: {}", name(&h));
    assert_eq!(h.state().layout.selected, ids.get(1).copied());
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.run_steps(2);
    // The layout out of sight, under the Model Builder or for the map: nothing to edit.
    let hint = |h: &egui_kittest::Harness<'_, RustGisApp>| h.query_by_label("Select an element to format it.").is_some();
    (h.state_mut().mb.open, h.state_mut().mb.active) = (true, true);
    h.run_steps(2);
    assert!(hint(&h), "under the Model Builder");
    (h.state_mut().mb.open, h.state_mut().mb.active) = (false, false);
    h.run_steps(2);
    assert!(!hint(&h));
    h.state_mut().run("layout.close", json!({})).unwrap();
    h.run_steps(2);
    assert!(hint(&h), "on the map");
    assert_eq!(h.state().ui.right, Some(Format), "the pane stays, as other panes do");
    // Closed, then the element of the same id selected in another layout: that is another
    // element, so the pane opens for it.
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.state_mut().run("layout.new", json!({"name": "Other"})).unwrap();
    let other = h.state().session.active_layout.unwrap();
    assert!(h.state().session.project.layouts[other].element(ids[1]).is_some(), "the id exists there too");
    h.state_mut().layout.selected = ids.get(1).copied();
    h.run_steps(2);
    assert_eq!(h.state().ui.right, Some(Format));
}

/// Picking Automatic or None for a chart's title drops its own title being typed, so the
/// field losing its focus doesn't bring it back (#2487).
#[test]
fn a_charts_title_mode_drops_the_title_being_typed() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Charts"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let p = json!({"layout": li, "kind": "chart", "layer": "Cities", "title": "Mine"});
    let id = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.selected = Some(id);
    let mut h = egui_kittest::Harness::builder().with_size(vec2(420.0, 1600.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            let layout = app.session.project.layouts[li].clone();
            egui::ScrollArea::vertical().show(ui, |ui| super::properties(app, ui, &layout, li));
        },
        app,
    );
    h.run_steps(2);
    h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("Mine")).unwrap().focus();
    h.run_steps(2);
    h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text(" draft");
    h.run_steps(2);
    assert!(!h.state().layout.typing.is_empty(), "being typed");
    h.get_by_role_and_label(Role::Button, "None").click();
    h.run_steps(3);
    let title = match &h.state().session.project.layouts[li].element(id).unwrap().kind {
        ElementKind::Chart { title, .. } => title.clone(),
        k => panic!("{k:?}"),
    };
    assert_eq!(title.as_deref(), Some(""), "no title, not the one typed");
    assert!(h.state().layout.typing.is_empty());
}

/// The Color by class box colours a chart's bars by its layer's classes, and clears it again
/// (#2487 part 3).
#[test]
fn color_by_class_colours_a_charts_bars() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Charts"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let p = json!({"layout": li, "kind": "chart", "layer": "Countries", "field": "pop_est", "group": "continent"});
    let id = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.selected = Some(id);
    let mut h = egui_kittest::Harness::builder().with_size(vec2(420.0, 1600.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            let layout = app.session.project.layouts[li].clone();
            egui::ScrollArea::vertical().show(ui, |ui| super::properties(app, ui, &layout, li));
        },
        app,
    );
    let classes = |h: &egui_kittest::Harness<RustGisApp>| {
        matches!(h.state().session.project.layouts[li].element(id).map(|e| &e.kind), Some(ElementKind::Chart { classes: true, .. }))
    };
    h.run_steps(2);
    assert!(!classes(&h));
    for on in [true, false] {
        h.get_by_role_and_label(Role::CheckBox, "Color by class").click();
        h.run_steps(2);
        assert_eq!(classes(&h), on);
    }
}

/// The Stack by list stacks a grouped chart's bars by another field, and is shown only while
/// the chart groups and sums or counts (#2487 part 3b).
#[test]
fn stack_by_stacks_a_charts_bars() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::map::layout::ElementKind;
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Charts"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let p = json!({"layout": li, "kind": "chart", "layer": "Countries", "field": "pop_est", "group": "continent"});
    let id = app.run("layout.addElement", p).unwrap()["element"].as_u64().unwrap() as u32;
    app.layout.selected = Some(id);
    let mut h = egui_kittest::Harness::builder().with_size(vec2(420.0, 1600.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            let layout = app.session.project.layouts[li].clone();
            egui::ScrollArea::vertical().show(ui, |ui| super::properties(app, ui, &layout, li));
        },
        app,
    );
    let stack = |h: &egui_kittest::Harness<RustGisApp>| match h.state().session.project.layouts[li].element(id).map(|e| &e.kind) {
        Some(ElementKind::Chart { stack, .. }) => stack.clone(),
        _ => None,
    };
    h.run_steps(2);
    assert_eq!(stack(&h), None);
    h.get_by_role_and_label(Role::ComboBox, "Stack by").click();
    h.run_steps(2);
    h.get_by_label("income_grp").click();
    h.run_steps(3);
    assert_eq!(stack(&h).as_deref(), Some("income_grp"));
    // Taking a mean lets the stack go, and the row with it.
    h.state_mut().run("layout.update", json!({"layout": li, "element": id, "statistic": "mean"})).unwrap();
    h.run_steps(2);
    assert_eq!(stack(&h), None);
    assert!(h.query_by_label("Stack by").is_none());
}

/// Capture To Clipboard draws the text still being typed into the Format pane.
#[test]
fn a_capture_keeps_the_text_being_typed() {
    let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
    app.run("layout.new", json!({"name": "Typing"})).unwrap();
    let li = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.typing.insert((li, title, "text"), "Typed".into());
    // The pane drew this field last frame and it still has focus.
    app.layout.drawn = Some((li, title));
    app.run("ui.captureLayout", json!(null)).unwrap();
    let text = match &app.session.project.layouts[li].element(title).unwrap().kind {
        super::ElementKind::Text { text, .. } => text.clone(),
        _ => String::new(),
    };
    assert_eq!(text, "Typed");
}

/// Text being typed for a layout's element stays that layout's when the layouts change places
/// (a move, a removal, a copy, an undo), however that came (the app, an agent through the
/// session itself, a script): it never reaches the element of the same id in the layout now
/// at its old place (#2802).
#[test]
fn a_draft_follows_its_layout_when_the_layouts_change_places() {
    let title_of = |app: &RustGisApp, name: &str| {
        let l = app.session.project.layouts.iter().find(|l| l.name == name).unwrap();
        let e = l.elements.iter().find(|e| e.name == "Title").unwrap();
        match &e.kind {
            super::ElementKind::Text { text, .. } => (e.id, text.clone()),
            _ => (e.id, String::new()),
        }
    };
    let place = |app: &RustGisApp, name: &str| app.session.project.layouts.iter().position(|l| l.name == name).unwrap();
    for by_agent in [false, true] {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        for n in ["A", "B", "C"] {
            app.run("layout.new", json!({"name": n})).unwrap();
        }
        let (title, before) = title_of(&app, "A");
        assert_eq!(title, title_of(&app, "C").0, "the same id in both layouts");
        super::follow_project(&mut app);
        app.layout.typing.insert((place(&app, "A"), title, "text"), "Typed for A".into());
        let to_front = json!({"name": "C", "to": 0});
        if by_agent {
            app.session.run("layout.move", &to_front).unwrap();
        } else {
            app.run("layout.move", to_front).unwrap();
        }
        // The end of the frame: the pane no longer shows the field.
        super::save_drafts(&mut app);
        assert_eq!(title_of(&app, "A").1, "Typed for A", "agent: {by_agent}");
        assert_eq!(title_of(&app, "C").1, before, "agent: {by_agent}");
    }
    // A layout removed with its draft: the draft goes with it.
    let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
    for n in ["A", "B"] {
        app.run("layout.new", json!({"name": n})).unwrap();
    }
    let (title, before) = title_of(&app, "B");
    super::follow_project(&mut app);
    app.layout.typing.insert((place(&app, "A"), title, "text"), "Typed for A".into());
    app.session.run("layout.remove", &json!({"name": "A"})).unwrap();
    super::save_drafts(&mut app);
    assert_eq!(title_of(&app, "B").1, before);
    assert!(app.layout.typing.is_empty());
    // Removed and another made in its stead within one frame (an agent's two commands): as
    // many layouts as before, but the draft's is gone, and so is the draft.
    app.run("layout.new", json!({"name": "C"})).unwrap();
    let (_, before_c) = title_of(&app, "C");
    super::follow_project(&mut app);
    app.layout.typing.insert((place(&app, "B"), title, "text"), "Typed for B".into());
    app.session.run("layout.remove", &json!({"name": "B"})).unwrap();
    app.session.run("layout.new", &json!({"name": "D"})).unwrap();
    super::save_drafts(&mut app);
    assert_eq!((title_of(&app, "C").1, title_of(&app, "D").1), (before_c.clone(), before_c.clone()));
    assert!(app.layout.typing.is_empty());
    // Removed and another made at its place within one frame, under another name or its own:
    // the draft is dropped, never written into the new layout.
    for name in ["F", "E"] {
        app.run("layout.new", json!({"name": "E"})).unwrap();
        super::follow_project(&mut app);
        app.layout.typing.insert((place(&app, "E"), title, "text"), "Typed for E".into());
        app.session.run("layout.remove", &json!({"name": "E"})).unwrap();
        app.session.run("layout.new", &json!({"name": name})).unwrap();
        super::save_drafts(&mut app);
        assert_eq!(title_of(&app, name).1, before_c, "{name}");
        assert!(app.layout.typing.is_empty(), "{name}");
        app.session.run("layout.remove", &json!({"name": name})).unwrap();
    }
    // Renamed, by an agent between frames: the draft stays with it.
    super::follow_project(&mut app);
    app.layout.typing.insert((place(&app, "C"), title, "text"), "Typed for C".into());
    app.session.run("layout.rename", &json!({"name": "C", "newName": "X"})).unwrap();
    super::save_drafts(&mut app);
    assert_eq!(title_of(&app, "X").1, "Typed for C");
    // Renamed, and its old name given to another, in one go: the draft stays with its own
    // layout, not the one now called what it was.
    super::follow_project(&mut app);
    app.layout.typing.insert((place(&app, "D"), title, "text"), "Typed for D".into());
    app.session.run("layout.rename", &json!({"name": "D", "newName": "Q"})).unwrap();
    app.session.run("layout.rename", &json!({"name": "X", "newName": "D"})).unwrap();
    super::save_drafts(&mut app);
    assert_eq!((title_of(&app, "Q").1, title_of(&app, "D").1), ("Typed for D".to_string(), "Typed for C".to_string()));
    // Copied: the draft is the original's, not the copy's.
    super::follow_project(&mut app);
    app.layout.typing.insert((place(&app, "Q"), title, "text"), "Typed again".into());
    app.session.run("layout.duplicate", &json!({"name": "Q", "newName": "Q2"})).unwrap();
    super::save_drafts(&mut app);
    assert_eq!((title_of(&app, "Q").1, title_of(&app, "Q2").1), ("Typed again".to_string(), "Typed for D".to_string()));
}

/// Text being typed when another project opens is for the old one: it never reaches the
/// element of the same id in the new one, whether the app opened it or an agent did through
/// the session itself (the MCP relay, the file bridge).
#[test]
fn a_draft_is_dropped_with_its_project() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    for by_agent in [false, true] {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Drafts"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        let text = serde_json::to_string(&app.session.project).unwrap();
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("Title")).unwrap().focus();
        h.run_steps(2);
        h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text("Draft");
        h.run_steps(2);
        assert!(!h.state().layout.typing.is_empty(), "being typed");
        let open = json!({"text": text});
        if by_agent {
            // As the relay runs an agent's command: on the session, past `RustGisApp::run`;
            // the copy's layout opened in the same go.
            h.state_mut().session.run("project.openText", &open).unwrap();
            h.state_mut().session.run("layout.activate", &json!({"layout": li})).unwrap();
        } else {
            h.state_mut().run("project.openText", open).unwrap();
        }
        h.run_steps(2);
        let app = h.state();
        assert_eq!(app.session.project.layouts[li].element(title).map(|e| e.name.as_str()), Some("Title"), "agent: {by_agent}");
        assert!(app.layout.typing.is_empty() && app.layout.selected.is_none(), "agent: {by_agent}");
    }
}

/// Element ids are per layout: opening another layout ends the selection, so Delete or
/// Ctrl+D never reaches the element of the same id there.
#[test]
fn opening_another_layout_clears_the_selection() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "First"})).unwrap();
    app.run("layout.new", json!({"name": "Second"})).unwrap();
    app.run("layout.activate", json!({"name": "First"})).unwrap();
    let first = app.session.active_layout.unwrap();
    let title = app.session.project.layouts[first].elements.iter().find(|e| e.name == "Title").unwrap().id;
    app.layout.selected = Some(title);
    app.run("layout.removeElement", json!({"element": 999_999})).ok();
    assert_eq!(app.layout.selected, Some(title), "a command in the same layout keeps it");
    app.run("layout.activate", json!({"name": "Second"})).unwrap();
    assert_ne!(app.session.active_layout, Some(first));
    assert!(app.session.project.layouts[app.session.active_layout.unwrap()].element(title).is_some(), "the id exists there too");
    assert_eq!(app.layout.selected, None);
}

#[test]
fn dragged_elements_snap_to_the_nearest_line() {
    let lines = (vec![0.0, 100.0, 150.0], vec![0.0, 50.0]);
    assert_eq!(snap_to(&[98.5, 120.0], &lines.0, 2.0), Some((1.5, 100.0)));
    assert_eq!(snap_to(&[90.0], &lines.0, 2.0), None);
    let r = (10.0, 10.0, 40.0, 20.0);
    // Moved: the right edge (50 + 49) is nearest a line, so the box lands with it on 100.
    let (moved, sx, sy) = landing(r, None, (49.0, 0.5), Some((&lines, 2.0)));
    assert_eq!((moved, sx, sy), ((60.0, 10.5, 40.0, 20.0), Some(100.0), None));
    // Without snapping it lands where the pointer put it.
    assert_eq!(landing(r, None, (49.0, 0.5), None).0, (59.0, 10.5, 40.0, 20.0));
    // Resized from the bottom-right handle: only those edges snap (148.5 → 150, 49 → 50);
    // the top-left stays.
    let (sized, sx, sy) = landing(r, Some(4), (98.5, 19.0), Some((&lines, 2.0)));
    assert_eq!((sized, sx, sy), ((10.0, 10.0, 140.0, 40.0), Some(150.0), Some(50.0)));
}

/// Dragging out of the top ruler adds a horizontal guide; dragging it back onto the ruler
/// removes it.
#[test]
fn guides_come_out_of_the_rulers_and_go_back() {
    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "Guides", "template": "blank"})).unwrap();
    let area = Rc::new(Cell::new(Rect::NOTHING));
    let shown = area.clone();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            shown.set(ui.available_rect_before_wrap());
            super::show(app, ui);
        },
        app,
    );
    h.run_steps(3);
    let li = h.state().session.active_layout.unwrap();
    let layout = h.state().session.project.layouts[li].clone();
    let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
    let drag = |h: &mut egui_kittest::Harness<'_, RustGisApp>, from: Pos2, to: Pos2| {
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=6 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
            h.step();
        }
        h.event(button(to, false));
        h.step();
    };
    let ruler = Pos2::new(page.center().x, area.get().top() + RULER / 2.0);
    let at = to_screen(page, s, (0.0, 40.0));
    drag(&mut h, ruler, Pos2::new(ruler.x, at.y));
    let y = h.state().session.project.layouts[li].guides.y.clone();
    assert_eq!(y.len(), 1, "a guide was added: {y:?}");
    assert!((y[0] - 40.0).abs() < 0.5, "at about 40 mm: {y:?}");
    drag(&mut h, Pos2::new(page.center().x, to_screen(page, s, (0.0, y[0])).y), ruler);
    assert!(h.state().session.project.layouts[li].guides.y.is_empty(), "dropped on the ruler, it's gone");
}

/// Each layout keeps its own zoom and pan of the page. One not opened yet fits the page, and
/// so does a new layout given the name of one renamed, removed or of another project.
#[test]
fn each_layout_keeps_its_page_view() {
    type H<'a> = egui_kittest::Harness<'a, RustGisApp>;
    type View = (f32, egui::Vec2);
    const FIT: View = (0.0, egui::Vec2::ZERO);
    fn run(h: &mut H<'_>, id: &str, params: serde_json::Value) -> View {
        h.state_mut().run(id, params).unwrap();
        h.run_steps(2);
        (h.state().layout.zoom, h.state().layout.pan)
    }
    fn set(h: &mut H<'_>, view: View) {
        let layout = &mut h.state_mut().layout;
        (layout.zoom, layout.pan) = view;
        h.run_steps(2);
    }
    let (a, b): (View, View) = ((2.0, vec2(30.0, -20.0)), (3.0, vec2(-40.0, 10.0)));
    let open = |name: &str| json!({"name": name});

    let mut app = RustGisApp::with_sample();
    app.run("layout.new", json!({"name": "A", "template": "blank"})).unwrap();
    app.run("layout.new", json!({"name": "B", "template": "blank"})).unwrap();
    let text = serde_json::to_string(&app.session.project).unwrap();
    let mut fonts = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
        move |ui, app: &mut RustGisApp| {
            if !fonts {
                crate::theme::install_fonts(ui.ctx(), &[], None);
                fonts = true;
                return;
            }
            with_format(app, ui);
        },
        app,
    );
    h.run_steps(3);

    set(&mut h, b);
    assert_eq!(run(&mut h, "layout.activate", open("A")), FIT, "not opened yet: fits the page");
    set(&mut h, a);
    assert_eq!(run(&mut h, "layout.activate", open("B")), b, "back as it was left");
    assert_eq!(run(&mut h, "layout.activate", open("A")), a);

    // Showing the map and coming back keeps it; a copy starts with its own.
    run(&mut h, "layout.close", json!({}));
    assert_eq!(run(&mut h, "layout.activate", open("A")), a, "closed and opened again");
    assert_eq!(run(&mut h, "layout.duplicate", json!({"name": "A", "newName": "A2"})), FIT, "a copy fits the page");
    assert_eq!(run(&mut h, "layout.activate", open("A")), a, "the copy didn't change it");

    // B's name freed by renaming it: a new B doesn't get its view.
    run(&mut h, "layout.activate", open("B"));
    assert_eq!(run(&mut h, "layout.rename", json!({"name": "B", "newName": "C"})), FIT, "renamed: fits the page");
    run(&mut h, "layout.activate", open("A"));
    assert_eq!(run(&mut h, "layout.new", json!({"name": "B", "template": "blank"})), FIT, "a new layout with a used name");

    // A's name freed by removing it while open.
    run(&mut h, "layout.activate", open("A"));
    set(&mut h, a);
    run(&mut h, "layout.remove", open("A"));
    assert_eq!(run(&mut h, "layout.new", json!({"name": "A", "template": "blank"})), FIT, "a new layout after the removal");

    // Another project with a layout of the same name.
    set(&mut h, a);
    run(&mut h, "project.openText", json!({"text": text}));
    assert_eq!(run(&mut h, "layout.activate", open("A")), FIT, "another project's layout");
}

#[test]
fn widening_a_hairline_keeps_it_thin() {
    let rule = (10.0, 30.0, 100.0, 0.15);
    assert_eq!(resized(rule, 3, 20.0, 5.0), (10.0, 30.0, 120.0, 0.15));
    assert_eq!(resized(rule, 7, -10.0, 0.0), (0.0, 30.0, 110.0, 0.15));
    // Corner and top/bottom handles size the height, at least 2 mm.
    assert_eq!(resized(rule, 4, 0.0, 0.5), (10.0, 30.0, 100.0, 2.0));
    assert_eq!(resized(rule, 5, 0.0, 0.5), (10.0, 30.0, 100.0, 2.0));
}

/// Two numbers kept across a drag (#607) share a key only for the same layout, element and field.
#[test]
fn number_key_is_one_per_layout_element_and_field() {
    assert_ne!(super::number_key(1, 7, "rows"), super::number_key(0, 7, "rows"));
    assert_ne!(super::number_key(1, 7, "rows"), super::number_key(1, 8, "rows"));
    assert_ne!(super::number_key(1, 7, "rows"), super::number_key(1, 7, "columns"));
}
