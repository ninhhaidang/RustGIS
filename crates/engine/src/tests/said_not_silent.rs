//! Names cut, keys left out and parameters held or dropped are said, and clean input says
//! nothing (#2639).

use super::*;

/// The notices given since `mark`, as text.
fn said(s: &Session, mark: u64) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).collect()
}

/// Whether any notice since `mark` contains `part`.
fn says(s: &Session, mark: u64, part: &str) -> bool {
    said(s, mark).iter().any(|n| n.contains(part))
}

#[test]
fn duplicated_maps_and_layouts_refuse_long_names_and_say_cuts() {
    let mut s = world();
    let long = "m".repeat(crate::cmd::MAX_NAME);
    // A name past the limit is refused, not cut, and leaves the maps as they were.
    let maps = s.project.maps.len();
    let e = s.run("map.duplicate", &json!({"name": "m".repeat(crate::cmd::MAX_NAME + 1)})).unwrap_err();
    assert!(e.contains("at most 200"), "{e}");
    assert_eq!(s.project.maps.len(), maps);
    // A short name: no notice.
    let mark = s.notice_mark();
    s.run("map.duplicate", &json!({"name": "Short"})).unwrap();
    assert!(!says(&s, mark, "is cut to"), "{:?}", said(&s, mark));
    // A name at the limit, taken: cut to fit its number, and said.
    s.run("map.rename", &json!({"name": long})).unwrap();
    let mark = s.notice_mark();
    let r = s.run("map.duplicate", &json!({"name": long})).unwrap();
    assert_eq!(r["name"], format!("{} 2", "m".repeat(198)));
    assert!(says(&s, mark, "Duplicate Map: the name given is cut to 198 characters to fit \" 2\""), "{:?}", said(&s, mark));
    // The default "<map> copy" of a map named at the limit cuts the map's name, and says so.
    let mark = s.notice_mark();
    let r = s.run("map.duplicate", &json!({"map": long})).unwrap();
    assert_eq!(r["name"], format!("{} copy", "m".repeat(195)));
    assert!(says(&s, mark, "Duplicate Map: the copy's name is cut to 195 characters to fit \" copy\""), "{:?}", said(&s, mark));
    // A rename to a taken name at the limit is numbered and says the cut; a short one doesn't.
    let mark = s.notice_mark();
    let r = s.run("map.rename", &json!({"name": long})).unwrap();
    assert_eq!(r["name"], format!("{} 3", "m".repeat(198)));
    assert!(says(&s, mark, "Rename Map: the name given is cut to 198 characters"), "{:?}", said(&s, mark));
    let mark = s.notice_mark();
    s.run("map.rename", &json!({"name": "Plain"})).unwrap();
    assert!(!says(&s, mark, "is cut to"), "{:?}", said(&s, mark));

    // Layouts: the same.
    s.run("layout.new", &json!({"name": long, "template": "blank"})).unwrap();
    let e = s.run("layout.duplicate", &json!({"newName": "l".repeat(crate::cmd::MAX_NAME + 1)})).unwrap_err();
    assert!(e.contains("at most 200"), "{e}");
    let mark = s.notice_mark();
    let r = s.run("layout.duplicate", &json!({})).unwrap();
    assert_eq!(r["name"], format!("{} copy", "m".repeat(195)));
    assert!(says(&s, mark, "Duplicate Layout: the copy's name is cut to 195"), "{:?}", said(&s, mark));
    let mark = s.notice_mark();
    s.run("layout.new", &json!({"name": long, "template": "blank"})).unwrap();
    assert!(says(&s, mark, "New Layout: the name given is cut to 198 characters to fit \" 2\""), "{:?}", said(&s, mark));
    let mark = s.notice_mark();
    s.run("layout.duplicate", &json!({"newName": "Short copy"})).unwrap();
    s.run("layout.rename", &json!({"newName": "Shorter"})).unwrap();
    s.run("layout.new", &json!({"name": "Other", "template": "blank"})).unwrap();
    assert!(!says(&s, mark, "is cut to"), "{:?}", said(&s, mark));
}

