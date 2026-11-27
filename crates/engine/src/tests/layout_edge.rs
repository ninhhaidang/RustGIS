//! `layout.publication`'s edge style (#3104): the map from one side of the page to the other, on a
//! light or dark paper, with a kicker over the title and the footer under a thin rule.

use super::*;
use rustgis_map::Rgba;
use rustgis_map::layout::{Element, ElementKind, Layout};

const PIAUI: [f64; 4] = [-46.8, -11.6, -38.5, -2.2];
const INK: Rgba = Rgba::rgb(0x2B, 0x26, 0x20);
const SOFT: Rgba = Rgba::rgb(0x5E, 0x55, 0x47);
const LIGHT_INK: Rgba = Rgba::rgb(0xF2, 0xEE, 0xE6);
const LIGHT_SOFT: Rgba = Rgba::rgb(0xCF, 0xC6, 0xB8);

/// An edge sheet of Piauí with every text of the footer, `extra`'s parameters over these.
fn edge(s: &mut Session, extra: Value) -> Result<Value, String> {
    let mut p = json!({"profile": "a4-portrait", "style": "edge", "extent": PIAUI, "title": "PIAUÍ",
        "subtitle": "Vegetation trend, 2001–2025", "sources": ["Boundaries: IBGE 2022"], "note": "Significant cells only."});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    s.run("layout.publication", &p)
}

fn last(s: &Session) -> &Layout {
    s.project.layouts.last().unwrap()
}

fn find<'a>(l: &'a Layout, name: &str) -> Option<&'a Element> {
    l.elements.iter().find(|e| e.name == name)
}

fn named<'a>(l: &'a Layout, name: &str) -> &'a Element {
    find(l, name).unwrap_or_else(|| panic!("no {name}"))
}

fn text_colour(l: &Layout, name: &str) -> Rgba {
    match &named(l, name).kind {
        ElementKind::Text { color, .. } => *color,
        k => panic!("{name}: {k:?}"),
    }
}

/// The map reaches both sides of the page with no border, nothing boxes the page or the footer,
/// and a thin rule parts the map from the footer.
fn assert_edge_to_edge(l: &Layout, what: &str) {
    let frame = named(l, "Map Frame");
    assert_eq!((frame.x, frame.w), (0.0, l.width), "{what}");
    let ElementKind::MapFrame { border_width, .. } = &frame.kind else { panic!() };
    assert_eq!(*border_width, 0.0, "{what}");
    for gone in ["Frame", "Sources Box", "Reference Box"] {
        assert!(find(l, gone).is_none(), "{what}: {gone}");
    }
    let rule = named(l, "Footer Rule");
    assert!(matches!(rule.kind, ElementKind::Line { .. }), "{what}");
    assert!(rule.y >= frame.y + frame.h, "{what}: the rule is under the map");
    for name in ["Sources Heading", "Sources", "Reference Heading", "Reference", "Note", "Author"] {
        assert!(named(l, name).y >= rule.y, "{what}: {name} is under the rule");
    }
    for e in &l.elements {
        assert!(
            e.x >= -1e-6 && e.y >= -1e-6 && e.x + e.w <= l.width + 1e-6 && e.y + e.h <= l.height + 1e-6,
            "{what}: {} leaves the page",
            e.name
        );
    }
}

#[test]
fn an_edge_sheet_runs_its_map_from_side_to_side() {
    let mut s = world();
    let r = edge(&mut s, json!({})).unwrap();
    assert_eq!(r["style"], "edge");
    let l = last(&s);
    assert_edge_to_edge(l, "a4");
    assert_eq!(l.paper, None);
    assert_eq!(text_colour(l, "Title"), INK);
    assert!(find(l, "Kicker").is_none());
}

#[test]
fn an_edge_sheet_fits_every_page_size() {
    for profile in crate::cmd::layout::PUBLICATION_PROFILES.iter().filter(|p| p.sheet) {
        let mut s = world();
        edge(&mut s, json!({"profile": profile.id, "kicker": "Day 22"})).unwrap_or_else(|e| panic!("{}: {e}", profile.id));
        assert_edge_to_edge(last(&s), profile.id);
    }
}

#[test]
fn a_light_paper_keeps_dark_texts_and_a_dark_one_turns_them_light() {
    for (paper, ink, soft) in [("#F4EFE6", INK, SOFT), ("#0B1B2B", LIGHT_INK, LIGHT_SOFT)] {
        let mut s = world();
        edge(&mut s, json!({"paper": paper})).unwrap();
        let l = last(&s);
        assert_eq!(l.paper, Rgba::from_hex(&paper[1..]), "{paper}");
        for name in ["Title", "Sources Heading", "Reference Heading"] {
            assert_eq!(text_colour(l, name), ink, "{paper}: {name}");
        }
        for name in ["Subtitle", "Sources", "Reference", "Note", "Author"] {
            assert_eq!(text_colour(l, name), soft, "{paper}: {name}");
        }
        assert_edge_to_edge(l, paper);
    }
}

#[test]
fn a_null_paper_is_a_white_page() {
    let mut s = world();
    edge(&mut s, json!({"paper": null})).unwrap();
    assert_eq!(last(&s).paper, None);
    assert_eq!(text_colour(last(&s), "Title"), INK);
}

#[test]
fn a_white_paper_is_stored_as_none_as_layout_page_stores_it() {
    let mut s = world();
    edge(&mut s, json!({"paper": "#FFFFFF"})).unwrap();
    assert_eq!(last(&s).paper, None);
}

