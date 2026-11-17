//! GeoLibre JSON projects, read without I/O. Unsupported state is reported per layer.
//!
//! The engine resolves the sources after parsing, so malformed JSON cannot leave a new map.
//! Lists and camera/style values are bounded before reaching the map model.

pub mod style;

pub use crate::qgis::{NoteKind, Report};
use crate::{Bookmark, Camera};
use rustgis_geom::{Coord, lonlat_to_merc};
use serde_json::Value;

/// Maximum JSON text accepted from any input channel.
pub const MAX_TEXT: usize = 64 << 20;
/// Maximum layers and groups inspected in a project.
pub const MAX_ITEMS: usize = 1000;

/// A parsed project; layer records retain their source-specific settings for the engine.
pub struct Project {
    pub name: String,
    pub camera: Camera,
    pub layers: Vec<Value>,
    pub groups: Vec<Value>,
    pub bookmarks: Vec<Bookmark>,
    pub basemap: String,
    pub basemap_visible: bool,
    pub basemap_opacity: f32,
}

/// Reads a project with GeoLibre's required fields and lenient optional values.
pub fn read_project(text: &str, report: &mut Report) -> Result<Project, String> {
    if text.len() > MAX_TEXT {
        return Err("GeoLibre project exceeds 64 MiB".into());
    }
    let mut v: Value =
        serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).map_err(|e| format!("not a GeoLibre project: {e}"))?;
    let name = string(&v, "name").to_string();
    let version = string(&v, "version").to_string();
    if name.is_empty() || version.is_empty() || !v.get("mapView").is_some_and(Value::is_object) {
        return Err("GeoLibre project requires name, version and mapView".into());
    }
    if !matches!(version.as_str(), "0.1.0" | "0.2.0") {
        report.approximated("", format!("unrecognised project version {version}; reading supported fields"));
    }
    for key in ["layers", "layerGroups", "bookmarks"] {
        if v.get(key).is_some_and(|value| !value.is_null() && !value.is_array()) {
            return Err(format!("GeoLibre {key} must be an array"));
        }
    }
    let layers = array(&v, "layers");
    if layers.len() > MAX_ITEMS {
        return Err("GeoLibre project has more than 1000 layers".into());
    }
    if array(&v, "layerGroups").len() > MAX_ITEMS {
        return Err("GeoLibre project has more than 1000 groups".into());
    }
    let mut ids = std::collections::HashSet::new();
    for layer in layers {
        let id = string(layer, "id");
        if !id.is_empty() && !ids.insert(id) {
            return Err(format!("duplicate GeoLibre layer id {id}"));
        }
    }
    let mut layers = match v.get_mut("layers").map(Value::take) {
        Some(Value::Array(layers)) => layers,
        _ => Vec::new(),
    };
    for layer in &mut layers {
        // Top-level styles override only the fields they explicitly supply.
        if let Some(top) = (!string(layer, "id").is_empty())
            .then(|| v.get("styles").and_then(|s| s.get(string(layer, "id"))).and_then(Value::as_object))
            .flatten()
        {
            let mut style = layer.get("style").and_then(Value::as_object).cloned().unwrap_or_default();
            style.extend(top.clone());
            if let Some(obj) = layer.as_object_mut() {
                obj.insert("style".into(), Value::Object(style));
            }
        }
    }
    // GeoLibre's map controller treats the last record as topmost. RustGIS stores
    // the top layer first. beforeId anchors to a native MapLibre style-layer id,
    // not necessarily another GeoLibre layer; there is no matching anchor here.
    for layer in &layers {
        if !string(layer, "beforeId").is_empty() {
            report.approximated(string(layer, "name"), "MapLibre beforeId anchor is not retained; using the project's layer order");
        }
    }
    layers.reverse();
    for key in ["plugins", "storymap", "widgets", "comments", "models", "processingHistory", "printLayout", "obia", "secondaryMapViews"] {
        if v.get(key).is_some_and(|x| !x.is_null() && x != &serde_json::json!([]) && x != &serde_json::json!({})) {
            report.skipped("", format!("project {key}"));
        }
    }
    let renderer = string(&v, "primaryRenderer");
    if !renderer.is_empty() && renderer != "maplibre" {
        report.approximated("", format!("{renderer} renderer; supported layers drawn on the RustGIS map"));
    }
    if array(&v, "bookmarks").len() > MAX_ITEMS {
        report.skipped("", "bookmarks past the first 1000");
    }
    let groups = array(&v, "layerGroups")
        .iter()
        .filter(|group| {
            if group.is_object() {
                true
            } else {
                report.skipped("", "invalid group record");
                false
            }
        })
        .cloned()
        .collect();
    let bookmarks = array(&v, "bookmarks").iter().take(MAX_ITEMS).filter(|bookmark| {
        if bookmark.is_object() { true } else { report.skipped("", "invalid bookmark record"); false }
    }).map(|b| {
        let view = serde_json::json!({"center": [number(b,"lng",0.0), number(b,"lat",0.0)], "zoom": b.get("zoom"), "bearing": b.get("bearing"), "pitch": b.get("pitch")});
        Bookmark::new(string(b,"name"), camera(&view))
    }).collect();
    Ok(Project {
        name,
        camera: camera(v.get("mapView").unwrap_or(&Value::Null)),
        layers,
        groups,
        bookmarks,
        basemap: v.get("basemapStyleUrl").and_then(Value::as_str).unwrap_or("https://tiles.openfreemap.org/styles/liberty").to_string(),
        basemap_visible: boolean(&v, "basemapVisible", true),
        basemap_opacity: number(&v, "basemapOpacity", 1.0).clamp(0.0, 1.0) as f32,
    })
}

