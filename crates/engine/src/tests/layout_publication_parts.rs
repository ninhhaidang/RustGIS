//! A publication's grid, scale bar and north arrow left out or set up by `layout.publication`
//! (#3094).

use super::*;
use rustgis_map::layout::{ArrowStyle, Element, ElementKind, GRID_LABEL_PT, Grid, GridKind, ScaleBarStyle, ScaleUnits};

/// An A4 sheet of open sea with `parts` added to the call: with nothing to cover, each surround
/// stays in its home corner.
fn sheet(parts: Value) -> (Session, Value) {
    let mut s = world();
    let r = publish(&mut s, parts).unwrap();
    (s, r)
}

fn publish(s: &mut Session, parts: Value) -> Result<Value, String> {
    let mut p = json!({"profile": "a4-portrait", "extent": [-30.0, -30.0, -20.0, -22.0]});
    p.as_object_mut().unwrap().extend(parts.as_object().unwrap().clone());
    s.run("layout.publication", &p)
}

fn find<'a>(s: &'a Session, name: &str) -> Option<&'a Element> {
    s.project.layouts[0].elements.iter().find(|e| e.name == name)
}

fn grid(s: &Session) -> Option<Grid> {
    match &find(s, "Map Frame").unwrap().kind {
        ElementKind::MapFrame { grid, .. } => grid.clone(),
        k => panic!("{k:?}"),
    }
}

/// Every text of the layout, its lines joined.
fn texts(s: &Session) -> String {
    let text = |e: &Element| match &e.kind {
        ElementKind::Text { text, .. } => Some(text.replace('\n', " ")),
        _ => None,
    };
    s.project.layouts[0].elements.iter().filter_map(text).collect::<Vec<_>>().join(" | ")
}

/// The room from the north arrow to the frame's right edge: the margin, and the grid's label strip.
fn right_room(s: &Session) -> f64 {
    let (frame, arrow) = (find(s, "Map Frame").unwrap(), find(s, "North Arrow").unwrap());
    frame.x + frame.w - (arrow.x + arrow.w)
}

/// The margin of an A4 sheet's surrounds from its frame's edge, in mm.
const MARGIN: f64 = 4.0;

#[test]
fn a_publication_given_empty_parts_is_the_default_one() {
    let elements = |parts: Value| serde_json::to_value(&sheet(parts).0.project.layouts[0].elements).unwrap();
    let default = elements(json!({}));
    assert_eq!(elements(json!({"grid": null, "scaleBar": null, "northArrow": null})), default);
    assert_eq!(elements(json!({"grid": {}, "scaleBar": {}, "northArrow": {}})), default);
    assert_eq!(elements(json!({"grid": {"step": null}, "scaleBar": {"textSize": null}, "northArrow": {"show": null}})), default);
}

#[test]
fn a_publication_without_a_grid_keeps_no_label_strips_and_names_no_graticule() {
    let (s, r) = sheet(json!({"grid": "none"}));
    assert_eq!(grid(&s), None);
    assert_eq!(r["grid"], Value::Null);
    assert!(!texts(&s).contains("Graticule") && texts(&s).contains("Datum:"), "{}", texts(&s));
    assert!((right_room(&s) - MARGIN).abs() < 1e-6, "{}", right_room(&s));
    let (default, _) = sheet(json!({}));
    assert!(right_room(&default) > MARGIN + 1.0);
}

#[test]
fn a_grid_token_typed_by_the_caller_says_a_dash_without_a_graticule() {
    let (s, _) = sheet(json!({"grid": "none", "reference": ["Lines every {grid}"]}));
    assert!(texts(&s).contains("Lines every —"), "{}", texts(&s));
}

#[test]
fn a_publication_grid_takes_its_step_and_the_reference_says_it() {
    let (s, r) = sheet(json!({"grid": {"step": 5}}));
    assert_eq!(grid(&s).unwrap().step, 5.0);
    assert_eq!(r["grid"], json!(5.0));
    assert!(texts(&s).contains("Graticule: 5°"), "{}", texts(&s));
}

#[test]
fn a_publication_measured_grid_picks_its_step_and_is_not_called_a_graticule() {
    let (s, r) = sheet(json!({"grid": {"type": "measured"}}));
    let g = grid(&s).unwrap();
    assert_eq!((g.kind, g.step), (GridKind::Measured, 0.0));
    assert_eq!(r["grid"], Value::Null);
    assert!(!texts(&s).contains("Graticule"), "{}", texts(&s));
}

