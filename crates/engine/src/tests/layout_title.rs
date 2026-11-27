//! A layout made from New Layout is titled with the map's name as `{map}`, so renaming the map
//! changes the title.

use super::*;
use rustgis_map::layout::{ElementKind, FontFamily, TextAlign};

/// What the layout `li`'s element `name` says as the page draws it: its text with the tokens
/// filled from the map in the layout's first map frame.
fn drawn(s: &Session, li: usize, name: &str) -> String {
    let l = &s.project.layouts[li];
    let ElementKind::Text { text, .. } = &l.elements.iter().find(|e| e.name == name).unwrap().kind else { panic!("{name} isn't text") };
    let ElementKind::MapFrame { map, .. } = &l.element(l.first_frame().unwrap()).unwrap().kind else { panic!() };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).unwrap();
    let cam = rustgis_map::layout::frame_camera(m, rustgis_geom::Coord::new(0.0, 0.0), 1.0e7, 0.0, 0.0);
    let info = crate::render::layout::PageInfo { page: 1, pages: 1, page_name: String::new() };
    crate::render::layout::expand(text, l, &info, Some((m, cam)))
}

/// The text stored in the layout `li`'s element `name`.
fn stored(s: &Session, li: usize, name: &str) -> String {
    match &s.project.layouts[li].elements.iter().find(|e| e.name == name).unwrap().kind {
        ElementKind::Text { text, .. } => text.clone(),
        k => panic!("{name}: {k:?}"),
    }
}

#[test]
fn the_new_layout_title_follows_a_rename_of_the_map() {
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    let before = s.map().name.clone();
    assert_eq!(drawn(&s, 0, "Title"), before);
    s.run("map.rename", &json!({"name": "Location map"})).unwrap();
    assert_eq!(drawn(&s, 0, "Title"), "Location map");
    assert_eq!(stored(&s, 0, "Title"), "{map}");
}

#[test]
fn a_publication_sheet_titled_with_the_map_token_follows_a_rename() {
    let mut s = world();
    s.run("layout.publication", &json!({"profile": "a4", "extent": [-46.8, -11.6, -38.5, -2.2], "title": "{map}"})).unwrap();
    s.run("map.rename", &json!({"name": "Location map"})).unwrap();
    assert_eq!(drawn(&s, 0, "Title"), "Location map");
}

/// The layout `li`'s elements as (name, x, y, w, h), so two layouts compare by their boxes.
fn boxes(s: &Session, li: usize) -> Vec<(String, f64, f64, f64, f64)> {
    s.project.layouts[li].elements.iter().map(|e| (e.name.clone(), e.x, e.y, e.w, e.h)).collect()
}

#[test]
fn a_sheet_titled_with_the_map_token_is_laid_out_as_one_titled_with_the_name() {
    let mut s = world();
    let name = s.map().name.clone();
    let extent = json!([-46.8, -11.6, -38.5, -2.2]);
    s.run("layout.publication", &json!({"profile": "a4", "extent": extent, "title": name})).unwrap();
    s.run("layout.publication", &json!({"profile": "a4", "extent": extent, "title": "{map}"})).unwrap();
    assert_eq!(boxes(&s, 0), boxes(&s, 1));
    assert_eq!(drawn(&s, 0, "Title"), drawn(&s, 1, "Title"));
}

#[test]
fn a_title_typed_over_the_token_stays_as_typed_after_a_rename() {
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    let title = s.project.layouts[0].elements.iter().find(|e| e.name == "Title").unwrap().id;
    s.run("layout.update", &json!({"layout": 0, "element": title, "text": "Flood risk"})).unwrap();
    s.run("map.rename", &json!({"name": "Location map"})).unwrap();
    assert_eq!(drawn(&s, 0, "Title"), "Flood risk");
    assert_eq!(stored(&s, 0, "Title"), "Flood risk");
}

/// The layout `li`'s element `name`.
fn element<'a>(s: &'a Session, li: usize, name: &str) -> &'a rustgis_map::layout::Element {
    s.project.layouts[li].elements.iter().find(|e| e.name == name).unwrap()
}

/// The lines the page draws the layout `li`'s text element `name` in, read from its SVG export.
fn drawn_lines(s: &Session, li: usize, name: &str) -> Vec<String> {
    let mut l = s.project.layouts[li].clone();
    let id = element(s, li, name).id;
    for e in &mut l.elements {
        e.visible = e.id == id;
    }
    let info = crate::render::layout::PageInfo { page: 1, pages: 1, page_name: String::new() };
    let svg = crate::render::svg::layout_svg(&l, &mut s.project.maps.clone(), 96.0, &info).unwrap().0;
    let text = svg.split("<text ").nth(1).unwrap().split("</text>").next().unwrap();
    let after_tag = |t: &str| t.split_once('>').unwrap().1.to_string();
    if text.contains("<tspan") {
        text.split("<tspan ").skip(1).map(|t| after_tag(t.split("</tspan>").next().unwrap())).collect()
    } else {
        vec![after_tag(text)]
    }
}