/// A string setting, empty for a malformed or missing value.
pub fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
/// A finite numeric setting, falling back when malformed.
pub fn number(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(default)
}
/// A boolean setting with its format default.
pub fn boolean(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}
/// An array setting, empty when missing or malformed.
pub fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}
/// MapLibre uses a 512-pixel world at zoom zero; RustGIS uses metres per logical pixel.
pub fn resolution(zoom: f64) -> f64 {
    (std::f64::consts::TAU * rustgis_geom::MERCATOR_R / (512.0 * 2f64.powf(zoom.clamp(0.0, 24.0))))
        .clamp(crate::camera::MIN_RES, crate::camera::MAX_RES)
}
/// Clamps a saved view to finite, drawable Mercator coordinates.
fn camera(v: &Value) -> Camera {
    let c = array(v, "center");
    let x = c.first().and_then(Value::as_f64).unwrap_or(0.0).clamp(-180.0, 180.0);
    let y = c.get(1).and_then(Value::as_f64).unwrap_or(0.0).clamp(-85.05112878, 85.05112878);
    let mut cam = Camera::new(lonlat_to_merc(Coord::new(x, y)), resolution(number(v, "zoom", 2.0)));
    cam.bearing = number(v, "bearing", 0.0).rem_euclid(360.0);
    cam.pitch = number(v, "pitch", 0.0).clamp(0.0, 85.0);
    cam
}