#[test]
fn a_publication_grid_keeps_label_strips_only_where_it_labels() {
    let strip = right_room(&sheet(json!({})).0) - MARGIN;
    let (unlabelled, _) = sheet(json!({"grid": {"labels": false}}));
    assert!(grid(&unlabelled).is_some());
    assert!((right_room(&unlabelled) - MARGIN).abs() < 1e-6);
    let (bottom, _) = sheet(json!({"grid": {"edges": ["bottom"]}}));
    assert!((right_room(&bottom) - MARGIN).abs() < 1e-6);
    let (frame, bar) = (find(&bottom, "Map Frame").unwrap(), find(&bottom, "Scale").unwrap());
    assert!(frame.y + frame.h - (bar.y + bar.h) > MARGIN + 1.0);
    let (big, _) = sheet(json!({"grid": {"labelSize": 2.0 * GRID_LABEL_PT}}));
    assert!((right_room(&big) - MARGIN - 2.0 * strip).abs() < 1e-6, "{} {strip}", right_room(&big));
}

#[test]
fn a_publication_without_a_scale_bar_leaves_the_other_surrounds_in_place() {
    let (default, _) = sheet(json!({}));
    let (s, r) = sheet(json!({"scaleBar": "none"}));
    for name in ["Scale Box", "Scale Bar", "Scale"] {
        assert!(find(&s, name).is_none(), "{name}");
    }
    for name in ["North Arrow", "Legend Box"] {
        let at = |s: &Session| find(s, name).map(|e| (e.x, e.y, e.w, e.h));
        assert_eq!(at(&s), at(&default), "{name}");
    }
    assert!(!r["warnings"].to_string().contains("scale bar"), "{}", r["warnings"]);
}

#[test]
fn a_publication_scale_bar_takes_its_options_and_its_box_is_sized_for_it() {
    let (s, _) = sheet(json!({"scaleBar": {"units": "miles", "style": "line", "divisions": 3}}));
    let bar = find(&s, "Scale Bar").unwrap();
    assert!(
        matches!(bar.kind, ElementKind::ScaleBar { units: ScaleUnits::Miles, style: ScaleBarStyle::Line, divisions: 3, .. }),
        "{:?}",
        bar.kind
    );
    assert_eq!(find(&s, "Scale Box").unwrap().w, bar.w);
}

#[test]
fn a_publication_scale_bar_given_a_larger_text_gets_the_height_for_it() {
    let (default, _) = sheet(json!({}));
    let (s, r) = sheet(json!({"scaleBar": {"textSize": 14}}));
    let bar = find(&s, "Scale Bar").unwrap();
    assert!(matches!(bar.kind, ElementKind::ScaleBar { text_size, .. } if text_size == 14.0), "{:?}", bar.kind);
    assert!(bar.h > find(&default, "Scale Bar").unwrap().h);
    assert!(!r["warnings"].to_string().contains("scale bar"), "{}", r["warnings"]);
}

#[test]
fn a_whole_globe_publication_has_no_scale_bar_whatever_its_options() {
    let mut s = world();
    s.run("map.projection", &json!({"type": "globe"})).unwrap();
    let r = s.run("layout.publication", &json!({"scaleBar": {"units": "miles"}})).unwrap();
    assert!(find(&s, "Scale Bar").is_none());
    assert!(r["warnings"].to_string().contains("no scale bar: the map shows the whole globe"), "{}", r["warnings"]);
    let r = s.run("layout.publication", &json!({"scaleBar": "none"})).unwrap();
    assert!(!r["warnings"].to_string().contains("no scale bar"), "{}", r["warnings"]);
}

#[test]
fn a_publication_north_arrow_takes_its_style_and_when_to_show() {
    let (s, _) = sheet(json!({"northArrow": {"style": "compass"}}));
    assert!(matches!(find(&s, "North Arrow").unwrap().kind, ElementKind::NorthArrow { style: ArrowStyle::Compass, .. }));
    // A continental span draws none unless asked "always".
    let wide = |arrow: Value| {
        let mut s = world();
        s.run("layout.publication", &json!({"extent": [-60.0, -30.0, -20.0, 0.0], "northArrow": arrow})).unwrap();
        find(&s, "North Arrow").map(|e| e.kind.clone())
    };
    assert_eq!(wide(json!({"style": "star"})), None);
    assert_eq!(wide(json!("auto")), None);
    assert!(wide(json!("always")).is_some());
    let shown = wide(json!({"show": "always", "style": "star"}));
    assert!(matches!(shown, Some(ElementKind::NorthArrow { style: ArrowStyle::Star, .. })), "{shown:?}");
    assert!(find(&sheet(json!({"northArrow": "none"})).0, "North Arrow").is_none());
}