/// How many lines the box of the text element `name` was sized for, `pad` mm of it not text.
fn sized_lines(s: &Session, li: usize, name: &str, pad: f64) -> usize {
    let e = element(s, li, name);
    let ElementKind::Text { size, .. } = &e.kind else { panic!("{name} isn't text") };
    let lines = (e.h - pad) / crate::render::layout::line_pitch_mm(*size);
    assert!((lines - lines.round()).abs() < 1e-6, "{name}: {lines} lines");
    lines.round() as usize
}

fn wraps(s: &Session, li: usize, name: &str) -> bool {
    matches!(element(s, li, name).kind, ElementKind::Text { wrap: true, .. })
}

const LONG: &str = "A very long map name that goes on and on past the width of the title box on a sheet of paper";

/// A name wider than the title's box can't be a `{map}` broken into lines in the stored text: the
/// title wraps to its box as it is drawn, in a box sized for those lines.
#[test]
fn a_map_name_wider_than_the_title_wraps_as_drawn_and_follows_a_rename() {
    let mut s = world();
    s.run("map.rename", &json!({"name": LONG})).unwrap();
    let extent = json!([-46.8, -11.6, -38.5, -2.2]);
    for (li, style) in ["classic", "banner", "panel", "editorial", "modular", "poster", "edge"].into_iter().enumerate() {
        let r =
            s.run("layout.publication", &json!({"profile": "a4-portrait", "extent": extent, "title": "{map}", "style": style})).unwrap();
        assert_eq!(stored(&s, li, "Title"), "{map}", "{style}");
        assert!(wraps(&s, li, "Title"), "{style}");
        let lines = drawn_lines(&s, li, "Title");
        assert!(lines.len() > 1, "{style}: {lines:?}");
        assert_eq!(lines.join(" "), LONG, "{style}");
        // The classic title's box has 1.5 mm under its text on A4.
        let pad = if style == "classic" { 1.5 } else { 0.0 };
        assert_eq!(sized_lines(&s, li, "Title", pad), lines.len(), "{style}: {lines:?}");
        assert!(!r["warnings"].to_string().contains("{map}"), "{r}");
    }
    s.run("map.rename", &json!({"name": "Location map"})).unwrap();
    assert_eq!(drawn_lines(&s, 0, "Title"), ["Location map"]);
    // A name that fits needs no wrapping.
    s.run("layout.publication", &json!({"profile": "a4-portrait", "extent": extent, "title": "{map}"})).unwrap();
    assert_eq!(stored(&s, 7, "Title"), "{map}");
    assert!(!wraps(&s, 7, "Title"));
}

/// Words whose advance and ink differ by enough to move a line break.
const WORDS: &[&str] = &[
    "AV", "T.", "WAVY", "and", "by", "county", "district", "flood", "fully", "if", "in", "land", "map", "north", "of", "off", "parcels",
    "river", "roads", "survey", "use", "valley", "water", "zoning",
];

/// The `n`th name made of [`WORDS`], the same on every run: 6 to 29 of them, picked by a linear
/// congruential generator seeded with `n`.
fn candidate(n: u64) -> String {
    let mut x = n;
    let mut next = |below: usize| {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (x >> 33) as usize % below
    };
    let words = 6 + next(24);
    (0..words).map(|_| WORDS[next(WORDS.len())]).collect::<Vec<_>>().join(" ")
}

