//! Layout files (`.rgislayout`) read with a format header, so an older RustGIS refuses what it
//! can't keep (#2277).

use crate::{Project, Session};
use serde_json::{Value, json};

/// A session with one layout, and that layout as a bare file: no `format`, no `requires`.
fn with_layout() -> (Session, Value) {
    let mut s = Session::new(Project::default());
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    let bare = serde_json::to_value(&s.project.layouts[0]).unwrap();
    (s, bare)
}

/// The bare layout under a header.
fn headed(bare: &Value, format: u64, requires: Value) -> String {
    let mut v = bare.clone();
    v["format"] = json!(format);
    v["requires"] = requires;
    v.to_string()
}

#[test]
fn a_layout_file_needing_an_unknown_capability_is_refused_by_name() {
    let (mut s, bare) = with_layout();
    let text = headed(&bare, 2, json!(["layout-element-hologram"]));
    let err = s.run("layout.import", &json!({"text": text})).unwrap_err();
    assert!(err.contains("layout-element-hologram"), "{err}");
    assert!(err.contains("this layout needs"), "{err}");
    assert_eq!(s.project.layouts.len(), 1, "nothing imported");
}

#[test]
fn a_layout_file_of_a_newer_format_is_refused() {
    let (mut s, bare) = with_layout();
    let err = s.run("layout.import", &json!({"text": headed(&bare, 3, json!([]))})).unwrap_err();
    assert!(err.contains("format 3"), "{err}");
    assert_eq!(s.project.layouts.len(), 1, "nothing imported");
}

#[test]
fn a_layout_file_whose_requires_isnt_a_list_of_names_is_refused() {
    let (mut s, bare) = with_layout();
    for requires in [json!(5), json!(["layout-element-background", 1])] {
        let err = s.run("layout.import", &json!({"text": headed(&bare, 2, requires)})).unwrap_err();
        assert!(err.contains("as something other than names"), "{err}");
    }
    assert_eq!(s.project.layouts.len(), 1, "nothing imported");
}

#[test]
fn legacy_and_headed_layout_files_with_known_capabilities_open() {
    let (mut s, bare) = with_layout();
    // A file as every release so far writes it.
    s.run("layout.import", &json!({"text": bare.to_string()})).unwrap();
    // A header listing what this RustGIS knows.
    let text = headed(&bare, 2, json!(["layout-element-background"]));
    s.run("layout.import", &json!({"text": text})).unwrap();
    assert_eq!(s.project.layouts.len(), 3);
    assert_eq!(s.project.layouts[2].elements.len(), s.project.layouts[0].elements.len());
}

#[test]
fn a_layout_file_read_from_its_path_is_checked_too() {
    let (mut s, bare) = with_layout();
    let path = std::env::temp_dir().join(format!("rustgis-layout-files-{}.rgislayout", std::process::id()));
    std::fs::write(&path, headed(&bare, 2, json!(["layout-element-hologram"]))).unwrap();
    let refused = s.run("layout.import", &json!({"path": path.display().to_string()}));
    std::fs::write(&path, bare.to_string()).unwrap();
    let opened = s.run("layout.import", &json!({"path": path.display().to_string()}));
    let _ = std::fs::remove_file(&path);
    assert!(refused.unwrap_err().contains("layout-element-hologram"));
    opened.unwrap();
    assert_eq!(s.project.layouts.len(), 2);
}