#[test]
fn a_publication_part_given_wrong_is_refused_and_nothing_is_made() {
    for (parts, said) in [
        (json!({"grid": {"style": "wavy"}}), "unknown grid style"),
        (json!({"grid": {"foo": 1}}), "grid takes no \"foo\""),
        (json!({"grid": {"stepp": null}}), "grid takes no \"stepp\""),
        (json!({"grid": {"off": true}}), "grid takes no \"off\""),
        // A graticule draws degrees: it reads no coordinate system.
        // Keys only the other kind of grid reads.
        (json!({"grid": {"crs": "EPSG:32723"}}), "grid takes no \"crs\" for a graticule"),
        (json!({"grid": {"type": "graticule", "short": true}}), "grid takes no \"short\" for a graticule"),
        (json!({"grid": {"type": "measured", "format": "dms"}}), "grid takes no \"format\" for a measured grid"),
        (json!({"grid": {"type": "measured", "hemisphere": false}}), "grid takes no \"hemisphere\" for a measured grid"),
        (json!({"grid": 5}), "grid is \"none\""),
        (json!({"grid": true}), "grid is \"none\""),
        (json!({"scaleBar": {"units": "parsecs"}}), "unknown units"),
        (json!({"scaleBar": {"step": 1}}), "scaleBar takes no \"step\""),
        (json!({"scaleBar": "auto"}), "scaleBar is \"none\""),
        (json!({"northArrow": {"show": "none"}}), "northArrow's show"),
        (json!({"northArrow": {"style": "x"}}), "unknown north arrow style"),
        (json!({"northArrow": {"size": 2}}), "northArrow takes no \"size\""),
        (json!({"northArrow": "yes"}), "northArrow is \"auto\""),
    ] {
        let mut s = world();
        let maps = s.project.maps.len();
        let err = publish(&mut s, parts.clone()).unwrap_err();
        assert!(err.contains(said), "{parts}: {err}");
        assert_eq!((s.project.layouts.len(), s.project.maps.len()), (0, maps), "{parts}");
    }
}

#[test]
fn a_saved_publication_that_sets_a_part_requires_its_capability() {
    let requires = |params: Value| {
        let mut s = world();
        s.project
            .models
            .push(serde_json::from_value(json!({"name": "Sheet", "steps": [{"tool": "layout.publication", "params": params}]})).unwrap());
        serde_json::to_value(&s.project).unwrap()["requires"].to_string().contains("layout-publication-parts")
    };
    for params in [
        json!({"grid": "none"}),
        json!({"grid": {"step": 5}}),
        json!({"scaleBar": "none"}),
        json!({"scaleBar": {"units": "miles"}}),
        json!({"northArrow": {"style": "star"}}),
        // Any object: an older reader refuses one.
        json!({"northArrow": {"show": "auto"}}),
        json!({"northArrow": {}}),
        json!({"northArrow": {"show": null}}),
    ] {
        assert!(requires(params.clone()), "{params}");
    }
    for params in [json!({}), json!({"grid": null, "scaleBar": {}}), json!({"northArrow": "always"}), json!({"northArrow": null})] {
        assert!(!requires(params.clone()), "{params}");
    }
}

#[test]
fn the_publication_parts_fixture_opens_and_keeps_its_requirement() {
    let fixture = crate::Project::from_json(include_str!("../../../../schema/fixtures/layout_publication_parts.rgis")).unwrap();
    assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["layout-publication-parts"]));
}

#[test]
fn a_publication_scale_bar_too_large_for_the_sheet_says_it_does_not_fit() {
    let (_, r) = sheet(json!({"scaleBar": {"textSize": 100}}));
    assert!(r["warnings"].to_string().contains("Scale bar \\\"Scale Bar\\\""), "{}", r["warnings"]);
}

#[test]
fn a_publication_grid_says_what_its_typed_coordinate_system_loses() {
    let mut s = world();
    // UTM 17N on NAD 27, bound to WGS 84 by an NTv2 grid, which isn't applied.
    let crs = r#"BOUNDCRS[SOURCECRS[PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["UTM zone 17N",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",-81,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]],ID["EPSG",16017]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]],ABRIDGEDTRANSFORMATION["unknown to WGS84",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","conus",ID["EPSG",8656]]]]"#;
    let mut said = |grid: Value| {
        let mark = s.notice_mark();
        publish(&mut s, json!({"grid": grid})).unwrap();
        s.notices_since(mark).iter().any(|n| n.text.starts_with("crs: "))
    };
    assert!(said(json!({"type": "measured", "crs": crs})));
    assert!(!said(json!({"type": "measured", "crs": "EPSG:32723"})));
}
