//! Layout files (`.rgislayout`) written and read with a format header, so an older RustGIS
//! refuses what it can't keep (#2277).

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

/// What `layout.save` writes for the layout at `layout`, as text and as a value.
fn saved_file(s: &mut Session, layout: usize, tag: &str) -> (String, Value) {
    let path = std::env::temp_dir().join(format!("rustgis-layout-save-{}-{tag}.rgislayout", std::process::id()));
    let ran = s.run("layout.save", &json!({"layout": layout, "path": path.display().to_string()}));
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    ran.unwrap();
    let text = text.unwrap();
    let value = serde_json::from_str(&text).unwrap();
    (text, value)
}

/// A saved layout file requires what its elements use, as the fixture that holds only that layout
/// requires: the nine capabilities, two of them at once, and none. Imported into a project without
/// the layout, it is the same layout, and saved again it is the same file.
#[test]
fn a_saved_layout_file_requires_what_its_elements_use_and_reads_back_the_same() {
    for (name, fixture) in [
        ("background", include_str!("../../../../schema/fixtures/layout_element_background.rgis")),
        ("ink", include_str!("../../../../schema/fixtures/layout_element_ink.rgis")),
        ("wrap", include_str!("../../../../schema/fixtures/layout_element_wrap.rgis")),
        ("data", include_str!("../../../../schema/fixtures/layout_chart_data.rgis")),
        ("look", include_str!("../../../../schema/fixtures/layout_chart_look.rgis")),
        ("classes", include_str!("../../../../schema/fixtures/layout_chart_classes.rgis")),
        ("stack", include_str!("../../../../schema/fixtures/layout_chart_stack.rgis")),
        ("border", include_str!("../../../../schema/fixtures/layout_frame_border.rgis")),
        ("legend-text", include_str!("../../../../schema/fixtures/layout_legend_text.rgis")),
        ("scale-grid-text", include_str!("../../../../schema/fixtures/layout_scale_grid_text.rgis")),
        ("plain", include_str!("../../../../schema/fixtures/layout_no_data.rgis")),
    ] {
        let project = Project::from_json(fixture).unwrap();
        // What the fixture was written with, not what this RustGIS works out for it.
        let expected = serde_json::from_str::<Value>(fixture).unwrap().get("requires").cloned();
        let layout = serde_json::to_value(&project.layouts[0]).unwrap();
        let mut s = Session::new(project.clone());
        let (text, file) = saved_file(&mut s, 0, name);
        assert_eq!(file["format"], json!(crate::project::FORMAT), "{name}");
        assert_eq!(file.get("requires").cloned(), expected, "{name}");
        // Read into the same project without its layout, then saved again.
        let mut bare = project;
        bare.layouts.clear();
        let mut s = Session::new(bare);
        s.run("layout.import", &json!({"text": text})).unwrap();
        assert_eq!(serde_json::to_value(&s.project.layouts[0]).unwrap(), layout, "{name}");
        let (again, _) = saved_file(&mut s, 0, &format!("{name}-again"));
        assert_eq!(again, text, "{name}");
    }
}

/// A layout file holds one layout and no commands: what another layout of the project uses, or a
/// saved command, is the project's to require, not the file's.
#[test]
fn a_layout_file_requires_nothing_for_other_layouts_or_saved_commands() {
    let mut document: Value = serde_json::from_str(include_str!("../../../../schema/fixtures/layout_element_background.rgis")).unwrap();
    document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Wrap", "command": "layout.addElement", "params": {"kind": "text", "text": "A", "wrap": true}}]}]);
    let mut s = Session::new(Project::from_json(&document.to_string()).unwrap());
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    let project = serde_json::to_value(&s.project).unwrap();
    assert_eq!(project["requires"], json!(["layout-element-wrap", "layout-element-background"]));
    let (_, background) = saved_file(&mut s, 0, "other-background");
    assert_eq!(background["requires"], json!(["layout-element-background"]));
    let (_, plain) = saved_file(&mut s, 1, "other-plain");
    assert_eq!(plain["format"], json!(crate::project::FORMAT));
    assert!(plain.get("requires").is_none(), "{plain}");
}