/// The box is sized by the lines drawn, not by the line breaking the sheet uses for other texts,
/// which measures a line's advance (in the serif, its ink). Which names the two count differently,
/// if any, depends on the installed fonts: each style's box is checked on `LONG` and on a name that
/// tells them apart where one turns up, and some style must find one.
#[test]
fn a_wrapped_map_title_is_sized_for_its_drawn_lines() {
    let mut s = world();
    let extent = json!([-46.8, -11.6, -38.5, -2.2]);
    let mut told_apart = false;
    // Each style, and what its title's box holds besides the text (1.5 mm for a classic one on A4).
    for (style, pad) in [("classic", 1.5), ("banner", 0.0), ("panel", 0.0), ("editorial", 0.0)] {
        // The sheet titled `name`: its title's box width, size, face and alignment.
        let titled = |s: &mut Session, name: &str| {
            s.project.layouts.clear();
            s.run("map.rename", &json!({"name": name})).unwrap();
            s.run("layout.publication", &json!({"profile": "a4-portrait", "extent": extent, "title": "{map}", "style": style})).unwrap();
            let e = element(s, 0, "Title");
            let ElementKind::Text { size, bold, font, align, .. } = &e.kind else { panic!("{style}: the title isn't text") };
            (e.w, *size, *bold, *font, *align)
        };
        // The page's lines for `name` in that box, where the sheet's line breaking counts them differently.
        let breaks_differ = |name: &str, (w, size, bold, font, align): (f64, f64, bool, FontFamily, TextAlign)| {
            let by_the_sheet = crate::render::report::wrap_with(name, w, |x| match font {
                FontFamily::Serif => crate::render::report::serif_width_mm(x, size as f32, bold),
                FontFamily::Sans => crate::render::report::width_mm(x, size as f32, bold),
            });
            let by_the_page = crate::render::layout::wrapped_lines(name, w, size, bold, font, align);
            (by_the_page.len() > 1 && by_the_sheet.len() != by_the_page.len()).then_some(by_the_page.len())
        };
        // The lines the sheet's title is drawn in, which its box must be sized for.
        let sized_as_drawn = |s: &Session| {
            assert!(wraps(s, 0, "Title"), "{style}");
            let lines = drawn_lines(s, 0, "Title");
            assert_eq!(sized_lines(s, 0, "Title", pad), lines.len(), "{style}: {lines:?}");
            lines
        };
        // A name is screened against the box `LONG` gets and then checked on its own sheet, so only the
        // likely ones need a sheet.
        let long_box = titled(&mut s, LONG);
        sized_as_drawn(&s);
        told_apart |= (0..20_000).map(candidate).filter(|name| breaks_differ(name, long_box).is_some()).any(|name| {
            let title = titled(&mut s, &name);
            let Some(counted) = breaks_differ(&name, title).filter(|_| wraps(&s, 0, "Title")) else { return false };
            let lines = sized_as_drawn(&s);
            assert_eq!(lines.len(), counted, "{style}: {lines:?}");
            true
        });
    }
    assert!(told_apart, "no style found a name that the sheet and the page break differently in the installed fonts");
}

/// A column of sources is one element: one `{map}` source too wide for it wraps the element, the
/// others keeping their own lines.
#[test]
fn a_map_token_among_several_sources_wraps_with_them() {
    let mut s = world();
    s.run("map.rename", &json!({"name": LONG})).unwrap();
    let extent = json!([-46.8, -11.6, -38.5, -2.2]);
    let sources = ["Parcels of {map}", "Census 2020"];
    s.run("layout.publication", &json!({"profile": "a4-portrait", "extent": extent, "sources": sources})).unwrap();
    assert_eq!(stored(&s, 0, "Sources"), "Parcels of {map}\nCensus 2020");
    assert!(wraps(&s, 0, "Sources"));
    let lines = drawn_lines(&s, 0, "Sources");
    assert_eq!(lines.last().unwrap(), "Census 2020");
    assert_eq!(lines[..lines.len() - 1].join(" "), format!("Parcels of {LONG}"));
    assert_eq!(sized_lines(&s, 0, "Sources", 0.0), lines.len(), "{lines:?}");
}

/// The notices `run` gives about `{map}`.
fn map_notices(s: &mut Session, command: &str, params: Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(command, &params).unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("{map}")).collect()
}

/// The id of the layout `li`'s element `name`.
fn id(s: &Session, li: usize, name: &str) -> u32 {
    element(s, li, name).id
}

#[test]
fn removing_the_last_map_frame_says_how_many_map_texts_draw_empty() {
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4", "template": "map", "name": "Sheet"})).unwrap();
    s.run("layout.addElement", &json!({"kind": "text", "text": "Map of {map}"})).unwrap();
    s.run("layout.addFrame", &json!({})).unwrap();
    let frames: Vec<u32> =
        s.project.layouts[0].elements.iter().filter(|e| matches!(e.kind, ElementKind::MapFrame { .. })).map(|e| e.id).collect();
    assert_eq!(frames.len(), 2);
    // A frame is left to name the map: nothing to say.
    assert_eq!(map_notices(&mut s, "layout.removeElement", json!({"element": frames[0]})), Vec::<String>::new());
    assert_eq!(
        map_notices(&mut s, "layout.removeElement", json!({"element": frames[1]})),
        ["Layout 'Sheet': 2 texts with {map} have no map frame showing a map to name, so {map} draws empty"]
    );
    // Said when it happens, not again.
    let title = id(&s, 0, "Title");
    assert_eq!(map_notices(&mut s, "layout.removeElement", json!({"element": title})), Vec::<String>::new());
}