/// Recognises the current and legacy file names, without claiming unrelated JSON files.
pub fn is_project_file(name: &str) -> bool {
    let name = if crate::tiles::is_web(name) { name.split(['?', '#']).next().unwrap_or(name) } else { name };
    let name = name.to_ascii_lowercase();
    name.ends_with(".geolibre") || name.ends_with(".geolibre.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn geolibre_utf8_bom_is_accepted() {
        let text = "\u{feff}".to_string() + &json!({"version":"0.2.0","name":"BOM","mapView":{},"layers":[]}).to_string();
        assert_eq!(read_project(&text, &mut Report::default()).unwrap().name, "BOM");
    }

    #[test]
    fn geolibre_non_object_groups_and_bookmarks_are_reported_and_skipped() {
        let text = json!({"version":"0.2.0","name":"Malformed records","mapView":{},"layers":[],"layerGroups":[null,1,{"id":"g","name":"Valid group"}],"bookmarks":[false,"bad",{"name":"Valid bookmark","lng":1,"lat":2}]}).to_string();
        let mut report = Report::default();
        let project = read_project(&text, &mut report).unwrap();
        assert_eq!(project.groups.len(), 1);
        assert_eq!(project.bookmarks.len(), 1);
        assert!(report.notes.iter().any(|note| note.text == "invalid group record"));
        assert!(report.notes.iter().any(|note| note.text == "invalid bookmark record"));
    }

    #[test]
    fn geolibre_camera_and_top_level_style_override() {
        let text=json!({"version":"0.2.0","name":"Test","mapView":{"center":[-90,35],"zoom":4,"bearing":30,"pitch":20},"layers":[{"id":"a","style":{"circleRadius":6,"fillColor":"#000000"}}],"styles":{"a":{"fillColor":"#ff0000"}},"basemapStyleUrl":""}).to_string();
        let p = read_project(&text, &mut Report::default()).unwrap();
        assert_eq!(p.camera.res, resolution(4.0));
        assert_eq!(p.camera.bearing, 30.0);
        assert_eq!(p.camera.pitch, 20.0);
        assert_eq!(p.layers[0]["style"]["circleRadius"], 6);
        assert_eq!(p.layers[0]["style"]["fillColor"], "#ff0000");
    }

    #[test]
    fn geolibre_new_version_and_hostile_optional_values() {
        let mut report = Report::default();
        let p=read_project(r#"{"version":"99.0.0","name":"Future","mapView":{"center":[1e300,-1e300],"zoom":-1e300,"pitch":1e300},"layers":[null,7,{}],"plugins":{"x":true}}"#,&mut report).unwrap();
        assert!(p.camera.res.is_finite());
        assert_eq!(p.camera.pitch, 85.0);
        assert_eq!(report.notes.len(), 2);
        for text in ["null", "[]", "{}", "{", r#"{"name":"x","version":"0.2.0","mapView":[]}"#] {
            assert!(read_project(text, &mut Report::default()).is_err());
        }
    }

    #[test]
    fn geolibre_bottom_to_top_order_and_bounded_lists() {
        let mut v = json!({"version":"0.1.0","name":"Order","mapView":{},"layers":[{"id":"a"},{"id":"b","beforeId":"a"}]});
        let p = read_project(&v.to_string(), &mut Report::default()).unwrap();
        assert_eq!(string(&p.layers[0], "id"), "b");
        v["layers"] = json!(vec![json!({}); MAX_ITEMS + 1]);
        assert!(read_project(&v.to_string(), &mut Report::default()).is_err());
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;
    use serde_json::json;

    fn layer() -> crate::Layer {
        let (ds,_)=rustgis_data::io::read_bytes("points.geojson",br#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"value":50,"kind":"a"}}]}"#).unwrap();
        crate::Layer::new(1, ds)
    }

    #[test]
    fn geolibre_style_modes_report_their_differences() {
        for mode in ["single", "categorized", "graduated", "proportional", "rule-based", "expression"] {
            let mut l = layer();
            let mut report = Report::default();
            let st = json!({"visible":false,"opacity":0.25,"style":{"vectorStyleMode":if mode=="proportional" {"single"} else {mode},"vectorStyleProperty":"value","vectorStyleStops":[{"value":0,"color":"#00ff00"},{"value":100,"color":"#ff0000"}],"proportionalSizeEnabled":mode=="proportional","proportionalSizeProperty":"value","circleRadius":1e300,"strokeWidth":-2,"labels":{"enabled":true,"field":"kind","size":1e300,"minZoom":3}}});
            style::apply(&mut l, &st, &mut report);
            assert!(!l.visible);
            assert_eq!(l.transparency, 75.0);
            assert!(l.labels.enabled);
            assert!(l.labels.default.size <= 128.0);
            assert!(l.renderer.base_symbol().size <= 256.0);
            if matches!(mode, "rule-based" | "expression" | "proportional") {
                assert!(!report.notes.is_empty(), "{mode}");
            }
            match mode {
                "categorized" => assert!(matches!(l.renderer, crate::Renderer::Unique { .. })),
                "graduated" => assert!(matches!(l.renderer, crate::Renderer::Graduated { .. })),
                "proportional" => assert!(matches!(l.renderer, crate::Renderer::Proportional { .. })),
                _ => assert!(matches!(l.renderer, crate::Renderer::Single(_))),
            }
        }
    }

    /// Colour classes with a proportional size (the hosted earthquakes demo: graduated by depth,
    /// sized by magnitude) keep both, sized by radius as GeoLibre draws them, without a note; a
    /// size field that isn't there, or an empty range, keeps the colours with a note.
    #[test]
    fn geolibre_colour_classes_keep_their_proportional_size() {
        let style = |field: &str, max: f64| json!({"style":{"vectorStyleMode":"graduated","vectorStyleProperty":"value","vectorStyleStops":[{"value":0,"color":"#00ff00"},{"value":40,"color":"#ff0000"}],"proportionalSizeEnabled":true,"proportionalSizeProperty":field,"proportionalSizeMinValue":0,"proportionalSizeMaxValue":max,"proportionalSizeMinRadius":2,"proportionalSizeMaxRadius":18}});
        let (mut l, mut report) = (layer(), Report::default());
        style::apply(&mut l, &style("value", 100.0), &mut report);
        assert!(report.notes.is_empty(), "{:?}", report.notes);
        assert!(matches!(l.renderer, crate::Renderer::Graduated { .. }));
        let by = l.size_by.clone().unwrap();
        assert_eq!((by.field.as_str(), by.min, by.max, by.min_size, by.max_size), ("value", 0.0, 100.0, 4.0, 36.0));
        assert_eq!(by.scaling, crate::symbol_by::SizeScaling::Radius);
        // 50 of 0–100: halfway in radius, the red class's colour.
        let sym = l.styler(None).symbol(0);
        assert_eq!((sym.size, sym.fill.0[..3].to_vec()), (20.0, vec![0xff, 0, 0]));
        for (field, max) in [("gone", 100.0), ("value", 0.0)] {
            let (mut l, mut report) = (layer(), Report::default());
            style::apply(&mut l, &style(field, max), &mut report);
            assert!(l.size_by.is_none() && matches!(l.renderer, crate::Renderer::Graduated { .. }));
            assert!(report.notes.iter().any(|n| n.text.contains("fixed size")), "{field}");
        }
        // A single symbol sized by a text field keeps its fixed size, and says so.
        let (mut l, mut report) = (layer(), Report::default());
        let mut text = style("kind", 100.0);
        text["style"]["vectorStyleMode"] = json!("single");
        style::apply(&mut l, &text, &mut report);
        assert!(!matches!(l.renderer, crate::Renderer::Proportional { .. }));
        assert!(report.notes.iter().any(|n| n.text.contains("not numeric")), "{:?}", report.notes);
        // Both renderer paths report when either radii or value bounds are clamped.
        for mode in ["single", "graduated"] {
            let (mut l, mut report) = (layer(), Report::default());
            let mut oversized = style("value", 1e200);
            oversized["style"]["vectorStyleMode"] = json!(mode);
            oversized["style"]["proportionalSizeMaxRadius"] = json!(200);
            style::apply(&mut l, &oversized, &mut report);
            assert!(report.notes.iter().any(|n| n.text.contains("radii clamped")), "{mode}");
            assert!(report.notes.iter().any(|n| n.text.contains("value bounds clamped")), "{mode}");
        }
    }
}

/// Public GeoLibre share pages expose their JSON at the same path plus `.geolibre.json`.
/// Other hosts and already-direct file addresses are left untouched.
pub fn download_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let tail = url.get(path.len()..).unwrap_or("");
    if let Some(rest) = path.strip_prefix("https://share.geolibre.app/") {
        let parts: Vec<_> = rest.trim_end_matches('/').split('/').collect();
        if parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && !p.contains('.')) {
            return format!("{}.geolibre.json{tail}", path.trim_end_matches('/'));
        }
    }
    url.to_string()
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn geolibre_graduated_lower_bounds_switch_at_equality() {
        let features: Vec<_> = [9.0, 10.0, 10.0001, 20.0, 30.0]
            .into_iter()
            .map(|value| json!({"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"value":value}}))
            .collect();
        let data = json!({"type":"FeatureCollection","features":features});
        let (ds, _) = rustgis_data::io::read_bytes("points.geojson", data.to_string().as_bytes()).unwrap();
        let mut layer = crate::Layer::new(1, ds);
        style::apply(
            &mut layer,
            &json!({"style":{"fillOpacity":1.0,"vectorStyleMode":"graduated","vectorStyleProperty":"value","vectorStyleStops":[{"value":0,"color":"#ff0000"},{"value":10,"color":"#00ff00"},{"value":20,"color":"#0000ff"}]}}),
            &mut Report::default(),
        );
        assert_eq!(layer.symbol(0).fill, crate::Rgba::rgb(255, 0, 0));
        assert_eq!(layer.symbol(1).fill, crate::Rgba::rgb(0, 255, 0));
        assert_eq!(layer.symbol(2).fill, crate::Rgba::rgb(0, 255, 0));
        assert_eq!(layer.symbol(3).fill, crate::Rgba::rgb(0, 0, 255));
        assert_eq!(layer.symbol(4).fill, crate::Rgba::rgb(0, 0, 255));
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    use serde_json::json;

    /// A point with both fields, so scale and class assertions exercise real symbols.
    fn point() -> crate::Layer {
        let (ds, _) = rustgis_data::io::read_bytes("point.geojson", br#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"value":50,"kind":"a"}}]}"#).unwrap();
        crate::Layer::new(1, ds)
    }

    /// Checks both interval endpoints and the surrounding zooms: layer and label scale
    /// denominators must use the same direction despite zoom increasing as scale decreases.
    #[test]
    fn geolibre_layer_and_label_zoom_limits_allow_the_interval() {
        let mut l = point();
        style::apply(
            &mut l,
            &json!({"style":{"minZoom":3,"maxZoom":10,"labels":{"enabled":true,"field":"kind","minZoom":3,"maxZoom":10}}}),
            &mut Report::default(),
        );
        for (zoom, expected) in [(1.0, false), (3.0, true), (5.0, true), (10.0, true), (15.0, false)] {
            let scale = resolution(zoom) / 0.00028;
            assert_eq!(l.in_scale_range(scale), expected, "layer at zoom {zoom}");
            assert_eq!(l.labels.default.in_range(scale, 0.0), expected, "labels at zoom {zoom}");
        }
    }

    /// A translucent base makes a second alpha multiplication observable for both missing
    /// and unsupported stop colours, as well as an explicit matching translucent stop.
    #[test]
    fn geolibre_class_fallback_applies_fill_opacity_once() {
        for mode in ["categorized", "graduated"] {
            for color in [serde_json::Value::Null, json!("rgb(10,20,30)"), json!("#33669980")] {
                let mut l = point();
                style::apply(
                    &mut l,
                    &json!({"style":{"fillColor":"#33669980","fillOpacity":0.6,"vectorStyleMode":mode,"vectorStyleProperty":if mode == "categorized" {"kind"} else {"value"},"vectorStyleStops":[{"value":if mode == "categorized" {json!("a")} else {json!(0)},"color":color},{"value":100,"color":color}]}}),
                    &mut Report::default(),
                );
                assert_eq!(l.renderer.symbol_for(&l.dataset, 0).fill.a(), 77, "{mode}: {color}");
            }
        }
    }

    /// Local punctuation is part of a filename; only HTTP addresses treat it as a query or
    /// fragment. Unrelated JSON must remain eligible for the ordinary data-opening path.
    #[test]
    fn geolibre_file_names_distinguish_local_punctuation_from_url_queries() {
        for name in ["plan#2.geolibre", "plan?2.geolibre.json", "MAP.GEOLIBRE", "https://example.org/map.geolibre?download=1#top"] {
            assert!(is_project_file(name), "{name}");
        }
        assert!(!is_project_file("plan.geolibre#2.json"));
        assert!(!is_project_file("plain.json"));
        assert_eq!(download_url("https://share.geolibre.app/user/map"), "https://share.geolibre.app/user/map.geolibre.json");
    }
}