#[test]
fn numbered_bookmarks_and_pasted_layers_say_their_cuts() {
    let mut s = world();
    let long = "b".repeat(crate::cmd::navigate::MAX_BOOKMARK_NAME);
    s.run("bookmark.add", &json!({"name": long})).unwrap();
    let mark = s.notice_mark();
    s.run("bookmark.add", &json!({"name": "Here"})).unwrap();
    assert!(!says(&s, mark, "is cut to"), "{:?}", said(&s, mark));
    let r = s.run("bookmark.add", &json!({"name": long})).unwrap();
    assert_eq!(r["name"], format!("{} 2", "b".repeat(118)));
    assert!(says(&s, mark, "New Bookmark: the name given is cut to 118 characters to fit \" 2\""), "{:?}", said(&s, mark));

    // A pasted layer named at the limit, its name taken, is cut to fit " copy": counted.
    let layer = "l".repeat(crate::cmd::layer::MAX_LAYER_NAME);
    s.run("layer.rename", &json!({"layer": "Cities", "name": layer})).unwrap();
    s.run("layer.copy", &json!({"layer": [layer, "Rivers"]})).unwrap();
    let mark = s.notice_mark();
    s.run("layer.paste", &json!({})).unwrap();
    assert!(says(&s, mark, "Paste Layers: 1 copied name(s) cut"), "{:?}", said(&s, mark));
    let mark = s.notice_mark();
    s.run("layer.copy", &json!({"layer": "Rivers"})).unwrap();
    s.run("layer.paste", &json!({})).unwrap();
    assert!(!says(&s, mark, "cut"), "{:?}", said(&s, mark));
}