#[test]
fn removing_the_last_map_frame_of_a_layout_without_map_texts_says_nothing() {
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4", "template": "blank"})).unwrap();
    s.run("layout.addElement", &json!({"kind": "text", "text": "Flood risk"})).unwrap();
    let frame = s.run("layout.addFrame", &json!({})).unwrap()["element"].clone();
    assert_eq!(map_notices(&mut s, "layout.removeElement", json!({"element": frame})), Vec::<String>::new());
}

#[test]
fn removing_the_map_a_layout_names_says_its_map_texts_draw_empty() {
    let mut s = world();
    let first = s.map().name.clone();
    s.run("map.new", &json!({"name": "Detail"})).unwrap();
    s.run("map.new", &json!({"name": "Spare"})).unwrap();
    s.run("map.activate", &json!({"name": "Detail"})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "template": "map", "name": "Sheet"})).unwrap();
    // A map no frame names, then the one this layout's frame shows.
    assert_eq!(map_notices(&mut s, "map.remove", json!({"name": "Spare"})), Vec::<String>::new());
    assert_eq!(
        map_notices(&mut s, "map.remove", json!({"name": "Detail"})),
        ["Layout 'Sheet': 1 text with {map} has no map frame showing a map to name, so {map} draws empty"]
    );
    assert_eq!(drawn_lines(&s, 0, "Title"), [""]);
    assert_eq!(s.map().name, first);
}

#[test]
fn removing_a_map_two_layouts_name_says_so_once() {
    let mut s = world();
    s.run("map.new", &json!({"name": "Detail"})).unwrap();
    s.run("map.activate", &json!({"name": "Detail"})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    assert_eq!(
        map_notices(&mut s, "map.remove", json!({"name": "Detail"})),
        ["2 layouts: 2 texts with {map} have no map frame showing a map to name, so {map} draws empty"]
    );
}

#[test]
fn removing_an_offline_area_and_its_map_says_the_map_texts_draw_empty() {
    let mut s = world();
    let first = s.map().name.clone();
    s.run("map.new", &json!({"name": "Crew (offline)"})).unwrap();
    s.run("map.activate", &json!({"name": "Crew (offline)"})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "template": "map", "name": "Sheet"})).unwrap();
    let extent = rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0);
    let area = json!({"name": "Crew", "map": first, "extent": extent, "created": 0.0, "data": s.project.maps[0]});
    s.project.offline_areas.push(serde_json::from_value(area).unwrap());
    assert_eq!(
        map_notices(&mut s, "offline.remove", json!({"name": "Crew"})),
        ["Layout 'Sheet': 1 text with {map} has no map frame showing a map to name, so {map} draws empty"]
    );
}

/// `{map}` names the first map frame's map, and the first is the lowest in the drawing order:
/// sending another frame to the back, or deleting the first, changes the map it names, and says
/// so. Moves that leave the first frame's map as it was say nothing.
#[test]
fn reordering_or_removing_frames_says_when_map_texts_name_another_map() {
    let mut s = world();
    let first = s.map().name.clone();
    s.run("map.new", &json!({"name": "Detail"})).unwrap();
    s.run("map.activate", &json!({"name": &first})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "template": "map", "name": "Sheet"})).unwrap();
    let detail =
        s.run("layout.addFrame", &json!({"map": "Detail", "x": 10, "y": 10, "w": 40, "h": 30})).unwrap()["element"].as_u64().unwrap();
    let title = id(&s, 0, "Title");
    // Neither changes the first frame.
    assert_eq!(map_notices(&mut s, "layout.arrange", json!({"element": title, "to": "front"})), Vec::<String>::new());
    assert_eq!(map_notices(&mut s, "layout.arrange", json!({"element": detail, "to": "front"})), Vec::<String>::new());
    assert_eq!(
        map_notices(&mut s, "layout.arrange", json!({"element": detail, "to": "back"})),
        ["Layout 'Sheet': its first map frame, the lowest in the drawing order, now shows 'Detail', so 1 text with {map} names it"]
    );
    // Undone, the title names the first map again; the next change is said again.
    s.run("edit.undo", &json!({})).unwrap();
    let world_frame = s.project.layouts[0].first_frame().unwrap();
    assert_eq!(
        map_notices(&mut s, "layout.removeElement", json!({"element": world_frame})),
        ["Layout 'Sheet': its first map frame, the lowest in the drawing order, now shows 'Detail', so 1 text with {map} names it"]
    );
}