#[test]
fn a_paper_that_is_no_opaque_colour_is_refused() {
    let mut s = world();
    for paper in [json!("#12"), json!("red"), json!("#0B1B2B80"), json!(5)] {
        let e = edge(&mut s, json!({"paper": paper})).unwrap_err();
        assert!(e.contains("paper"), "{paper}: {e}");
    }
    assert!(s.project.layouts.is_empty());
}

#[test]
fn a_kicker_sits_over_the_title_in_spaced_capitals_of_the_accent() {
    for (extra, accent) in [(json!({}), Rgba::rgb(0xB4, 0x43, 0x2E)), (json!({"accent": "#1F6F8B"}), Rgba::rgb(0x1F, 0x6F, 0x8B))] {
        let mut s = world();
        let mut p = json!({"kicker": "Día 22"});
        if let Some(a) = extra.get("accent") {
            p["accent"] = a.clone();
        }
        edge(&mut s, p).unwrap();
        let l = last(&s);
        let (kicker, title) = (named(l, "Kicker"), named(l, "Title"));
        assert!(kicker.y + kicker.h <= title.y + 1e-6);
        let ElementKind::Text { text, color, letter_spacing, .. } = &kicker.kind else { panic!() };
        assert_eq!(text, "DÍA 22");
        assert_eq!(*color, accent);
        assert!(*letter_spacing > 0.0);
    }
}

#[test]
fn a_long_kicker_wraps_within_the_page() {
    let mut s = world();
    edge(&mut s, json!({"profile": "a5-portrait", "kicker": "thirty day map challenge ".repeat(8)})).unwrap();
    let l = last(&s);
    let (kicker, title) = (named(l, "Kicker"), named(l, "Title"));
    let ElementKind::Text { text, .. } = &kicker.kind else { panic!() };
    assert!(text.contains('\n'), "{text}");
    assert!(kicker.x + kicker.w <= l.width && kicker.y + kicker.h <= title.y + 1e-6);
}

/// A kicker naming the map is kept as typed and wraps as it is drawn, so it follows a rename.
#[test]
fn a_kicker_with_the_map_token_keeps_it_and_wraps_as_drawn() {
    let mut s = world();
    s.run("map.rename", &json!({"name": "a very long location map name ".repeat(5)})).unwrap();
    edge(&mut s, json!({"profile": "a5-portrait", "kicker": "{map}"})).unwrap();
    let l = last(&s);
    let (kicker, title) = (named(l, "Kicker"), named(l, "Title"));
    let ElementKind::Text { text, wrap, .. } = &kicker.kind else { panic!() };
    assert_eq!((text.as_str(), *wrap), ("{map}", true));
    // More than one line's height, and clear of the title.
    assert!(kicker.h > 2.0 * crate::render::layout::line_pitch_mm(9.0 * l.width / 210.0), "{}", kicker.h);
    assert!(kicker.y + kicker.h <= title.y + 1e-6);
}

#[test]
fn a_sheet_with_no_header_starts_its_map_at_the_top_margin() {
    let mut s = world();
    s.run("layout.publication", &json!({"profile": "a4-portrait", "style": "edge", "extent": PIAUI})).unwrap();
    let l = last(&s);
    for gone in ["Kicker", "Title", "Subtitle"] {
        assert!(find(l, gone).is_none(), "{gone}");
    }
    let frame = named(l, "Map Frame");
    assert!(frame.y <= 15.0 + 1e-6, "{}", frame.y);
}

#[test]
fn paper_and_kicker_are_refused_with_another_style_and_on_a_figure() {
    let mut s = world();
    for (key, value) in [("paper", "#0B1B2B"), ("kicker", "Day 22")] {
        for style in ["classic", "banner", "panel", "editorial", "modular", "poster"] {
            let e = s.run("layout.publication", &json!({"extent": PIAUI, "style": style, key: value})).unwrap_err();
            assert!(e.contains(key) && e.contains("\"edge\""), "{style}: {e}");
        }
        for extra in [json!({}), json!({"style": "edge"})] {
            let mut p = json!({"extent": PIAUI, "profile": "figure-1col", key: value});
            if let Some(style) = extra.get("style") {
                p["style"] = style.clone();
            }
            let e = s.run("layout.publication", &p).unwrap_err();
            assert!(e.contains(key), "{p}: {e}");
        }
    }
    assert!(s.project.layouts.is_empty());
}

#[test]
fn a_figure_leaves_the_edge_style_to_its_caption() {
    let mut s = world();
    let r = s.run("layout.publication", &json!({"extent": PIAUI, "profile": "figure-1col", "style": "edge"})).unwrap();
    assert!(r["warnings"].to_string().contains("style \\\"edge\\\""), "{r}");
}

#[test]
fn an_edge_sheet_is_one_undo_step() {
    let mut s = world();
    edge(&mut s, json!({})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.project.layouts.is_empty());
}

#[test]
fn a_duplicated_edge_sheet_keeps_its_paper() {
    let mut s = world();
    edge(&mut s, json!({"paper": "#0B1B2B"})).unwrap();
    s.run("layout.duplicate", &json!({})).unwrap();
    assert_eq!(s.project.layouts.len(), 2);
    assert_eq!(last(&s).paper, Some(Rgba::rgb(0x0B, 0x1B, 0x2B)));
}