#[test]
fn a_downloaded_layer_named_from_a_long_address_says_the_cut() {
    let mut s = world();
    let csv = b"name,lon,lat\nA,1,2\n".to_vec();
    let long = format!("https://example.org/{}.csv", "d".repeat(150));
    let id = s.download_started();
    assert!(s.download_landed(id, csv.clone()));
    let mark = s.notice_mark();
    s.run("layer.addDownload", &json!({"id": id, "url": long})).unwrap();
    assert!(
        says(
            &s,
            mark,
            "Add Downloaded Data: the address's file name is 154 characters, so the file name the layer is named from is cut to 120"
        ),
        "{:?}",
        said(&s, mark)
    );
    // A zip's layers are named by the files in it: nothing to say of its address.
    let long_zip = format!("https://example.org/{}.zip", "z".repeat(150));
    assert_eq!(crate::cmd::layer::url_name_cut("Add Downloaded Data", &long_zip, &json!({}), b"PK\x03\x04rest"), None);
    assert!(crate::cmd::layer::url_name_cut("Add Downloaded Data", &long_zip, &json!({}), b"not a zip").is_some());
    // A .shp address's zip is made of its parts and named from the address: said.
    let long_shp = format!("https://example.org/{}.shp", "s".repeat(150));
    assert!(crate::cmd::layer::url_name_cut("Add Downloaded Data", &long_shp, &json!({}), b"PK\x03\x04rest").is_some());
    // A CityJSON model's added ".city" doesn't hide a cut.
    let city = format!("https://example.org/{}.json", "c".repeat(120));
    let model = br#"{"type":"CityJSON","version":"2.0","CityObjects":{},"vertices":[],"transform":{"scale":[1,1,1],"translate":[0,0,0]}}"#;
    assert!(crate::cmd::layer::url_file_name(&city, model).unwrap().ends_with(".city.json"));
    assert!(crate::cmd::layer::url_name_cut("Add Downloaded Data", &city, &json!({}), model).is_some());
    // A long "extension" that isn't one is dropped, the short stem kept whole: nothing cut.
    let odd = format!("https://example.org/a.{}", "x".repeat(121));
    assert_eq!(
        crate::cmd::layer::url_name_cut("Add Downloaded Data", &odd, &json!({}), br#"{"type":"FeatureCollection","features":[]}"#),
        None
    );
    // Named, or a short address: nothing to say.
    for (url, name) in [(long.as_str(), Some("Named")), ("https://example.org/short.csv", None)] {
        let id = s.download_started();
        assert!(s.download_landed(id, csv.clone()));
        let mark = s.notice_mark();
        let mut p = json!({"id": id, "url": url});
        if let Some(n) = name {
            p["name"] = json!(n);
        }
        s.run("layer.addDownload", &p).unwrap();
        assert!(!says(&s, mark, "file name is"), "{:?}", said(&s, mark));
    }
}

#[test]
fn a_project_with_keys_this_version_doesnt_read_says_so() {
    use crate::project::ReadPart;
    let mut s = world();
    s.run("layout.new", &json!({"name": "Page", "template": "blank"})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    let unknown = |p: &mut Project| format!("{:?}", p.take_read_notices(&crate::project::ALL_PARTS));
    // A clean file: nothing left out, nothing said.
    let mut clean = Project::from_json(&text).unwrap();
    let notes = unknown(&mut clean);
    assert!(!notes.contains("doesn't read"), "{notes}");

    let mut file: Value = serde_json::from_str(&text).unwrap();
    file["glow"] = json!(1);
    file["maps"][0]["haze"] = json!(true);
    file["maps"][0]["layers"][0]["shine"] = json!({"a": 1});
    file["maps"][0]["layers"][1]["shine"] = json!(2);
    file["maps"][0]["layers"][2]["shine"] = json!(3);
    file["maps"][0]["layers"][3]["shine"] = json!(4);
    file["layouts"][0]["tilt"] = json!(5);
    file["layouts"][0]["a\nb\u{202E}c"] = json!(6);
    let mut opened = Project::from_json(&file.to_string()).unwrap();
    let maps = format!("{:?}", opened.take_read_notices(&[ReadPart::Maps]));
    assert!(maps.contains("\"5\"") && maps.contains("maps.0.haze") && maps.contains("and 2 more"), "{maps}");
    let mut opened = Project::from_json(&file.to_string()).unwrap();
    let layouts = format!("{:?}", opened.take_read_notices(&[ReadPart::Layouts]));
    assert!(layouts.contains("\"2\"") && layouts.contains("layouts.0.tilt") && layouts.contains("a b c"), "{layouts}");
    let mut opened = Project::from_json(&file.to_string()).unwrap();
    let items = format!("{:?}", opened.take_read_notices(&[ReadPart::Items]));
    assert!(items.contains("\"1\"") && items.contains("glow") && !items.contains("haze"), "{items}");

    // An import that takes only the first map counts and names only its keys, not the second's.
    let mut two = file.clone();
    let mut first = serde_json::from_str::<Value>(&text).unwrap()["maps"][0].clone();
    first["mist"] = json!(1);
    first["name"] = json!("First");
    if let Some(maps) = two["maps"].as_array_mut() {
        maps.insert(0, first);
    }
    let mut taken = Project::from_import_maps_json(&two.to_string(), None, 1).unwrap();
    let maps = format!("{:?}", taken.take_read_notices(&[ReadPart::Maps]));
    assert!(maps.contains("Plain(\"1\")") && maps.contains("maps.0.mist") && !maps.contains("maps.1"), "{maps}");

    // Opened by the session, each part's keys come in one notice.
    let dir = std::env::temp_dir().join(format!("rustgis-unknown-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("keys.rgis");
    std::fs::write(&path, file.to_string()).unwrap();
    let mark = s.notice_mark();
    s.run("project.open", &json!({"path": path})).unwrap();
    let notes = said(&s, mark);
    assert_eq!(notes.iter().filter(|n| n.contains("key(s) this version doesn't read were left out")).count(), 3, "{notes:?}");
    std::fs::write(&path, &text).unwrap();
    let mark = s.notice_mark();
    s.run("project.open", &json!({"path": path})).unwrap();
    assert!(!says(&s, mark, "doesn't read"), "{:?}", said(&s, mark));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn label_repeat_and_line_offset_where_they_do_nothing_are_said() {
    let mut s = world();
    // On points: kept, and said.
    let mark = s.notice_mark();
    s.run("labels.placement", &json!({"layer": "Cities", "repeat": 100, "lineOffset": 4})).unwrap();
    assert!(says(&s, mark, "Cities: repeat and lineOffset kept, but it does nothing here"), "{:?}", said(&s, mark));
    let c = &s.map().layer_by_name("Cities").unwrap().labels.default;
    assert!(c.repeat.is_some() && c.line_offset == 4.0);
    // On lines placed parallel: nothing to say; nor for 0 or null, which take them away.
    let mark = s.notice_mark();
    s.run("labels.placement", &json!({"layer": "Rivers", "position": "parallel", "repeat": 100, "lineOffset": 4})).unwrap();
    s.run("labels.placement", &json!({"layer": "Cities", "repeat": 0, "lineOffset": null})).unwrap();
    assert!(!says(&s, mark, "kept, but"), "{:?}", said(&s, mark));
    // A line class placed another way: said.
    let mark = s.notice_mark();
    s.run("labels.placement", &json!({"layer": "Rivers", "position": "center", "lineOffset": 2})).unwrap();
    assert!(says(&s, mark, "Rivers: lineOffset kept"), "{:?}", said(&s, mark));
}

#[test]
fn layout_shape_values_held_to_their_limits_are_said() {
    let mut s = world();
    s.run("layout.new", &json!({"name": "Page", "template": "blank"})).unwrap();
    let mark = s.notice_mark();
    let r = s
        .run(
            "layout.addElement",
            &json!({"kind": "rectangle", "x": 10, "y": 10, "w": 50, "h": 30, "width": 80, "radius": 70, "background": {"padding": 1e9}}),
        )
        .unwrap();
    let notes = said(&s, mark);
    let held = notes.iter().find(|n| n.starts_with("Add Element:")).cloned().unwrap_or_default();
    assert!(
        held.contains("width 80 held to 50")
            && held.contains("radius 70 held to 50")
            && held.contains("background padding 1000000000 held to 50"),
        "{notes:?}"
    );
    let id = r["element"].clone();
    // Within the limits: nothing said.
    let mark = s.notice_mark();
    s.run("layout.addElement", &json!({"kind": "rectangle", "x": 10, "y": 10, "w": 50, "h": 30, "width": 2, "radius": 5})).unwrap();
    s.run("layout.update", &json!({"element": id, "width": 3, "radius": 4, "background": {"padding": 2}})).unwrap();
    s.run("layout.addElement", &json!({"kind": "line", "x": 10, "y": 10, "w": 50, "h": 30, "width": 1})).unwrap();
    assert!(!says(&s, mark, "held to"), "{:?}", said(&s, mark));
    // Past them on an update, and a line thinner than its least: said.
    let mark = s.notice_mark();
    s.run("layout.update", &json!({"element": id, "radius": 90})).unwrap();
    s.run("layout.addElement", &json!({"kind": "line", "x": 10, "y": 10, "w": 50, "h": 30, "width": 0.01})).unwrap();
    assert!(says(&s, mark, "Element Properties: radius 90 held to 50"), "{:?}", said(&s, mark));
    assert!(says(&s, mark, "Add Element: width 0.01 held to 0.1"), "{:?}", said(&s, mark));
}

#[test]
fn gp_env_says_a_cell_size_dropped_with_its_system() {
    let mut s = world();
    s.run("gp.env", &json!({"crs": "EPSG:32616", "cellSize": 100})).unwrap();
    // The same system again keeps it: nothing to say.
    let mark = s.notice_mark();
    s.run("gp.env", &json!({"crs": "EPSG:32616"})).unwrap();
    // Another system with its own cell size: nothing dropped unsaid.
    s.run("gp.env", &json!({"crs": "EPSG:32617", "cellSize": 50})).unwrap();
    assert!(!says(&s, mark, "dropped with that system"), "{:?}", said(&s, mark));
    // Another system without one: said.
    let mark = s.notice_mark();
    s.run("gp.env", &json!({"crs": "EPSG:3857"})).unwrap();
    assert!(says(&s, mark, "the cell size 50 was in"), "{:?}", said(&s, mark));
    assert_eq!(s.project.gp_env.projected.and_then(|g| g.cell_size), None);
    // A change with cellSize null gives the new system none: said too.
    s.run("gp.env", &json!({"crs": "EPSG:32616", "cellSize": 100})).unwrap();
    let mark = s.notice_mark();
    s.run("gp.env", &json!({"crs": "EPSG:32617", "cellSize": null})).unwrap();
    assert!(says(&s, mark, "the cell size 100 was in"), "{:?}", said(&s, mark));
    // The system cleared, with null or blank text: said; cleared with no cell size: nothing.
    for clear in [json!(null), json!(" ")] {
        s.run("gp.env", &json!({"crs": "EPSG:32616", "cellSize": 100})).unwrap();
        let mark = s.notice_mark();
        s.run("gp.env", &json!({"crs": clear})).unwrap();
        assert!(says(&s, mark, "the cell size 100 in"), "{:?}", said(&s, mark));
    }
    let mark = s.notice_mark();
    s.run("gp.env", &json!({"crs": null})).unwrap();
    assert!(!says(&s, mark, "cell size"), "{:?}", said(&s, mark));
}

/// Every project fixture, of each format and capability, opens with only keys this version reads,
/// but `unknown_keys.rgis`, whose key another program wrote is said (#2639).
#[test]
fn the_schema_fixtures_have_no_keys_left_out() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/fixtures");
    let mut read = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rgis") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut p = Project::from_json(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let notes = format!("{:?}", p.take_read_notices(&crate::project::ALL_PARTS));
        if path.file_name().is_some_and(|n| n == "unknown_keys.rgis") {
            assert!(notes.contains("doesn't read") && notes.contains("future_setting"), "{notes}");
            continue;
        }
        assert!(!notes.contains("doesn't read"), "{}: {notes}", path.display());
        read += 1;
    }
    assert!(read > 20, "{read} fixtures read");
}
