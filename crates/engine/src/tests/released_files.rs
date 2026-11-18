//! Compatibility with files written by published releases, never by the code under test.

use crate::{Project, Session};
use rustgis_map::{Marker, Projection, Renderer, Rgba};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

/// Releases with no file of a kind ("project" or "prefs"). Only an entry here excuses a missing
/// file; the README's note on the skip is for readers, and the test doesn't read it.
const SKIPS: &[(&str, &str)] = &[];

fn present_or_skipped(fixtures: &Path, version: &str, kind: &str, file: &str) -> bool {
    let present = fixtures.join(file).exists();
    assert!(present || SKIPS.contains(&(version, kind)), "{version}: {file} is missing and no skip is recorded");
    present
}

#[test]
fn released_projects_and_preferences_still_open() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/releases");
    let files = std::fs::read_dir(&fixtures)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let version = name.strip_suffix(".rgis").or_else(|| name.strip_suffix(".prefs.json"))?;
            version.starts_with('v').then(|| version.to_owned())
        })
        .collect::<BTreeSet<_>>();
    // The README's table lists every release captured. Each row is checked below, so a release whose
    // files were deleted fails unless both kinds are in SKIPS; a file needs its row.
    let versions = std::fs::read_to_string(fixtures.join("README.md"))
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("| v").and_then(|rest| rest.split_once(' ')).map(|(v, _)| format!("v{v}")))
        .collect::<BTreeSet<_>>();
    assert!(!versions.is_empty(), "no releases in the table in {}", fixtures.join("README.md").display());
    let unrecorded = files.difference(&versions).collect::<Vec<_>>();
    assert!(unrecorded.is_empty(), "release fixtures without a README row: {unrecorded:?}");
    for version in versions.iter().map(String::as_str) {
        if present_or_skipped(&fixtures, version, "prefs", &format!("{version}.prefs.json")) {
            let prefs = super::read_at(&fixtures.join(format!("{version}.prefs.json"))).unwrap();
            assert_eq!(prefs.get("ribbon_minimized"), Some(&json!("true")), "{version}");
            if matches!(version, "v0.2.0" | "v0.3.0") {
                assert_eq!(prefs.len(), 1, "{version}");
            } else {
                assert_eq!(prefs.len(), 2, "{version}");
                let connections = crate::connections::Connections::from_json(prefs["connections"].as_str().unwrap());
                assert!(connections.connections.is_empty() && connections.unread().is_empty(), "{version}");
            }
        }

        if !present_or_skipped(&fixtures, version, "project", &format!("{version}.rgis")) {
            continue;
        }
        let path = fixtures.join(format!("{version}.rgis"));
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // v0.7.0 was the first release to write format 2.
        let format = if matches!(version, "v0.2.0" | "v0.3.0" | "v0.4.0" | "v0.5.0" | "v0.6.0") { 1 } else { 2 };
        assert_eq!(written.get("format"), Some(&json!(format)), "{version}");

        let mut s = Session::new(Project::default());
        s.run("project.open", &json!({"path": path})).unwrap();
        // These inline datasets need no repair. An unexpected loss notice is a regression too.
        assert_eq!(
            s.notices_since(0).iter().map(|n| (n.text.as_str(), n.error)).collect::<Vec<_>>(),
            [(format!("Opened {version}").as_str(), false)],
            "{version}"
        );
        assert_eq!(s.project.name, version);
        assert_eq!(s.project.maps.len(), 1, "{version}");
        let map = s.map();
        assert_eq!(map.name, "Map");
        assert_eq!(map.projection, Projection::WebMercator);
        let center = map.camera.center_lonlat();
        assert!((center.x - 2.0).abs() < 1e-10 && (center.y - 3.0).abs() < 1e-10, "{version}");
        assert!((map.camera.scale() - 250_000.0).abs() < 1e-6, "{version}");
        // These releases changed where a newly added layer goes; preserve each saved order.
        let order =
            if matches!(version, "v0.2.0" | "v0.3.0" | "v0.4.0") { [(1, "Places"), (2, "Routes")] } else { [(2, "Routes"), (1, "Places")] };
        assert_eq!(map.layers.iter().map(|l| (l.id, l.name.as_str())).collect::<Vec<_>>(), order, "{version}");

        let places = map.layer(1).unwrap();
        assert!(places.visible);
        assert_eq!(places.dataset.name, "Places");
        assert_eq!(
            places.dataset.fields.iter().map(|f| (f.name.as_str(), f.ty)).collect::<Vec<_>>(),
            [("name", rustgis_data::FieldType::Text), ("pop", rustgis_data::FieldType::Integer)],
            "{version}"
        );
        assert_eq!(places.definition_query.as_deref(), Some("pop >= 20"));
        assert_eq!(places.visible_rows(), [false, true]);
        assert_eq!(
            serde_json::to_value(&places.dataset.features).unwrap(),
            json!([
                {"id": 1, "geometry": {"Point": {"x": 1.0, "y": 2.0}}, "attrs": [{"Text": "Alpha"}, {"Int": 10}]},
                {"id": 2, "geometry": {"Point": {"x": 3.0, "y": 4.0}}, "attrs": [{"Text": "Béta"}, {"Int": 20}]}
            ]),
            "{version}"
        );
        let Renderer::Unique { field, classes, other, ramp, .. } = &places.renderer else { panic!("{version}: unique renderer lost") };
        assert_eq!(field, "name");
        assert_eq!(
            classes.iter().map(|(name, symbol)| (name.as_str(), symbol.fill)).collect::<Vec<_>>(),
            [("Alpha", Rgba::rgb(59, 125, 216)), ("Béta", Rgba::rgb(232, 122, 65)),],
            "{version}"
        );
        assert_eq!(other.fill, Rgba::rgb(176, 176, 176));
        for symbol in classes.iter().map(|(_, symbol)| symbol).chain(std::iter::once(other)) {
            assert_eq!(symbol.stroke, Rgba::rgb(255, 255, 255));
            assert_eq!((symbol.stroke_width, symbol.size, symbol.marker), (1.0, 7.0, Marker::Circle));
        }
        assert_eq!(ramp.name(), "Harbour");
        assert!(places.labels.enabled && places.labels.default.bold && places.labels.default.halo);
        assert_eq!(places.labels.default.field, "name");
        assert_eq!(places.labels.default.size, 14.0);
        assert_eq!(places.labels.default.color, Rgba::rgb(18, 52, 86));

        let routes = map.layer(2).unwrap();
        assert!(routes.visible && !routes.labels.enabled);
        assert_eq!(routes.transparency, 25.0);
        assert_eq!(
            serde_json::to_value(&routes.dataset.features).unwrap(),
            json!([{"id": 1, "geometry": {"Line": [[{"x": 1.0, "y": 2.0}, {"x": 3.0, "y": 4.0}]]}, "attrs": [{"Text": "Route"}]}]),
            "{version}"
        );
        let Renderer::Single(symbol) = &routes.renderer else { panic!("{version}: single renderer lost") };
        assert_eq!(symbol.fill, Rgba::rgb(51, 102, 153));
        assert_eq!(symbol.stroke, Rgba::rgb(17, 34, 51));
        assert_eq!((symbol.stroke_width, symbol.size, symbol.marker), (2.5, 0.0, Marker::Circle));
        assert_eq!(routes.dataset.fields[0].name, "name");
        assert_eq!(routes.dataset.fields[0].ty, rustgis_data::FieldType::Text);
    }
}
