use serde_json::{Value, json};

use crate::{Project, Session, sample};

/// Keeps tests away from the user's installed plugins.
fn isolate_plugins() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("rustgis-test-plugins-{}", std::process::id()));
        rustgis_plugins::store::set_dir(Some(dir));
    });
}

fn world() -> Session {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    s.run("project.sample", &Value::Null).unwrap();
    s
}

#[test]
fn worldview_swaps_only_untouched_sample_countries() {
    use sample::{Worldview, swap_countries};
    let countries = |s: &Session| s.map().layer_by_name("Countries").unwrap().dataset.features.len();
    let mut s = world();
    s.dirty = false;
    let n = countries(&s);
    // The layer holds the default countries, not China's.
    assert!(!swap_countries(&mut s, Worldview::China, Worldview::Default));
    assert!(swap_countries(&mut s, Worldview::Default, Worldview::China));
    assert_ne!(countries(&s), n);
    assert!(!s.dirty);
    assert!(swap_countries(&mut s, Worldview::China, Worldview::Default));
    assert_eq!(countries(&s), n);
    // Not in a project with changes, or a saved one.
    s.dirty = true;
    assert!(!swap_countries(&mut s, Worldview::Default, Worldview::China));
    s.dirty = false;
    s.path = Some("atlas.rgis".into());
    assert!(!swap_countries(&mut s, Worldview::Default, Worldview::China));
    // Not someone else's layer named Countries.
    let mut t = world();
    t.dirty = false;
    t.run("edit.deleteField", &json!({"layer": "Countries", "field": "pop_est"})).unwrap();
    t.dirty = false;
    assert!(!swap_countries(&mut t, Worldview::Default, Worldview::China));
}

#[test]
fn sample_project_loads() {
    let s = world();
    let names: Vec<&str> = s.map().layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Cities", "Rivers", "Lakes", "Countries"]);
    assert!(s.active_layer.is_some());
    assert!(!sample::world_project().map().bookmarks.is_empty());
}

#[test]
fn layer_move_reorders_in_one_undo_step() {
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    let depth = s.undo_depth();
    // Down three places: one command, one undo step.
    s.run("layer.move", &json!({"layer": "Cities", "to": 3})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Lakes", "Countries", "Cities"]);
    assert_eq!(s.undo_depth(), depth + 1);
    // Up, to the middle.
    s.run("layer.move", &json!({"layer": "Cities", "to": 1})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Cities", "Lakes", "Countries"]);
    // Past the end is the bottom.
    s.run("layer.move", &json!({"layer": "Rivers", "to": 1e9})).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes", "Countries", "Rivers"]);
    // Staying put, a missing or junk position and an unknown layer fail and leave no undo step.
    let depth = s.undo_depth();
    for p in [
        json!({"layer": "Rivers", "to": 3}),
        json!({"layer": "Rivers", "to": 99}),
        json!({"layer": "Rivers"}),
        json!({"layer": "Rivers", "to": -1}),
        json!({"layer": "Rivers", "to": 1.5}),
        json!({"layer": "Nope", "to": 0}),
    ] {
        assert!(s.run("layer.move", &p).is_err(), "{p}");
    }
    assert_eq!(names(&s), ["Cities", "Lakes", "Countries", "Rivers"]);
    assert_eq!(s.undo_depth(), depth);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(names(&s), ["Rivers", "Cities", "Lakes", "Countries"]);
}

#[test]
fn table_remove_takes_a_standalone_table_out_of_the_project() {
    let mut s = world();
    let names = |s: &Session| s.project.tables.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    for name in ["sites", "notes"] {
        s.run("layer.addDataText", &json!({"fileName": format!("{name}.csv"), "text": "id,street\n1,Champ de Mars\n"})).unwrap();
    }
    assert_eq!(names(&s), ["sites", "notes"]);
    let (depth, layers) = (s.undo_depth(), s.map().layers.len());
    s.run("table.remove", &json!({"table": "sites"})).unwrap();
    assert_eq!(names(&s), ["notes"]);
    assert_eq!((s.undo_depth(), s.map().layers.len()), (depth + 1, layers), "one undo step, and the map's layers stay");
    // A table that isn't there, a layer's name and a missing or junk name fail and leave no undo step.
    for p in [json!({"table": "sites"}), json!({"table": "Cities"}), json!({}), json!({"table": 3}), Value::Null] {
        assert!(s.run("table.remove", &p).is_err(), "{p}");
    }
    assert_eq!((names(&s), s.undo_depth()), (vec!["notes".to_string()], depth + 1));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(names(&s), ["sites", "notes"]);
    // Two tables of one name have ids of their own: the id says which, the name is the first.
    s.run("layer.addDataText", &json!({"fileName": "notes.csv", "text": "id,city\n1,Paris\n"})).unwrap();
    assert_eq!(names(&s), ["sites", "notes", "notes"]);
    let ids: Vec<u32> = s.project.tables.iter().map(|t| t.id).collect();
    assert!(ids.iter().all(|&id| id != 0) && ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2], "{ids:?}");
    for table in [json!(0), json!(9999), json!(-1), json!(1.5), json!("9999")] {
        assert!(s.run("table.remove", &json!({"table": table})).is_err(), "{table}");
    }
    // The position the command took before ids is refused, not ignored.
    assert!(s.run("table.remove", &json!({"table": "notes", "at": 2})).is_err());
    assert_eq!(names(&s), ["sites", "notes", "notes"]);
    s.run("table.remove", &json!({"table": ids[2]})).unwrap();
    assert_eq!(s.project.tables.iter().map(|t| t.id).collect::<Vec<_>>(), [ids[0], ids[1]]);
    assert!(s.project.table(ids[1]).is_some_and(|t| t.field_index("street").is_some()), "the first of the two stays");
    // An id in text works as well, and an id isn't given again: undo brings the table back under its own.
    s.run("table.remove", &json!({"table": ids[1].to_string()})).unwrap();
    let again = s.run("layer.addDataText", &json!({"fileName": "more.csv", "text": "id,city\n1,Paris\n"})).unwrap();
    assert!(again["id"].as_u64().is_some_and(|id| ids.iter().all(|&old| u64::from(old) != id)), "{again}");
    s.run("edit.undo", &Value::Null).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.tables.iter().map(|t| t.id).collect::<Vec<_>>(), [ids[0], ids[1]]);
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(s.project.tables.iter().map(|t| t.id).collect::<Vec<_>>(), [ids[0]]);
    // A table added and undone leaves its id unused: the next table gets another.
    let undone = s.run("layer.addDataText", &json!({"fileName": "a.csv", "text": "id,city\n1,Paris\n"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    let next = s.run("layer.addDataText", &json!({"fileName": "b.csv", "text": "id,city\n1,Paris\n"})).unwrap();
    assert!(undone["id"].as_u64() < next["id"].as_u64(), "{undone} then {next}");
}

#[test]
fn standalone_tables_keep_their_ids_in_the_project_file() {
    let mut s = world();
    for name in ["sites", "sites", "notes"] {
        s.run("layer.addDataText", &json!({"fileName": format!("{name}.csv"), "text": "id,street\n1,Champ de Mars\n"})).unwrap();
    }
    s.run("table.remove", &json!({"table": "sites"})).unwrap();
    let ids: Vec<u32> = s.project.tables.iter().map(|t| t.id).collect();
    // Saved and read again: the same ids, and the next table gets one that was never used.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut p = Project::from_json(&text).unwrap();
    assert_eq!(
        p.tables.iter().map(|t| (t.id, t.name.clone(), t.features.len())).collect::<Vec<_>>(),
        [(ids[0], "sites".to_string(), 1), (ids[1], "notes".to_string(), 1)]
    );
    let next = p.add_table(rustgis_data::Dataset::new("more", rustgis_data::GeometryType::None, Vec::new()));
    assert!(next > ids[1], "{next} after {ids:?}");
    // A file from before tables had ids, and one with an id repeated by hand: every table gets
    // an id of its own, and those that had a good one keep it.
    let mut v: Value = serde_json::from_str(&text).unwrap();
    for t in v["tables"].as_array_mut().unwrap() {
        t.as_object_mut().unwrap().remove("id");
    }
    v.as_object_mut().unwrap().remove("next_table_id");
    let old = Project::from_json(&v.to_string()).unwrap();
    assert_eq!(old.tables.iter().map(|t| t.id).collect::<Vec<_>>(), [1, 2]);
    v["tables"][0]["id"] = json!(7);
    v["tables"][1]["id"] = json!(7);
    let edited = Project::from_json(&v.to_string()).unwrap();
    assert_eq!(edited.tables.iter().map(|t| t.id).collect::<Vec<_>>(), [7, 8]);
    // A tool's table put again under its name keeps the id.
    let stats = json!({"tool": "summaryStatistics", "params": {"input": "Countries", "field": "pop_est", "case": "continent"}});
    s.run("gp.run", &stats).unwrap();
    let id = s.project.tables.last().map(|t| t.id).unwrap();
    s.run("gp.run", &stats).unwrap();
    assert_eq!((s.project.tables.len(), s.project.tables.last().map(|t| t.id)), (3, Some(id)));
    // Commands and tools reach a table by its id as well as by its name, which is the first of
    // two tables sharing it.
    for v in [10, 99] {
        s.run("layer.addDataText", &json!({"fileName": "counts.csv", "text": format!("k,v\na,{v}\n")})).unwrap();
    }
    let info = s.run("info.project", &Value::Null).unwrap();
    let counts: Vec<u64> =
        info["tables"].as_array().unwrap().iter().filter(|t| t["name"] == "counts").filter_map(|t| t["id"].as_u64()).collect();
    assert_eq!((counts.len(), info["tables"][0]["rows"].as_u64()), (2, Some(1)), "{info}");
    // A name exactly as written comes before one that differs in case, for tools as for commands.
    s.run("layer.addDataText", &json!({"fileName": "Counts.csv", "text": "k,v\na,5\n"})).unwrap();
    let upper = s.project.tables.last().map(|t| t.id).unwrap();
    assert_eq!(s.table_index(&json!({"table": "Counts"})).ok().and_then(|i| s.project.tables.get(i)).map(|t| t.id), Some(upper));
    for (input, sum) in [(json!("counts"), 10.0), (json!("COUNTS"), 10.0), (json!("Counts"), 5.0), (json!(counts[1].to_string()), 99.0)] {
        s.run("gp.run", &json!({"tool": "summaryStatistics", "params": {"input": input, "field": "v"}})).unwrap();
        // The run's table is the last: put again under its name, or added.
        let out = s.project.tables.last().unwrap();
        assert_eq!(out.value(0, out.field_index("SUM_v").unwrap()), &rustgis_data::Value::Float(sum), "{input}");
    }
    // A table named as another's id: the text finds it by its name, and a number is the id.
    s.run("layer.addDataText", &json!({"fileName": format!("{}.csv", counts[1]), "text": "k,v\na,1\n"})).unwrap();
    assert_eq!(s.project.tables.last().map(|t| t.name.clone()), Some(counts[1].to_string()));
    for (input, sum) in [(json!(counts[1].to_string()), 1.0), (json!(counts[1]), 99.0)] {
        s.run("gp.run", &json!({"tool": "summaryStatistics", "params": {"input": input, "field": "v"}})).unwrap();
        let out = s.project.tables.last().unwrap();
        assert_eq!(out.value(0, out.field_index("SUM_v").unwrap()), &rustgis_data::Value::Float(sum), "{input}");
    }
    for input in [json!(9999), json!(-1), json!(1.5)] {
        assert!(s.run("gp.run", &json!({"tool": "summaryStatistics", "params": {"input": input, "field": "v"}})).is_err());
    }
    let dir = std::env::temp_dir().join(format!("rustgis-table-ids-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("counts.csv");
    s.run("share.exportTable", &json!({"table": counts[1], "path": path})).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("99"));
    std::fs::remove_dir_all(&dir).ok();
    assert!(s.run("share.exportTable", &json!({"table": 9999, "path": path})).is_err());
    // The table the attribute table shows is forgotten with the table, and with the project.
    s.table_standalone = Some(id);
    s.run("table.remove", &json!({"table": ids[0]})).unwrap();
    assert_eq!(s.table_standalone, Some(id), "another table's removal leaves it");
    s.run("table.remove", &json!({"table": id})).unwrap();
    assert_eq!(s.table_standalone, None);
    s.table_standalone = Some(ids[1]);
    s.run("project.new", &Value::Null).unwrap();
    assert_eq!(s.table_standalone, None);
}

#[test]
fn select_symbolize_undo_redo() {
    let mut s = world();
    let r = s.run("select.byAttributes", &json!({"layer": "Countries", "where": "continent = 'Africa'"})).unwrap();
    assert!(r["selected"].as_u64().unwrap() > 40);
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "continent"})).unwrap();
    assert_eq!(s.map().layer_by_name("Countries").unwrap().renderer.kind_label(), "Unique Values");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Countries").unwrap().renderer.kind_label(), "Graduated Colors");
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Countries").unwrap().renderer.kind_label(), "Unique Values");
}

#[test]
fn undo_steps_share_the_layers_they_leave_alone() {
    // #214: each undo step held a full copy of every layer, so a large layer grew by its whole
    // size with every command (a rename, a style change).
    let mut s = world();
    s.map_mut().layers.iter_mut().for_each(|l| {
        l.projected();
    });
    s.run("layer.rename", &json!({"layer": "Countries", "name": "Nations"})).unwrap();
    s.run("layer.rename", &json!({"layer": "Nations", "name": "States"})).unwrap();
    let live = s.map().layer_by_name("States").unwrap();
    let id = live.id;
    for (snap, _, _, _) in &s.undo {
        let old = snap.maps.iter().flat_map(|m| &m.layers).find(|l| l.id == id).unwrap();
        assert!(old.dataset.features.shares(&live.dataset.features), "the snapshot copied the features");
        assert_eq!(old.projected_cache().as_ptr(), live.projected_cache().as_ptr(), "the snapshot copied the projected geometry");
    }
    // An edit copies the layer it changes, once, and leaves the snapshot as it was.
    let before = s.map().layer_by_name("Cities").unwrap().dataset.features.len();
    s.run("edit.addFeature", &json!({"layer": "Cities", "geometry": {"type": "Point", "coordinates": [1.0, 2.0]}})).unwrap();
    let cities = s.map().layer_by_name("Cities").unwrap();
    let (snap, _, _, _) = s.undo.last().unwrap();
    let old = snap.maps.iter().flat_map(|m| &m.layers).find(|l| l.id == cities.id).unwrap();
    assert!(!old.dataset.features.shares(&cities.dataset.features));
    assert_eq!((old.dataset.features.len(), cities.dataset.features.len()), (before, before + 1));
    let states = s.map().layer_by_name("States").unwrap();
    let old = snap.maps.iter().flat_map(|m| &m.layers).find(|l| l.id == id).unwrap();
    assert!(old.dataset.features.shares(&states.dataset.features));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().dataset.features.len(), before);
}

#[test]
fn geoprocessing_adds_layers_and_history() {
    let mut s = world();
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 200}})).unwrap();
    assert!(s.map().layer_by_name("Cities_Buffer").is_some());
    s.run("gp.run", &json!({"tool": "summaryStatistics", "params": {"input": "Countries", "field": "pop_est", "case": "continent"}}))
        .unwrap();
    assert_eq!(s.project.tables.len(), 1);
    assert!(s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Nope"}})).is_err());
    assert_eq!(s.project.gp_history.len(), 2, "a failed run restores the project");
}

#[test]
fn spatial_join_command_joins_layers_and_records_history() {
    let mut s = world();
    s.run(
        "gp.spatialJoin",
        &json!({
            "target": "Cities",
            "join": "Countries",
            "match": "Intersects",
            "operation": "Join one to one",
            "output": "Joined_Cities"
        }),
    )
    .unwrap();
    let l = s.map().layer_by_name("Joined_Cities").unwrap();
    assert_eq!(l.dataset.features.len(), s.map().layer_by_name("Cities").unwrap().dataset.features.len());
    assert!(l.dataset.field_index("join_count").is_some());
    assert_eq!(s.project.gp_history.len(), 1);

    s.run(
        "gp.run",
        &json!({
            "tool": "spatialJoin",
            "params": {
                "target": "Countries",
                "join": "Cities",
                "match": "Contains",
                "output": "Countries_With_Cities"
            }
        }),
    )
    .unwrap();
    assert!(s.map().layer_by_name("Countries_With_Cities").is_some());
}

#[test]
fn editing_features_and_fields() {
    let mut s = world();
    s.run(
        "edit.addFeature",
        &json!({"layer": "Cities", "geometry": {"type": "Point", "coordinates": [1.0, 2.0]}, "attributes": {"name": "Null Island"}}),
    )
    .unwrap();
    let n = s.map().layer_by_name("Cities").unwrap().dataset.features.len();
    assert!(
        s.run("edit.addFeature", &json!({"layer": "Cities", "geometry": {"type": "LineString", "coordinates": [[0,0],[1,1]]}})).is_err()
    );
    s.run("edit.addField", &json!({"layer": "Cities", "name": "score", "type": "double"})).unwrap();
    s.run("edit.calculateField", &json!({"layer": "Cities", "field": "score", "expression": "pop_max / 1000"})).unwrap();
    s.run("edit.delete", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().dataset.features.len(), n - 1);
}

#[test]
fn line_strokes_stack_and_survive_save_and_undo() {
    use rustgis_map::{LineCap, LineJoin, LineStroke, Rgba};
    let mut s = world();
    let depth = s.undo_depth();
    // A black casing under an orange core, both shifted 8.5 pt right (#478).
    let strokes = json!([
        {"color": "#000000", "width": 11.5, "offset": 8.5, "cap": "round", "join": "round"},
        {"color": "#FF8C00", "width": 7, "offset": 8.5, "cap": "flat", "join": "miter"},
        {"offset": -2}
    ]);
    s.run("layer.lineStrokes", &json!({"layer": "Rivers", "strokes": strokes})).unwrap();
    let want = vec![
        LineStroke { color: Some(Rgba::rgb(0, 0, 0)), width: Some(11.5), offset: 8.5, cap: LineCap::Round, join: LineJoin::Round },
        LineStroke { color: Some(Rgba::rgb(0xFF, 0x8C, 0)), width: Some(7.0), offset: 8.5, cap: LineCap::Butt, join: LineJoin::Miter },
        LineStroke { offset: -2.0, ..LineStroke::default() },
    ];
    assert_eq!(s.map().layer_by_name("Rivers").unwrap().line_strokes, want);
    assert_eq!(s.undo_depth(), depth + 1);
    // Kept in the project file, and drawn in an export.
    let dir = std::env::temp_dir().join(format!("rustgis-strokes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("strokes.rgis");
    s.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    assert_eq!(t.map().layer_by_name("Rivers").unwrap().line_strokes, want);
    let png = dir.join("map.png");
    t.run("share.exportMap", &json!({"path": png.display().to_string(), "width": 320, "height": 200})).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    // Out-of-range numbers are brought into range; junk is refused and changes nothing.
    s.run("layer.lineStrokes", &json!({"layer": "Rivers", "strokes": [{"width": 1e9, "offset": -1e9}]})).unwrap();
    let st = s.map().layer_by_name("Rivers").unwrap().line_strokes.clone();
    assert_eq!((st[0].width, st[0].offset), (Some(50.0), -100.0));
    s.run("layer.lineStrokes", &json!({"layer": "Rivers", "strokes": [{"width": 1e300, "offset": 1e300}]})).unwrap();
    let st = s.map().layer_by_name("Rivers").unwrap().line_strokes.clone();
    assert_eq!((st[0].width, st[0].offset), (Some(50.0), 100.0));
    let depth = s.undo_depth();
    for bad in [
        json!({"layer": "Rivers"}),
        json!({"layer": "Rivers", "strokes": [{"cap": "pointy"}]}),
        json!({"layer": "Rivers", "strokes": [{"join": "zigzag"}]}),
        json!({"layer": "Rivers", "strokes": [{"cap": 5}]}),
        json!({"layer": "Rivers", "strokes": [{"join": true}]}),
        json!({"layer": "Rivers", "strokes": [{"color": "orange"}]}),
        json!({"layer": "Rivers", "strokes": [{"width": "wide"}]}),
        json!({"layer": "Rivers", "strokes": [7]}),
        json!({"layer": "Rivers", "strokes": vec![json!({}); 17]}),
        json!({"layer": "Cities", "strokes": []}),
    ] {
        assert!(s.run("layer.lineStrokes", &bad).is_err(), "{bad}");
    }
    assert_eq!(s.undo_depth(), depth);
    // [] goes back to the single stroke; undo brings the stack back.
    s.run("layer.lineStrokes", &json!({"layer": "Rivers", "strokes": []})).unwrap();
    assert!(s.map().layer_by_name("Rivers").unwrap().line_strokes.is_empty());
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Rivers").unwrap().line_strokes.len(), 1);
}

#[test]
fn project_save_and_open_roundtrip() {
    let mut s = world();
    let dir = std::env::temp_dir().join(format!("rustgis-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("atlas.rgis");
    s.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    assert_eq!(t.map().layers.len(), 4);
    let png = dir.join("map.png");
    t.run("share.exportMap", &json!({"path": png.display().to_string(), "width": 320, "height": 200})).unwrap();
    assert!(std::fs::metadata(&png).unwrap().len() > 1000);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn project_extension_is_rgis_and_legacy_rgisx_opens() {
    let mut s = world();
    let dir = std::env::temp_dir().join(format!("rustgis-ext-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    s.run("project.saveAs", &json!({"path": dir.join("atlas").display().to_string()})).unwrap();
    let saved = dir.join("atlas.rgis");
    assert!(saved.exists(), "a path without an extension gets .rgis");
    let legacy = dir.join("old.rgisx");
    std::fs::copy(&saved, &legacy).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.open", &json!({"path": legacy.display().to_string()})).unwrap();
    assert_eq!(t.map().layers.len(), 4);
    for (name, is) in [("a.rgis", true), ("A.RGISX", true), ("a.rgismap", false), ("a.rgispkg", false), ("rgis", false)] {
        assert_eq!(crate::project::is_project_file(name), is, "{name}");
    }
    for (name, opens) in [("a.rgis", true), ("A.RGISPKG", true), ("rgispkg", false), ("a.geojson", false), ("a.rgismap", false)] {
        assert_eq!(crate::project::opens_as_project(name), opens, "{name}");
    }
    assert_eq!(crate::project::open_command("dir/a.geojson"), "layer.addData");
    std::fs::remove_dir_all(&dir).ok();
}

/// A table file picked inside a File Geodatabase stands for the geodatabase, in `layer.addData`
/// and `info.fileLayers` alike; through GDAL when it is installed.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_file_inside_a_geodatabase_adds_the_geodatabase() {
    let dir = std::env::temp_dir().join(format!("rustgis-gdb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let gdb = dir.join("roads.gdb");
    let table = gdb.join("a00000001.gdbtable").display().to_string();
    let mut s = Session::new(Project::default());
    if rustgis_data::source::gdal::available() {
        let geojson = dir.join("cities.geojson");
        std::fs::write(
            &geojson,
            r#"{"type":"FeatureCollection","features":[
{"type":"Feature","properties":{"name":"a"},"geometry":{"type":"Point","coordinates":[8.5,47.4]}},
{"type":"Feature","properties":{"name":"b"},"geometry":{"type":"Point","coordinates":[7.4,46.9]}}]}"#,
        )
        .unwrap();
        let made = std::process::Command::new("ogr2ogr")
            .args(["-f", "OpenFileGDB", "-nln", "cities"])
            .arg(&gdb)
            .arg(&geojson)
            .status()
            .is_ok_and(|st| st.success());
        if made {
            let listed = s.run("info.fileLayers", &json!({"path": table})).unwrap();
            assert_eq!(listed["layers"], json!(["cities"]), "{listed}");
            let added = s.run("layer.addData", &json!({"path": table})).unwrap();
            assert_eq!(added["features"], json!(2), "{added}");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
    }
    // Without GDAL the geodatabase can't be read, but the error names it, not the table.
    std::fs::create_dir_all(&gdb).unwrap();
    std::fs::write(&table, b"").unwrap();
    for cmd in ["layer.addData", "info.fileLayers"] {
        let e = s.run(cmd, &json!({"path": table})).unwrap_err();
        assert!(e.contains("roads.gdb") && !e.contains("gdbtable"), "{cmd}: {e}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_clears_dirty_and_open_clears_history() {
    let mut s = world();
    assert!(!s.dirty && !s.can_undo(), "a fresh sample is clean");
    s.run("layer.color", &json!({"layer": "Countries", "color": "#336699"})).unwrap();
    assert!(s.dirty && s.can_undo());
    let dir = std::env::temp_dir().join(format!("rustgis-dirty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.rgis");
    s.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
    assert!(!s.dirty, "saving clears the dirty marker");
    assert!(s.can_undo(), "saving keeps the edit history");
    s.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    assert!(!s.dirty && !s.can_undo() && !s.can_redo(), "opening starts a fresh history");
    assert!(s.run("edit.undo", &Value::Null).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn degenerate_geometry_and_deleted_fields() {
    let mut s = world();
    for g in [
        json!({"type": "LineString", "coordinates": [[0, 0]]}),
        json!({"type": "MultiPoint", "coordinates": []}),
        json!({"type": "MultiPolygon", "coordinates": []}),
    ] {
        let layer = if g["type"] == "LineString" {
            "Rivers"
        } else if g["type"] == "MultiPoint" {
            "Cities"
        } else {
            "Countries"
        };
        assert!(s.run("edit.addFeature", &json!({"layer": layer, "geometry": g})).is_err(), "{g}");
    }
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": "pop_est > 1000"})).unwrap();
    s.run("edit.deleteField", &json!({"layer": "Countries", "field": "pop_est"})).unwrap();
    let l = s.map().layer_by_name("Countries").unwrap();
    assert_eq!(l.renderer.kind_label(), "Single Symbol");
    assert!(l.definition_query.is_none());
    s.run("edit.deleteField", &json!({"layer": "Countries", "field": "name"})).unwrap();
    assert!(!s.map().layer_by_name("Countries").unwrap().labels.enabled);
}

#[test]
fn open_from_text() {
    let mut s = world();
    let text = serde_json::to_string(&s.project).unwrap();
    s.run("layer.addDataText", &json!({"fileName": "pts.csv", "text": "name,lon,lat\nA,1,2\n"})).unwrap();
    assert!(s.map().layer_by_name("pts").is_some());
    assert!(s.run("layer.addDataText", &json!({"fileName": "x.shp", "text": ""})).is_err());
    assert!(s.run("layer.addDataText", &json!({"fileName": "x.geojson", "text": "{"})).is_err());
    assert!(s.run("project.openText", &json!({"text": "not json"})).is_err());
    s.run("project.openText", &json!({"text": text})).unwrap();
    assert!(s.map().layer_by_name("pts").is_none() && !s.dirty && !s.can_undo());
}

#[test]
fn opened_project_is_named_after_its_file() {
    // Issue #197: a project saved from the sample and renamed kept the sample's name.
    let mut s = world();
    let text = serde_json::to_string(&s.project).unwrap();
    s.run("project.openText", &json!({"text": text, "fileName": "test.rgis"})).unwrap();
    assert_eq!(s.project.name, "test");
    s.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(s.project.name, "World Atlas", "without a file name the project keeps its own");
    s.run("project.openText", &json!({"text": text, "fileName": " .rgis"})).unwrap();
    assert_eq!(s.project.name, "World Atlas", "an empty file name doesn't rename it");
    let dir = std::env::temp_dir().join(format!("rustgis-named-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Trip plan.rgis");
    std::fs::write(&path, &text).unwrap();
    s.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    assert_eq!(s.project.name, "Trip plan");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn command_ids_are_unique() {
    let s = Session::new(Project::default());
    let mut ids: Vec<&str> = s.registry.commands.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(n, ids.len());
}

/// A 3-band 8×6 UTM 17N GeoTIFF near 81°W, 42°N, as base64.
fn utm_tiff_b64() -> String {
    use base64::Engine;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let gt = GeoTransform { origin_x: 497_000.0, origin_y: 4_651_000.0, pixel_width: 500.0, pixel_height: -500.0 };
    let bands = (0..3).map(|b| (0..48).map(|i| ((i * 5 + b * 40) % 256) as f32).collect()).collect();
    let r = Raster::new("utm", 8, 6, bands, gt, Crs::from_epsg(32617).unwrap(), None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
    base64::engine::general_purpose::STANDARD.encode(geotiff::write(&r, 0).unwrap())
}

/// The sample world with a streamed layer active.
fn with_streamed(path: &std::path::Path) -> Session {
    let mut s = world();
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true, "name": "Streamed"})).unwrap();
    s
}

fn with_raster() -> Session {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "scene.tif", "data": utm_tiff_b64()})).unwrap();
    s
}

/// The parts of a large GeoTIFF the web app reads (#140), opened and kept in a project: a
/// cloud-optimised file read a part at a time, as `parts.rs` reads a picked file.
#[test]
fn geotiff_parts_open_and_survive_a_saved_project() {
    use base64::Engine;
    use rustgis_data::raster::cog::{self, Sparse};
    use rustgis_data::raster::geotiff::{self, Partial};
    let file = cog::example(1024, 256);
    let len = file.len() as u64;
    let mut parts = Sparse::default().with_len(len).with(0, file[..4096].to_vec());
    let mut done = false;
    for _ in 0..100 {
        match geotiff::read_sparse(&parts, "scene").unwrap() {
            Partial::Done(_) => {
                done = true;
                break;
            }
            Partial::Need(a, b) => {
                let end = b.max(a + (64 << 10)).min(len).min(parts.next_start(a).unwrap_or(len));
                parts = parts.with(a, file[a as usize..end as usize].to_vec());
            }
        }
    }
    assert!(done);
    let container = parts.container();
    let mut s = world();
    let data = base64::engine::general_purpose::STANDARD.encode(&container);
    s.run("layer.addDataBytes", &json!({"fileName": "scene.tif", "data": data})).unwrap();
    let value = |s: &Session| s.map().layer_by_name("scene").and_then(|l| l.raster.as_ref()?.data.as_ref()?.value(0, 300, 200));
    // Pixel (x, y) of the example is (x + y) % 256.
    assert_eq!(value(&s), Some(244.0));
    // Saved, the project keeps the parts, not the whole file, and opens the raster again.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.len() < file.len() * 2, "{} for a {}-byte file", text.len(), file.len());
    s.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(value(&s), Some(244.0));
}

#[test]
fn imported_tasks_name_outputs_and_steps_are_edited() {
    let mut s = Session::new(sample::world_project());
    // A task from a file may name a tool's output layer, but not write to a path through it.
    let lesson = json!({"name": "Lesson", "steps": [
        {"title": "Buffer the cities", "instructions": "Analyze › Tools › Buffer", "command": "gp.run", "params": {"tool": "buffer", "params": {"input": "Cities", "distance": 100, "output": "Near cities"}}},
        {"title": "Zoom", "command": "layer.zoomTo", "params": {"layer": "Near cities"}},
        {"title": "Clear", "command": "select.clear"}
    ]});
    s.run("task.import", &json!({"text": lesson.to_string()})).unwrap();
    s.run("task.run", &json!({"task": "Lesson", "step": 0})).unwrap();
    assert!(s.map().layer_by_name("Near cities").is_some());
    let to_a_file = json!({"name": "To a file", "steps": [{"title": "Buffer", "command": "gp.run", "params": {"tool": "buffer", "params": {"input": "Cities", "distance": 100, "output": "C:/temp/x.shp"}}}]});
    assert!(s.run("task.import", &json!({"text": to_a_file.to_string()})).is_err());
    assert!(crate::cmd::items::touches_files("gp.run", &json!({"tool": "buffer", "params": {"output": "../x.shp"}})));
    assert!(!crate::cmd::items::touches_files("gp.run", &json!({"tool": "buffer", "params": {"output": "Near cities"}})));
    // Only a plain layer name, only gp.run's, only for RustGIS's own tools.
    for (command, params) in [
        ("gp.run", json!({"tool": "buffer", "params": {"output": "x.shp"}})),
        ("gp.run", json!({"tool": "buffer", "params": {"output": "sub/x"}})),
        ("gp.run", json!({"tool": "buffer", "params": {"output": "sub\\x"}})),
        ("gp.run", json!({"tool": "buffer", "params": {"output": ".bashrc"}})),
        ("gp.run", json!({"tool": "geolibre:slope", "params": {"output": "slope"}})),
        ("gp.run", json!({"tool": "plugin:kit/hex", "params": {"output": "hex"}})),
        // Not one of RustGIS's own tools, even without a prefix.
        ("gp.run", json!({"tool": "someOtherTool", "params": {"output": "x"}})),
        ("gp.run", json!({"tool": "buffer", "params": {"input": "Cities", "nested": {"output": "x"}}})),
        ("layer.rename", json!({"layer": "Cities", "output": "x"})),
    ] {
        assert!(crate::cmd::items::touches_files(command, &params), "{command} {params}");
    }

    // `"step": null` runs the whole task, as before; a step that isn't an index is refused.
    s.run("select.all", &json!({"layer": "Cities"})).unwrap();
    let r = s.run("task.run", &json!({"task": "Lesson", "step": null})).unwrap();
    assert_eq!(r["ran"].as_array().unwrap().len(), 3);
    assert!(s.run("task.run", &json!({"task": "Lesson", "step": "two"})).is_err());

    // Steps are edited, moved and deleted, each one undoable.
    s.run(
        "task.editStep",
        &json!({"task": "Lesson", "step": 1, "title": "  Look at it  ", "instructions": "Line one.
Line two."}),
    )
    .unwrap();
    let step = |s: &Session, i: usize| s.project.tasks.iter().find(|t| t.name == "Lesson").unwrap().steps[i].clone();
    assert_eq!(
        (step(&s, 1).title.as_str(), step(&s, 1).instructions.as_str()),
        (
            "Look at it",
            "Line one.
Line two."
        )
    );
    assert_eq!(step(&s, 1).command, "layer.zoomTo", "editing keeps the command");
    assert!(s.run("task.editStep", &json!({"task": "Lesson", "step": 1, "title": " "})).is_err());
    assert!(s.run("task.editStep", &json!({"task": "Lesson", "step": 9, "title": "x"})).is_err());
    s.run("task.moveStep", &json!({"task": "Lesson", "step": 2, "to": 0})).unwrap();
    assert_eq!(step(&s, 0).title, "Clear");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(step(&s, 0).title, "Buffer the cities");
    s.run("task.removeStep", &json!({"task": "Lesson", "step": 2})).unwrap();
    assert_eq!(s.project.tasks.iter().find(|t| t.name == "Lesson").unwrap().steps.len(), 2);
    assert!(s.run("task.moveStep", &json!({"task": "Lesson", "step": 0, "to": 5})).is_err());
    // Neither changes nothing (which would leave an undo step).
    let depth = s.undo_depth();
    assert!(s.run("task.moveStep", &json!({"task": "Lesson", "step": 1, "to": 1})).is_err());
    let bad = s.run("task.removeStep", &json!({"task": "Lesson", "step": -1})).unwrap_err();
    assert!(bad.contains("whole number"), "{bad}");
    assert!(s.run("task.editStep", &json!({"task": "Lesson", "step": 1})).is_err());
    let same = s.project.tasks.iter().find(|t| t.name == "Lesson").unwrap().steps[1].title.clone();
    assert!(s.run("task.editStep", &json!({"task": "Lesson", "step": 1, "title": format!(" {same} ")})).is_err());
    assert_eq!(s.undo_depth(), depth);
    // Still a task from a file: a step that writes a file stays refused after editing.
    assert!(!s.trusted_tasks.contains("lesson"));
}

#[test]
fn tissot_indicatrix_reports_the_maps_own_distortion() {
    let mut s = Session::new(sample::world_project());
    let field = |s: &Session, layer: &str, lat: f64, name: &str| -> Value {
        let ds = &s.map().layer_by_name(layer).unwrap().dataset;
        let col = |n: &str| ds.fields.iter().position(|f| f.name == n).unwrap();
        let row = (0..ds.features.len())
            .find(|&r| {
                ds.value(r, col("lat")) == &rustgis_data::Value::Float(lat) && ds.value(r, col("lon")) == &rustgis_data::Value::Float(0.0)
            })
            .unwrap();
        match ds.value(row, col(name)) {
            rustgis_data::Value::Float(v) => json!(v),
            rustgis_data::Value::Text(t) => json!(t),
            v => panic!("{name}: {v:?}"),
        }
    };
    // Web Mercator, the default: area grows to about sec² 60° = 4, shapes are kept.
    s.run("gp.run", &json!({"tool": "tissot", "params": {"output": "merc"}})).unwrap();
    assert_eq!(field(&s, "merc", 60.0, "projection"), json!("Web Mercator"));
    let area = field(&s, "merc", 60.0, "area_scale").as_f64().unwrap();
    assert!((area - 4.0).abs() < 0.05, "{area}");
    assert!(field(&s, "merc", 60.0, "max_angle").as_f64().unwrap() < 0.5);
    // Mollweide is equal-area: about 1 everywhere, while angles bend away from its centre.
    s.run("map.projection", &json!({"type": "mollweide"})).unwrap();
    s.run("gp.run", &json!({"tool": "tissot", "params": {"output": "moll"}})).unwrap();
    for lat in [-60.0, 0.0, 30.0, 60.0] {
        let area = field(&s, "moll", lat, "area_scale").as_f64().unwrap();
        assert!((area - 1.0).abs() < 0.01, "{lat}: {area}");
    }
    assert!(field(&s, "moll", 60.0, "max_angle").as_f64().unwrap() > 10.0);
    // On the globe there's no flat projection to measure: circles only.
    s.run("map.projection", &json!({"type": "globe"})).unwrap();
    s.run("gp.run", &json!({"tool": "tissot", "params": {"output": "globe"}})).unwrap();
    let fields: Vec<String> = s.map().layer_by_name("globe").unwrap().dataset.fields.iter().map(|f| f.name.clone()).collect();
    assert_eq!(fields, ["lat", "lon"]);
    // Map Extent on the globe fills the whole Earth.
    s.run("gp.run", &json!({"tool": "tissot", "params": {"area": "Map Extent", "spacing": 60, "output": "globeView"}})).unwrap();
    // Six columns of 60° and three rows (−60°, 0°, 60°; the poles' rows are left out).
    assert_eq!(s.map().layer_by_name("globeView").unwrap().dataset.features.len(), 18);
}

/// A5 grids (#551) through gp.run: a world grid of equal-area cells, and the sample's cities
/// counted by cell, every city in one.
#[test]
fn a5_grids_and_points_by_cell() {
    let mut s = world();
    let r = s.run("gp.run", &json!({"tool": "a5Grid", "params": {"resolution": 1, "area": "World", "output": "cells"}})).unwrap();
    let cells = s.map().layer_by_name("cells").unwrap_or_else(|| panic!("{r}"));
    let names: Vec<&str> = cells.dataset.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["a5", "resolution", "area_km2"]);
    assert!(cells.dataset.features.len() > 12);
    let cities = s.map().layer_by_name("Cities").unwrap().dataset.features.len();
    s.run("gp.run", &json!({"tool": "a5Aggregate", "params": {"input": "Cities", "resolution": 2, "output": "by cell"}})).unwrap();
    let by = &s.map().layer_by_name("by cell").unwrap().dataset;
    let i = by.field_index("count").unwrap();
    let total: f64 = (0..by.features.len()).map(|r| by.value(r, i).as_f64().unwrap()).sum();
    assert_eq!(total as usize, cities);
    assert!(s.run("gp.run", &json!({"tool": "a5Grid", "params": {"resolution": 13, "area": "World"}})).is_err());
}

#[test]
fn globe_projection_toggles_renders_and_saves() {
    let mut s = with_raster();
    assert_eq!(s.run("map.projection", &Value::Null).unwrap()["projection"], "globe");
    assert_eq!(s.map().projection, rustgis_map::Projection::Globe);
    // The globe draws vectors, rasters and labels, from any side.
    for (lon, lat, res) in [(-81.0, 42.0, 30_000.0), (100.0, -60.0, 60_000.0), (-81.0, 42.0, 50.0), (0.0, 89.0, 160_000.0)] {
        s.map_mut().camera =
            rustgis_map::Camera { center: rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(lon, lat)), res, bearing: 0.0, pitch: 0.0 };
        let png = crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap();
        assert!(png.len() > 1000);
    }
    assert!(s.run("map.projection", &json!({"type": "nope"})).is_err());
    s.run("map.projection", &json!({"type": "mercator"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().projection, rustgis_map::Projection::Globe, "undo restores the globe");
    let json = serde_json::to_string(&s.project).unwrap();
    assert!(Project::from_json(&json).unwrap().map().projection == rustgis_map::Projection::Globe);
}

/// `map.projection` takes ESRI codes (#213): South America Albers, the issue's regional map.
#[test]
fn esri_projection_codes() {
    let mut s = world();
    s.run("map.goToXY", &json!({"lon": -60.0, "lat": -15.0, "scale": 20_000_000})).unwrap();
    let r = s.run("map.projection", &json!({"type": "esri:102033"})).unwrap();
    assert_eq!(r["projection"], "ESRI:102033");
    assert_eq!(r["label"], "ESRI:102033");
    let c = s.map().center_lonlat();
    assert!((c.x + 60.0).abs() < 1e-6 && (c.y + 15.0).abs() < 1e-6, "{c:?}");
    assert!(crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap().len() > 1000);
    let json = serde_json::to_string(&s.project).unwrap();
    assert_eq!(Project::from_json(&json).unwrap().map().projection.code(), "ESRI:102033");
    // A code the table leaves out is refused, and the map keeps its projection.
    let e = s.run("map.projection", &json!({"type": "ESRI:54003"})).unwrap_err();
    assert!(e.contains("ESRI:54003"), "{e}");
    assert_eq!(s.map().projection.code(), "ESRI:102033");
    // ESRI's Web Mercator is the default map.
    assert_eq!(s.run("map.projection", &json!({"type": "ESRI:102100"})).unwrap()["projection"], "mercator");
}

/// Presets take a central meridian, and the conics their standard parallels too (#517). The map
/// keeps them, and a bad value changes nothing.
#[test]
fn projection_presets_take_parameters() {
    let mut s = world();
    let r = s.run("map.projection", &json!({"type": "robinson", "lon_0": 150})).unwrap();
    assert_eq!(r["projection"], "robinson +lon_0=150");
    assert_eq!(r["label"], "Robinson (lon_0=150)");
    // Without a type, the values go to the map's own preset. A code with them, as typed, is the same.
    s.run("map.projection", &json!({"lon_0": -20})).unwrap();
    assert_eq!(s.map().projection.code(), "robinson +lon_0=-20");
    s.run("map.projection", &json!({"type": "Albers Equal Area Conic +lat_2=-35 +lon_0=-55 +lat_1=-5"})).unwrap();
    assert_eq!(s.map().projection.code(), "albers +lon_0=-55 +lat_1=-5 +lat_2=-35");
    s.run("map.projection", &json!({"lat_1": 0})).unwrap();
    assert_eq!(s.map().projection.code(), "albers +lon_0=-55 +lat_1=0 +lat_2=-35");
    // The preset's own value drops out.
    s.run("map.projection", &json!({"type": "robinson", "lon_0": 0})).unwrap();
    assert_eq!(s.map().projection.code(), "robinson");
    // Out of range, a parameter the preset doesn't take, not a number, parameters on a code or on
    // Web Mercator, and parallels that make no cone: refused, and the map keeps its projection.
    s.run("map.projection", &json!({"type": "robinson", "lon_0": 150})).unwrap();
    for bad in [
        json!({"lon_0": 200}),
        json!({"lat_1": 10}),
        json!({"lon_0": "east"}),
        json!({"type": "EPSG:3035", "lon_0": 10}),
        json!({"type": "mercator", "lon_0": 10}),
        json!({"type": "robinson +lon_0=999"}),
        json!({"type": "albers", "lat_1": -30, "lat_2": 30}),
    ] {
        assert!(s.run("map.projection", &bad).is_err(), "{bad}");
        assert_eq!(s.map().projection.code(), "robinson +lon_0=150", "{bad}");
    }
    // Saved with the map, and drawn.
    let json = serde_json::to_string(&s.project).unwrap();
    assert_eq!(Project::from_json(&json).unwrap().map().projection.code(), "robinson +lon_0=150");
    let png = crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap();
    assert!(png.len() > 1000);
}

#[test]
fn vn2000_projection_command_and_project_persistence() {
    let mut s = world();
    // Explicit prefixed province syntax works and canonicalizes to the preset
    s.run("map.projection", &json!({"type": "vn2000:danang"})).unwrap();
    assert_eq!(s.map().projection.code(), "vn2000tm3 +lon_0=107.75");
    assert_eq!(s.map().projection.label(), "VN-2000 / TM-3 (Vietnam) (lon_0=107.75)");

    // Capital Hanoi on central meridian 105.0 drops redundant parameter
    s.run("map.projection", &json!({"type": "vn2000:hanoi"})).unwrap();
    assert_eq!(s.map().projection.code(), "vn2000tm3");
    assert_eq!(s.map().projection.label(), "VN-2000 / TM-3 (Vietnam)");

    // UTM zones work
    s.run("map.projection", &json!({"type": "vn2000Utm48"})).unwrap();
    assert_eq!(s.map().projection.code(), "vn2000Utm48");
    assert_eq!(s.map().projection.label(), "VN-2000 / UTM zone 48N");

    s.run("map.projection", &json!({"type": "vn2000Utm49"})).unwrap();
    assert_eq!(s.map().projection.code(), "vn2000Utm49");
    assert_eq!(s.map().projection.label(), "VN-2000 / UTM zone 49N");

    // Bare province names and "vietnam" are rejected in strict command context
    for bare in ["danang", "vinh", "hanoi", "hue", "hcm", "vietnam"] {
        assert!(s.run("map.projection", &json!({"type": bare})).is_err(), "{bare}");
    }
    // Ambiguous bare VN-2000 is rejected
    for bare in ["vn2000", "VN-2000", "VN2000"] {
        let err = s.run("map.projection", &json!({"type": bare})).unwrap_err();
        assert!(err.contains("ambiguous"), "{bare}: {err}");
    }

    // Set a provincial TM-3 projection and verify project save and reload
    s.run("map.projection", &json!({"type": "VN-2000 / Đà Nẵng"})).unwrap();
    assert_eq!(s.map().projection.code(), "vn2000tm3 +lon_0=107.75");
    let text = serde_json::to_string(&s.project).unwrap();

    // Reload project from serialized JSON text
    let mut s2 = world();
    s2.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(s2.map().projection.code(), "vn2000tm3 +lon_0=107.75");
    assert_eq!(s2.map().projection.label(), "VN-2000 / TM-3 (Vietnam) (lon_0=107.75)");
}

#[test]
fn custom_projections_switch_keep_scale_query_and_render() {
    let mut s = with_raster();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 5_000_000})).unwrap();
    let before = s.scale();
    let r = s.run("map.projection", &json!({"type": "europeEqualArea"})).unwrap();
    assert_eq!(r["projection"], "europeEqualArea");
    assert!((s.scale() / before - 1.0).abs() < 0.02, "scale kept: {before} vs {}", s.scale());
    let c = s.map().center_lonlat();
    assert!((c.x - 10.0).abs() < 1e-6 && (c.y - 50.0).abs() < 1e-6, "{c:?}");
    // Identify and box select work in the projection's coordinates.
    let v = s.run("identify", &json!({"lon": 2.35, "lat": 48.85, "tolerancePx": 5})).unwrap();
    assert!(!v.is_null(), "something is under Paris");
    let sel = s.run("select.byRect", &json!({"minLon": 5.0, "minLat": 45.0, "maxLon": 15.0, "maxLat": 55.0})).unwrap();
    assert!(sel["selected"].as_u64().unwrap() > 0);
    assert!(crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap().len() > 1000);
    // An EPSG code; the raster (UTM 17N) in the map's own system draws without reprojection.
    s.run("map.projection", &json!({"type": "EPSG:32617"})).unwrap();
    s.run("layer.zoomTo", &json!({"layer": "scene"})).unwrap();
    assert!(crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap().len() > 1000);
    assert!(s.run("map.projection", &json!({"type": "EPSG:4326"})).is_err(), "degrees are refused");
    assert!(s.run("map.projection", &json!({"type": "nope"})).is_err());
    // A known code whose method isn't implemented (Brazil Polyconic) is refused, not drawn in
    // Web Mercator under its own name; the map keeps its projection (#208).
    let e = s.run("map.projection", &json!({"type": "EPSG:5880"})).unwrap_err();
    assert!(e.contains("poly"), "{e}");
    assert_eq!(s.map().projection.code(), "EPSG:32617");
    assert!(s.run("map.goToXY", &json!({"lon": 100.0, "lat": 0.0})).is_err(), "folded far side of the UTM zone");
    // Undo and save/reload keep the projection.
    s.run("map.projection", &json!({"type": "mollweide"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().projection.code(), "EPSG:32617");
    let json = serde_json::to_string(&s.project).unwrap();
    assert_eq!(Project::from_json(&json).unwrap().map().projection.code(), "EPSG:32617");
    assert!(s.run("map.projections", &Value::Null).unwrap().as_array().unwrap().len() >= 10);
    // A project naming a projection this version can't use opens in Web Mercator.
    let broken = json.replace("EPSG:32617", "EPSG:9");
    let reopened = Project::from_json(&broken).unwrap();
    assert_eq!(reopened.map().projection, rustgis_map::Projection::WebMercator);
    assert_eq!(reopened.map().camera, rustgis_map::Camera::world(), "UTM metres aren't read as Mercator ones");
    // Bookmarks follow a projection switch, and the extent history doesn't replay old cameras.
    s.run("map.projection", &json!({"type": "mercator"})).unwrap();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 5_000_000})).unwrap();
    // The sample has a "Europe" already: the new one is numbered (#351), and found by its name.
    let europe = s.run("bookmark.add", &json!({"name": "Europe"})).unwrap()["name"].clone();
    assert_eq!(europe, "Europe 2");
    s.run("map.goToXY", &json!({"lon": -100.0, "lat": 40.0})).unwrap();
    assert!(s.can_extent_back());
    s.run("map.projection", &json!({"type": "europeEqualArea"})).unwrap();
    assert!(!s.can_extent_back(), "history from Web Mercator is dropped");
    s.run("bookmark.go", &json!({"name": europe})).unwrap();
    let c = s.map().center_lonlat();
    assert!((c.x - 10.0).abs() < 1e-6 && (c.y - 50.0).abs() < 1e-6, "{c:?}");
    assert!((s.scale() / 5_000_000.0 - 1.0).abs() < 0.02, "{}", s.scale());
    // An export at another size measures its scale bar with its own camera.
    let wide = s.map().ground_res_at(&rustgis_map::Camera { res: s.map().camera.res * 3.0, ..s.map().camera });
    assert!((wide / s.map().ground_res() - 3.0).abs() < 1e-9);
    // The built-in world projections (and Equal Earth by its EPSG code) draw the whole world.
    for p in ["robinson", "equalEarth", "naturalEarth", "winkelTripel", "eckertIV", "EPSG:8857"] {
        s.run("map.projection", &json!({"type": p})).unwrap_or_else(|e| panic!("{p}: {e}"));
        s.run("map.zoomFull", &Value::Null).unwrap();
        let c = s.map().center_lonlat();
        assert!(c.x.abs() < 1.0 && c.y.abs() < 30.0, "{p}: {c:?}");
        let v = s.run("identify", &json!({"lon": 2.35, "lat": 48.85, "tolerancePx": 5})).unwrap();
        assert!(!v.is_null(), "{p}: something is under Paris");
        assert!(crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap().len() > 1000, "{p}");
    }
}

#[test]
fn raster_layers_open_style_identify_and_round_trip() {
    let mut s = with_raster();
    let l = s.map().layers.last().unwrap();
    assert!(l.is_raster(), "rasters go under the vector layers");
    assert_eq!(l.name, "scene");
    assert_eq!(s.active_layer, Some(l.id));
    let e = l.extent();
    assert!((e.center().x + 81.0).abs() < 0.1 && (e.center().y - 42.0).abs() < 0.1, "{e:?}");
    // Rendering, stretches and enhancement.
    for p in [
        json!({"type": "stretch", "band": 2, "ramp": "Terrain"}),
        json!({"type": "classify", "classes": 4, "method": "quantile"}),
        json!({"type": "unique"}),
        json!({"type": "shadedRelief", "azimuth": 270}),
        json!({"type": "rgb"}),
    ] {
        s.run("raster.symbology", &p).unwrap_or_else(|e| panic!("{p}: {e}"));
    }
    assert!(s.run("raster.symbology", &json!({"type": "colormap"})).is_err());
    assert!(s.run("raster.symbology", &json!({"band": 9})).is_err());
    s.run("raster.bands", &json!({"red": 3, "green": 2, "blue": 1})).unwrap();
    s.run("raster.symbology", &json!({"type": "rgb"})).unwrap(); // keeps the combination
    assert!(s.run("raster.bands", &json!({"preset": "colorInfrared"})).is_err(), "no band 4");
    s.run("raster.stretch", &json!({"type": "standardDeviation", "n": 2.5})).unwrap();
    s.run("raster.dra", &json!({"on": true})).unwrap();
    s.run("raster.resampling", &json!({"type": "bilinear"})).unwrap();
    s.run("raster.enhance", &json!({"brightness": 20, "contrast": -10, "gamma": 1.4})).unwrap();
    s.run("layer.ramp", &json!({"ramp": "Aurora"})).unwrap();
    let st = s.map().layers.last().unwrap().raster.as_ref().unwrap().style.clone();
    assert_eq!(st.renderer, rustgis_map::RasterRenderer::Rgb { red: 2, green: 1, blue: 0 });
    assert!(st.dra && st.gamma == 1.4);
    // Feature-only commands are off for rasters, and layer conditions follow a named layer.
    assert!(s.run("table.open", &Value::Null).is_err());
    assert!(s.run("labels.enable", &json!({"layer": "scene"})).is_err());
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "single"})).unwrap();
    s.active_layer = s.map().layer_by_name("Countries").map(|l| l.id);
    s.run("raster.info", &json!({"layer": "scene"})).unwrap();
    s.active_layer = s.map().layer_by_name("scene").map(|l| l.id);
    // Identify reports features first (Lake Erie is here), then pixel values.
    let v = s.run("identify", &json!({"lon": -81.0, "lat": 42.0})).unwrap();
    assert_eq!(v["layer"], "Lakes", "{v}");
    for l in ["Cities", "Rivers", "Lakes", "Countries"] {
        s.run("layer.visibility", &json!({"layer": l, "visible": false})).unwrap();
    }
    let v = s.run("identify", &json!({"lon": -81.0, "lat": 42.0})).unwrap();
    assert_eq!(v["raster"], true, "{v}");
    assert!(v["attributes"]["Band 1"].is_string());
    let info = s.run("raster.info", &Value::Null).unwrap();
    assert_eq!(info["crs"], "EPSG:32617");
    assert_eq!(info["bands"].as_array().map(Vec::len), Some(3));
    // Export, then reopen from the file.
    let dir = std::env::temp_dir().join(format!("rustgis-raster-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tif = dir.join("out.tif");
    s.run("share.exportRaster", &json!({"path": tif.display().to_string()})).unwrap();
    s.run("layer.addData", &json!({"path": tif.display().to_string()})).unwrap();
    assert!(s.map().layer_by_name("out").is_some_and(|l| l.raster.as_ref().is_some_and(|r| r.path.is_some())));
    // Save and reload the project: the upload is embedded, the file is reopened by path.
    let proj = dir.join("p.rgis");
    s.run("project.saveAs", &json!({"path": proj.display().to_string()})).unwrap();
    s.run("project.open", &json!({"path": proj.display().to_string()})).unwrap();
    for name in ["scene", "out"] {
        let l = s.map().layer_by_name(name).unwrap();
        assert!(l.raster.as_ref().unwrap().data.is_some(), "{name} reloaded");
    }
    assert!(s.map().layer_by_name("scene").unwrap().raster.as_ref().unwrap().style.dra, "style kept");
    // Undo after a style change restores the previous style.
    s.active_layer = s.map().layer_by_name("scene").map(|l| l.id);
    s.run("raster.dra", &json!({"on": false})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layer_by_name("scene").unwrap().raster.as_ref().unwrap().style.dra);
    let _ = std::fs::remove_dir_all(&dir);
    // The map renders with rasters.
    assert!(crate::cmd::share::map_png(&mut s, Some(320.0), Some(200.0), None).is_ok());
}

/// #348: `raster.mask` hides pixel values on the fly: values drawn as no data and a range,
/// kept in the project and undone like any style change.
#[test]
fn raster_masks_hide_values() {
    let mut s = with_raster();
    let style = |s: &Session| s.map().layer_by_name("scene").unwrap().raster.as_ref().unwrap().style.clone();
    let info = s.run("raster.info", &Value::Null).unwrap();
    let (lo, hi) = (info["bands"][0]["min"].as_f64().unwrap(), info["bands"][0]["max"].as_f64().unwrap());
    assert!(info["mask"].is_null());
    s.run("raster.mask", &json!({"values": format!("{lo}, {lo} ; {hi}")})).unwrap();
    assert_eq!(style(&s).mask.values, vec![lo, hi], "text, sorted and without repeats");
    let rl = s.map().layer_by_name("scene").unwrap().raster.clone().unwrap();
    let st = rl.masked_stats().unwrap();
    assert!(st[0].min > lo && st[0].max < hi, "{:?}", st[0]);
    // A range on top, then the other way round; a bound given as null is removed.
    s.run("raster.mask", &json!({"min": lo + 1.0, "max": hi - 1.0})).unwrap();
    s.run("raster.mask", &json!({"inside": true, "max": null})).unwrap();
    let m = style(&s).mask;
    assert_eq!((m.min, m.max, m.inside, m.values.len()), (Some(lo + 1.0), None, true, 2));
    assert!(s.run("raster.info", &Value::Null).unwrap()["mask"].as_str().is_some_and(|t| t.contains('≥')));
    // Refused: a reversed range, values that aren't numbers, too many values.
    assert!(s.run("raster.mask", &json!({"min": 5, "max": 1})).is_err());
    assert!(s.run("raster.mask", &json!({"values": "1, x"})).is_err());
    assert!(s.run("raster.mask", &json!({"values": [1, "nan"]})).is_err());
    assert!(s.run("raster.mask", &json!({"min": "x"})).is_err());
    assert!(s.run("raster.mask", &json!({"values": (0..300).collect::<Vec<_>>()})).is_err());
    assert_eq!(style(&s).mask, m, "nothing changed by a refused mask");
    // Class breaks follow the pixels shown.
    s.run("raster.mask", &json!({"clear": true, "min": lo, "max": (lo + hi) / 2.0})).unwrap();
    s.run("raster.symbology", &json!({"type": "classify", "classes": 4})).unwrap();
    let top = |s: &Session| match style(s).renderer {
        rustgis_map::RasterRenderer::Classify { breaks, .. } => breaks.last().copied().unwrap_or_default(),
        _ => f64::NAN,
    };
    assert!(top(&s) <= (lo + hi) / 2.0, "{}", top(&s));
    s.run("raster.mask", &json!({"max": null})).unwrap();
    assert!(top(&s) > (lo + hi) / 2.0, "worked out again without the bound");
    // Masking down to one value collapses the classes; clearing the mask brings back the 4 asked for.
    let classes = |s: &Session| match style(s).renderer {
        rustgis_map::RasterRenderer::Classify { breaks, .. } => breaks.len(),
        _ => 0,
    };
    s.run("raster.mask", &json!({"min": lo, "max": lo})).unwrap();
    assert_eq!(classes(&s), 1);
    s.run("raster.mask", &json!({"clear": true})).unwrap();
    assert_eq!(classes(&s), 4);
    // Undo the clear and the one-value range.
    s.run("edit.undo", &Value::Null).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    // Breaks edited by hand are kept.
    let mut edited = style(&s);
    if let rustgis_map::RasterRenderer::Classify { breaks, .. } = &mut edited.renderer {
        breaks[0] += 0.5;
    }
    let id = s.map().layer_by_name("scene").unwrap().id;
    s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().style = edited.clone();
    s.run("raster.mask", &json!({"values": [lo]})).unwrap();
    assert_eq!(style(&s).renderer, edited.renderer);
    // Undo that mask.
    s.run("edit.undo", &Value::Null).unwrap();
    // Undo removing the max bound; then saved and drawn.
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(style(&s).mask.max, Some((lo + hi) / 2.0));
    let dir = std::env::temp_dir().join(format!("rustgis-mask-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proj = dir.join("p.rgis");
    s.run("project.saveAs", &json!({"path": proj.display().to_string()})).unwrap();
    s.run("project.open", &json!({"path": proj.display().to_string()})).unwrap();
    assert_eq!(style(&s).mask.max, Some((lo + hi) / 2.0), "kept in the project");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(crate::cmd::share::map_png(&mut s, Some(320.0), Some(200.0), None).is_ok());
    s.active_layer = s.map().layer_by_name("scene").map(|l| l.id);
    s.run("raster.mask", &json!({"clear": true})).unwrap();
    assert!(style(&s).mask.is_empty());
}

/// Every command with junk parameters returns Ok or Err, never panics, and leaves a usable session.
#[test]
fn hostile_params_never_panic() {
    let junk = [
        Value::Null,
        json!({}),
        json!({"layer": 99999, "path": "/nonexistent/x.geojson", "name": "", "where": "((", "tool": "buffer", "params": 7}),
        json!({"layer": "Countries", "row": 1e30, "field": "nope", "value": [1], "scale": -1, "lon": 1e300, "lat": "x", "classes": 1e9, "type": "graduated"}),
        json!({"layer": "Cities", "rows": [1, 99999999], "geometry": {"type": "Polygon", "coordinates": [[]]}, "width": 1e9, "height": -3, "expression": "'"}),
        json!({"band": -1, "red": 1e12, "green": "x", "blue": f64::MAX, "preset": "", "ramp": "nope", "classes": -5, "n": 1e308, "low": -50, "high": 900, "gamma": 0, "zFactor": -1, "fileName": "a.tif", "data": "!!!"}),
        json!({"fileName": "a.tif", "data": "AAAA", "type": "unique", "on": "maybe"}),
        json!({"type": "custom", "min": 1e308, "max": -1e308}),
        json!({"type": "custom", "min": [1e308, "x", null, 4], "max": [null, -1e308, 5]}),
        json!({"type": "custom", "min": [1e308, null, 0], "max": [-1e308, f64::NAN, null], "red": 3}),
        json!({"kind": "colorbar", "ticks": -1e308, "tickSide": 7, "orientation": "x", "layer": "", "reverse": "maybe"}),
        json!({"type": "albers", "lon_0": f64::MAX, "lat_1": "x", "lat_2": -1e308}),
        json!({"kind": "legend", "element": 1, "exclude": [null, 7, "", {"x": 1}]}),
        json!({"kind": "text", "element": 1, "font": {"family": "serif"}, "italic": [], "align": 7}),
        // A STAC trend's source: a valid band and period so the junk after them is reached, and an
        // API on a closed local port, so nothing is read.
        json!({"collection": "c", "asset": "NDVI", "url": "http://127.0.0.1:9/v1", "bbox": "nowhere", "cellSize": -1e308, "resampling": 3, "start": "2020", "end": "2021"}),
        json!({"collection": "c", "asset": "NDVI", "url": "http://127.0.0.1:9/v1", "bbox": [1e308, -91, 7, "x"], "start": "2020", "end": "2021"}),
        // MySQL addresses that can't be read, so nothing connects.
        json!({"database": "mysql://u:p#@[::1/x?ssl-mode=nope", "password": 7, "layer": ":", "crs": "EPSG:-1"}),
        json!({"database": "MARIADB://u@h:99999999/%zz?ssl-mode=VERIFY_CA&ssl-ca=/nonexistent", "sql": "SELECT 1"}),
        json!({"database": "mssql://u:p#@[::1/x?encrypt=nope", "password": 7, "layer": ":", "crs": "EPSG:-1"}),
        json!({"database": "MSSQL://u@h:99999999/%zz?ca=/nonexistent", "sql": "SELECT 1"}),
        // Control points that don't parse, then ones that reach a fit.
        json!({"points": [[], 7, [1, 2, 3, 4, "x"]], "transform": "spline"}),
        json!({"points": [[0, 0, 1e308, -1e308], [1, 0, -1e308, 1e308], [0, 1, 1e308, 1e308], [1, 1, 0, 0, 0]], "transform": "spline"}),
        json!({"points": vec![json!([5, 5, 1, 1]); 1000], "compare": true}),
        // A warp: junk systems, sizes and paths with points that reach the fit.
        json!({"transform": "spline", "crs": "EPSG:999999999", "pointsCrs": [7], "resampling": 1, "cellSize": -1e308, "path": 5, "points": [[0, 0, 1, 1]]}),
        json!({"transform": "affine", "crs": 32613, "pointsCrs": 4326, "cellSize": 1e-300, "path": "/nonexistent/x.tif", "points": [[0, 0, 1, 1], [1, 0, 2, 1], [0, 1, 1, 2]]}),
        json!({"points": [[0, 0, f64::MAX, f64::MAX], [1, 0, -f64::MAX, f64::MAX], [0, 1, f64::MAX, -f64::MAX]], "compare": true}),
        // Connections: junk kinds, options and secrets; no connection exists in a fresh session.
        json!({"id": "c1", "kind": "s3", "address": "http://127.0.0.1:9", "name": "\u{7}", "group": 5, "options": {"credentials": "nope", "buckets": "NO_", "region": [1]}, "password": 7, "accessKeyId": "A", "secretAccessKey": 9}),
        json!({"id": "", "kind": "postgres", "address": "postgres:host=127.0.0.1 port=9 password=x", "options": "x"}),
        json!({"kind": "wms", "address": "http://127.0.0.1:9/wms", "probe": "x", "answer": 7}),
        json!({"kind": "stac", "address": "http://127.0.0.1:9/stac", "answer": "\u{0}{"}),
        json!({"id": "c1", "node": "s3:/\u{7}", "probe": true, "answer": 7, "documents": "x"}),
    ];
    let ids: Vec<&str> = Session::new(Project::default()).registry.commands.iter().map(|c| c.id).collect();
    let streamed = grid_fgb("hostile", 20);
    for id in ids {
        if matches!(
            id,
            "project.save"
                | "project.saveAs"
                | "share.exportMap"
                | "time.export"
                | "share.exportLayer"
                | "share.exportTable"
                | "share.exportRaster"
                // Download from the network.
                | "gp.installGeolibre"
                // Deletes the installed GeoLibre runner.
                | "gp.uninstallGeolibre"
                | "plugin.registry"
                | "plugin.installFromRegistry"
                // Ask Earth Search when no API is given.
                | "stac.collections"
                | "stac.search"
                // Asks geoBoundaries for its list.
                | "boundaries.countries"
                // Asks Source Cooperative for its featured products.
                | "sourcecoop.products"
        ) {
            continue;
        }
        for p in &junk {
            // Places given by name go to the public geocoder: with an answer given, the junk
            // goes through reading it and the options, and nothing is fetched.
            let routed;
            let p = match p {
                Value::Object(o) if id == "map.route" => {
                    let mut o = o.clone();
                    o.insert("answer".into(), json!(r#"{"code":"Ok","routes":[{"legs":[{"steps":[{"geometry":"_ibE_ibE_ibE_ibE","maneuver":{"type":"depart"}}]}]}]}"#));
                    routed = Value::Object(o);
                    &routed
                }
                _ if id == "map.route" => continue,
                p => p,
            };
            let mut s = world();
            let _ = s.run(id, p);
            let _ = s.run("info.project", &Value::Null);
            // Again with a raster layer active.
            let mut s = with_raster();
            let _ = s.run(id, p);
            let _ = s.run("info.project", &Value::Null);
            // And with a streamed layer active.
            let mut s = with_streamed(&streamed);
            let _ = s.run(id, p);
            let _ = s.run("info.project", &Value::Null);
        }
    }
}

#[test]
fn every_tool_survives_an_environment_extent() {
    // An extent holding some features, and one holding none, with every tool and junk params.
    for extent in [json!([0, 40, 20, 55]), json!([-179.9, -89, -179.8, -88.9])] {
        for t in rustgis_analysis::tools().filter(|t| !rustgis_analysis::is_external(t)) {
            let mut s = world();
            s.run("gp.env", &json!({"extent": extent})).unwrap();
            for params in [json!({"input": "Countries"}), json!({"input": "Cities", "distance": 1e9, "clip": "Countries"})] {
                let _ = s.run("gp.run", &json!({"tool": t.id, "params": params}));
            }
            let _ = s.run("info.project", &Value::Null);
        }
    }
    let mut s = world();
    s.run("gp.env", &json!({"extent": [-179.9, -89, -179.8, -88.9]})).unwrap();
    let e = s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 10}})).unwrap_err();
    assert!(e.contains("inside the environment"), "{e}");
}

#[cfg(feature = "geolibre")]
#[test]
fn geolibre_tools_run_on_layers_and_add_their_outputs() {
    let mut s = with_raster();
    let st = s.run("gp.geolibre", &Value::Null).unwrap();
    assert_eq!(st["supported"], true);
    assert!(s.run("gp.setGeolibre", &json!({"path": "/no/such/geolibre"})).is_err(), "not a runner");
    let undo = s.undo_label();
    s.run("gp.setGeolibre", &json!({"path": ""})).unwrap();
    assert_eq!(s.undo_label(), undo, "choosing a runner isn't an edit");
    if st["available"] != true {
        eprintln!("skipped: no GeoLibre runner (set RUSTGIS_GEOLIBRE)");
        return;
    }
    let tools = s.run("gp.tools", &Value::Null).unwrap();
    assert!(tools.as_array().unwrap().iter().any(|t| t["id"] == "geolibre:slope"));
    let before = s.map().layers.len();
    let r = s.run("gp.run", &json!({"tool": "geolibre:slope", "params": {"input": "scene"}})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.map().layers.len(), before + 1);
    let slope = s.map().layer_by_name("Slope").unwrap();
    assert!(slope.is_raster() && slope.raster.as_ref().is_some_and(|r| r.embedded.is_some()), "kept in the project");
    // It saves with the project, and undo takes it away.
    let json = serde_json::to_string(&s.project).unwrap();
    assert!(
        Project::from_json(&json)
            .unwrap()
            .map()
            .layer_by_name("Slope")
            .is_some_and(|l| l.raster.as_ref().is_some_and(|r| r.data.is_some()))
    );
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layer_by_name("Slope").is_none());
    // A vector where a raster belongs is an error, not a crash.
    let err = s.run("gp.run", &json!({"tool": "geolibre:slope", "params": {"input": "Cities"}})).unwrap_err();
    assert!(err.starts_with("Slope:"), "{err}");
}

/// The browser's way: prepare a run, let a runner (here, pretend) produce the files, then
/// apply it with `gp.finish`, which keeps undo and history.
#[cfg(feature = "geolibre")]
#[test]
fn geolibre_runs_can_finish_asynchronously() {
    use crate::cmd::gp::{external_complete, external_start};
    let mut s = with_raster();
    assert!(s.run("gp.finish", &json!({"id": 99})).is_err(), "nothing pending");
    assert!(external_complete(&mut s, 99, Ok((vec![], vec![]))).is_err(), "nothing started");
    if s.run("gp.geolibre", &Value::Null).unwrap()["available"] != true {
        eprintln!("skipped: no GeoLibre runner (set RUSTGIS_GEOLIBRE)");
        return;
    }
    assert!(external_start(&mut s, "geolibre:slope", &json!({})).unwrap_err().contains("required"));
    assert!(external_start(&mut s, "buffer", &json!({})).is_err(), "only GeoLibre tools");
    let run = external_start(&mut s, "geolibre:slope", &json!({"input": "scene"})).unwrap();
    assert_eq!((run.tool.as_str(), run.files.len()), ("slope", 1), "the embedded raster is staged");
    assert!(run.argv.iter().any(|a| a.starts_with("--output=/work/")));
    // The "runner" writes the input back as the output.
    let out = run.files[0].1.clone();
    external_complete(&mut s, run.id, Ok((vec![("output.tif".into(), out)], vec![]))).unwrap();
    let before = s.map().layers.len();
    let r = s.run("gp.finish", &json!({"id": run.id})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.map().layers.len(), before + 1);
    assert!(s.project.gp_history.last().is_some_and(|h| h.ok && h.tool == "Slope"));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().layers.len(), before, "undo removes it");
    assert!(s.run("gp.finish", &json!({"id": run.id})).is_err(), "applied once");
    // A runner error is reported like a failed gp.run.
    let run = external_start(&mut s, "geolibre:slope", &json!({"input": "scene"})).unwrap();
    external_complete(&mut s, run.id, Err("trap".into())).unwrap();
    assert_eq!(s.run("gp.finish", &json!({"id": run.id})).unwrap_err(), "Slope: trap");
    // A run of a project that was closed meanwhile doesn't land in the new one.
    let run = external_start(&mut s, "geolibre:slope", &json!({"input": "scene"})).unwrap();
    s.run("project.new", &Value::Null).unwrap();
    assert!(external_complete(&mut s, run.id, Ok((vec![], vec![]))).unwrap_err().contains("project was closed"));
}

#[test]
fn clipboard_copies_cuts_and_pastes_features() {
    let mut s = world();
    assert!(s.run("edit.copy", &Value::Null).is_err(), "nothing selected");
    s.run("select.byAttributes", &json!({"layer": "Cities", "where": "name = 'Paris'"})).unwrap();
    let r = s.run("edit.copy", &Value::Null).unwrap();
    assert_eq!(r["copied"], 1);
    let text = r["text"].as_str().unwrap().to_string();
    assert!(text.contains("Paris"));
    // Into the same layer, then as a new layer, then from GeoJSON text.
    let n = s.map().layer_by_name("Cities").unwrap().dataset.features.len();
    s.run("edit.paste", &json!({"layer": "Cities"})).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().dataset.features.len(), n + 1);
    assert!(s.run("edit.paste", &json!({"layer": "Countries"})).is_err(), "points don't go in polygons");
    let r = s.run("edit.pasteSpecial", &json!({"name": "Copied"})).unwrap();
    assert_eq!(r["pasted"], 1);
    assert_eq!(s.map().layer_by_name("Copied").unwrap().dataset.features.len(), 1);
    s.run("edit.pasteSpecial", &json!({"text": text, "name": "FromText"})).unwrap();
    assert!(s.map().layer_by_name("FromText").is_some());
    assert!(s.run("edit.paste", &json!({"layer": "Cities", "text": "not json"})).is_err());
    // Cut removes the features.
    s.run("select.byAttributes", &json!({"layer": "Cities", "where": "name = 'Paris'"})).unwrap();
    let cut = s.run("edit.cut", &Value::Null).unwrap();
    assert_eq!(cut["copied"], 2);
    assert_eq!(s.map().layer_by_name("Cities").unwrap().dataset.features.len(), n - 1);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().dataset.features.len(), n + 1);
    // The sample layers live in the project, not in files.
    assert!(s.run("layer.copyPath", &json!({"layer": "Cities"})).is_err());
}

#[test]
fn popup_modes_choose_what_identify_reports() {
    let mut s = world();
    let at = json!({"lon": 2.35, "lat": 48.85, "tolerancePx": 5});
    let top = s.run("identify", &at).unwrap();
    assert_eq!(top["layer"], "Cities");
    s.run("map.popupMode", &json!({"mode": "visible"})).unwrap();
    let all = s.run("identify", &at).unwrap();
    let layers: Vec<&str> = all["results"].as_array().unwrap().iter().map(|r| r["layer"].as_str().unwrap()).collect();
    assert_eq!(layers, ["Cities", "Countries"]);
    s.run("map.popupMode", &json!({"mode": "selectedInContents"})).unwrap();
    s.run("layer.select", &json!({"layer": "Countries"})).unwrap();
    assert_eq!(s.run("identify", &at).unwrap()["layer"], "Countries");
    s.run("map.popupMode", &json!({"mode": "none"})).unwrap();
    assert!(s.run("identify", &at).unwrap().is_null());
    assert!(s.run("map.popupMode", &json!({"mode": "bogus"})).is_err());
    // The mode is saved with the project.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("\"popup_mode\":\"None\""));
}

#[test]
fn identify_reports_every_overlapping_feature() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
    let mut s = world();
    // Two buoys on one spot of the South Atlantic, far from any country, under a float.
    let spot = || Some(Geometry::Point(rustgis_geom::Coord::new(-20.0, -30.0)));
    let mut d = Dataset::new("Buoys", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for n in [1, 2] {
        d.push(spot(), vec![V::Int(n)]);
    }
    let id = u64::from(s.map_mut().add_dataset(d));
    let mut d = Dataset::new("Floats", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    d.push(spot(), vec![V::Int(1)]);
    let float = u64::from(s.map_mut().add_dataset(d));
    let at = json!({"lon": -20.0, "lat": -30.0, "tolerancePx": 5});
    let rows = |v: &Value| {
        v["results"].as_array().unwrap().iter().map(|r| (r["layerId"].as_u64().unwrap(), r["row"].as_u64().unwrap())).collect::<Vec<_>>()
    };
    // Topmost: every feature of the top layer with one; both buoys, the one drawn on top first (#232).
    assert_eq!(rows(&s.run("identify", &at).unwrap()), [(float, 0)]);
    s.run("layer.visibility", &json!({"layer": "Floats", "visible": false})).unwrap();
    let top = s.run("identify", &at).unwrap();
    assert_eq!(rows(&top), [(id, 1), (id, 0)]);
    assert_eq!(top["row"], 1);
    // Each result carries its feature id, to find the feature again after a streamed read.
    assert_eq!(top["results"][0]["fid"], 2);
    assert_eq!(top["results"][1]["fid"], 1);
    s.run("layer.visibility", &json!({"layer": "Floats", "visible": true})).unwrap();
    s.run("map.popupMode", &json!({"mode": "visible"})).unwrap();
    assert_eq!(rows(&s.run("identify", &at).unwrap()), [(float, 0), (id, 1), (id, 0)]);
    // Selectable: every selectable layer, not only the top one.
    s.run("map.popupMode", &json!({"mode": "selectable"})).unwrap();
    assert_eq!(rows(&s.run("identify", &at).unwrap()), [(float, 0), (id, 1), (id, 0)]);
    s.run("layer.selectable", &json!({"layer": "Floats", "on": false})).unwrap();
    assert_eq!(rows(&s.run("identify", &at).unwrap()), [(id, 1), (id, 0)]);
    // A definition query hides one.
    s.run("layer.definitionQuery", &json!({"layer": "Buoys", "where": "n = 1"})).unwrap();
    assert_eq!(rows(&s.run("identify", &at).unwrap()), [(id, 0)]);
}

#[test]
fn label_size_stays_within_what_text_symbol_shows() {
    // Style › Text Symbol offers the range `labels.style` accepts (#457): 72 pt is kept, and
    // anything outside is brought back to the nearest end.
    let mut s = world();
    let size = |s: &Session| s.map().layers.iter().find(|l| l.name == "Countries").map(|l| f64::from(l.labels.default.size));
    for (given, kept) in [(72.0, 72.0), (100.0, 72.0), (1.0, 4.0)] {
        s.run("labels.style", &json!({"layer": "Countries", "size": given})).unwrap();
        assert_eq!(size(&s), Some(kept), "size {given}");
    }
    let range = rustgis_map::symbology::LABEL_SIZE;
    assert_eq!((*range.start(), *range.end()), (4.0, 72.0));
}

#[test]
fn label_range_placement_lock_and_convert() {
    use crate::render::View;
    let mut s = world();
    s.run("labels.enable", &json!({"layer": "Countries", "on": true})).unwrap();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 20_000_000})).unwrap();
    let view = |s: &Session| View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    let count = |s: &mut Session| {
        let v = view(s);
        crate::render::render(s.map_mut(), &v).unwrap().labels.len()
    };
    let placed = count(&mut s);
    assert!(placed > 5);
    // Out beyond 1:5,000,000: no labels at 1:20,000,000.
    s.run("labels.scaleRange", &json!({"layer": "Countries", "min": 5_000_000})).unwrap();
    let n = count(&mut s);
    assert!(n < placed, "{n} < {placed}");
    s.run("labels.scaleRange", &json!({"layer": "Countries", "clear": true})).unwrap();
    assert_eq!(count(&mut s), placed);
    s.run("labels.distanceRange", &json!({"layer": "Countries", "farthest": 1000})).unwrap();
    assert!(count(&mut s) < placed);
    s.run("labels.distanceRange", &json!({"layer": "Countries", "farthest": null})).unwrap();
    assert!(s.run("labels.scaleRange", &json!({"layer": "Countries", "min": -4})).is_err());
    // Pause hides every label; unplaced labels are reported when asked for.
    s.run("labels.pause", &json!({"on": true})).unwrap();
    assert_eq!(count(&mut s), 0);
    s.run("labels.pause", &json!({"on": false})).unwrap();
    s.run("labels.viewUnplaced", &json!({"on": true})).unwrap();
    let v = view(&s);
    let r = crate::render::render(s.map_mut(), &v).unwrap();
    assert!(!r.unplaced.is_empty() && r.unplaced.iter().all(|l| l.color == crate::render::UNPLACED));
    s.run("labels.viewUnplaced", &json!({"on": false})).unwrap();
    // Placement moves labels.
    s.run("labels.placement", &json!({"layer": "Cities", "position": "right"})).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().labels.default.placement, crate::map::LabelPlacement::Right);
    assert!(s.run("labels.placement", &json!({"position": "sideways"})).is_err());
    // Lock keeps the labelled set as the map moves.
    s.run("labels.lock", &json!({"on": true})).unwrap();
    let locked = count(&mut s);
    assert!(s.map().label_lock.is_some());
    s.run("map.zoomOut", &Value::Null).unwrap();
    assert!(count(&mut s) <= locked);
    s.run("labels.lock", &json!({"on": false})).unwrap();
    count(&mut s);
    assert!(s.map().label_lock.is_none());
    // Convert makes an annotation layer and turns the source's labels off.
    let r = s.run("labels.convert", &json!({"layer": "Countries"})).unwrap();
    assert!(r["annotations"].as_u64().unwrap() > 100);
    let a = s.map().layer_by_name("Countries Annotation").unwrap();
    assert!(a.labels.enabled && a.labels.default.field == "TEXT");
    assert!(!s.map().layer_by_name("Countries").unwrap().labels.enabled);
}

/// Label classes by command (#618): added from Default and checked before it, renamed, given
/// a clause, moved, styled and removed; refusals change nothing, and Default stays last.
#[test]
fn label_classes_by_command() {
    use crate::map::LabelPlacement;
    let mut s = world();
    let cities = s.map().layer_by_name("Cities").unwrap().id;
    s.run("layer.select", &json!({"layer": cities})).unwrap();
    let labels = |s: &Session| s.map().layer(cities).unwrap().labels.clone();
    let order = |s: &Session| labels(s).in_order().map(|c| c.name.clone()).collect::<Vec<_>>();
    assert_eq!(s.run("labels.class.add", &json!({"name": "Big", "where": "pop_max > 5000000"})).unwrap()["class"], "Big");
    s.run("labels.class.add", &json!({"name": " Medium ", "where": "pop_max > 1000000"})).unwrap();
    assert_eq!(order(&s), ["Big", "Medium", "Default"]);
    let l = labels(&s);
    assert_eq!((l.classes[0].size, l.classes[0].field.as_str()), (l.default.size, l.default.field.as_str()), "a class starts as Default");
    for bad in [
        json!({"name": "big"}),
        json!({"name": "default"}),
        json!({"name": "  "}),
        json!({"name": "X", "where": "(("}),
        json!({"name": "X", "where": "nope = 1"}),
        json!({"name": "X", "where": 5}),
        json!({"name": "X", "from": "Nope"}),
    ] {
        assert!(s.run("labels.class.add", &bad).is_err(), "{bad}");
    }
    assert!(s.run("labels.class.remove", &json!({"class": "Default"})).is_err(), "Default stays");
    assert!(s.run("labels.class.move", &json!({"class": "Default", "to": 0})).is_err(), "and stays last");
    assert!(s.run("labels.class.rename", &json!({"class": "Big", "name": "medium"})).is_err());
    assert!(s.run("labels.class.update", &json!({"class": "Big", "where": "(("})).is_err());
    assert!(s.run("labels.style", &json!({"class": "Nope", "size": 30})).is_err());
    // A class that isn't a name is refused, not taken for Default.
    for bad in [json!(3), json!("  "), json!(["Big"])] {
        assert!(s.run("labels.style", &json!({"class": bad, "size": 30})).is_err(), "{bad}");
        assert!(s.run("labels.class.add", &json!({"name": "X", "from": bad})).is_err(), "{bad}");
    }
    assert_eq!(labels(&s), l, "refusals change nothing");
    // A field for a class that isn't there turns nothing on; one for a class labels the layer.
    s.run("labels.enable", &json!({"on": false})).unwrap();
    assert!(s.run("labels.field", &json!({"class": "Nope", "field": "name"})).is_err());
    assert!(!labels(&s).enabled);
    s.run("labels.field", &json!({"class": "Big", "field": "name"})).unwrap();
    assert!(labels(&s).enabled && labels(&s).classes[0].field == "name");
    // The labels.* commands change the class named, Default when none is.
    s.run("labels.style", &json!({"class": "big", "size": 20, "bold": true})).unwrap();
    s.run("labels.placement", &json!({"class": "Big", "position": "right"})).unwrap();
    s.run("labels.scaleRange", &json!({"class": "Medium", "min": 10000000})).unwrap();
    s.run("labels.field", &json!({"class": "Medium", "field": "pop_max"})).unwrap();
    let l = labels(&s);
    assert_eq!((l.classes[0].size, l.classes[0].bold, l.classes[0].placement), (20.0, true, LabelPlacement::Right));
    assert_eq!((l.classes[1].min_scale, l.classes[1].field.as_str()), (Some(10_000_000.0), "pop_max"));
    assert!(l.default.size < 20.0 && l.default.field == "name", "Default is left as it was");
    // Renamed (Default too), moved, its clause cleared, hidden; each one undo step.
    s.run("labels.class.rename", &json!({"class": "Medium", "name": "Large"})).unwrap();
    s.run("labels.class.move", &json!({"class": "Large", "to": 0})).unwrap();
    s.run("labels.class.update", &json!({"class": "Big", "where": null, "visible": false})).unwrap();
    s.run("labels.class.rename", &json!({"class": "Default", "name": "Other"})).unwrap();
    assert_eq!(order(&s), ["Large", "Big", "Other"]);
    let l = labels(&s);
    assert_eq!((l.classes[1].where_.as_deref(), l.classes[1].visible), (None, false));
    s.run("edit.undo", &json!(null)).unwrap();
    assert_eq!(order(&s), ["Large", "Big", "Default"]);
    let info = s.run("info.project", &json!(null)).unwrap().to_string();
    assert!(info.contains("labelClasses") && info.contains("Large"), "info.project lists the classes");
    s.run("labels.class.remove", &json!({"class": "big"})).unwrap();
    assert_eq!(order(&s), ["Large", "Default"]);
    // Clear removes the rest, Default kept; again, with none left, it is no error (a script
    // that starts with it runs again).
    assert_eq!(s.run("labels.class.clear", &json!({})).unwrap()["removed"], 1);
    assert_eq!(order(&s), ["Default"]);
    assert_eq!(s.run("labels.class.clear", &json!({})).unwrap()["removed"], 0);
    // info.project lists the classes also for a Default alone that is hidden (or has a clause).
    let listed = |s: &mut Session| {
        let info = s.run("info.project", &json!(null)).unwrap();
        info["layers"].as_array().unwrap().iter().find(|l| l["name"] == "Cities").unwrap()["labelClasses"].clone()
    };
    assert!(listed(&mut s).is_null(), "a plain Default isn't listed");
    s.run("labels.class.update", &json!({"visible": false})).unwrap();
    assert_eq!(listed(&mut s)[0]["visible"], false);
    s.run("labels.class.update", &json!({"visible": true, "where": "pop_max > 0"})).unwrap();
    assert_eq!(listed(&mut s)[0]["where"], "pop_max > 0");
    s.run("labels.class.update", &json!({"where": null})).unwrap();
    // A long name is cut at 100 characters, and doesn't end in the space it was cut at.
    let long = format!("{} tail", "a".repeat(99));
    assert_eq!(s.run("labels.class.add", &json!({"name": long})).unwrap()["class"], "a".repeat(99));
}

/// Converting labels with classes keeps each class's features apart: the annotation has the same
/// classes, picking their points by a CLASS field (#618). Deleting a field a class labels with
/// moves it to another field and keeps the class.
#[test]
fn label_classes_convert_and_outlive_a_deleted_field() {
    let mut s = world();
    let cities = s.map().layer_by_name("Cities").unwrap().id;
    s.run("layer.select", &json!({"layer": cities})).unwrap();
    s.run("labels.enable", &json!({"on": true})).unwrap();
    s.run("labels.class.add", &json!({"name": "Big's", "where": "pop_max > 5000000"})).unwrap();
    s.run("labels.style", &json!({"class": "Big's", "size": 18})).unwrap();
    let big = rustgis_data::query::select("pop_max > 5000000", &s.map().layer(cities).unwrap().dataset).unwrap().len();
    assert!(big > 0);
    let r = s.run("labels.convert", &json!({})).unwrap();
    let a = s.map().layer(u32::try_from(r["layer"].as_u64().unwrap()).unwrap()).unwrap();
    let classes: Vec<_> = a.labels.in_order().map(|c| (c.name.clone(), c.where_.clone(), c.field.clone(), c.size)).collect();
    assert_eq!(classes[0], ("Big's".to_string(), Some(r#""CLASS" = 'Big''s'"#.to_string()), "TEXT".to_string(), 18.0));
    assert_eq!((classes[1].1.as_ref(), classes[1].2.as_str()), (None, "TEXT"), "Default takes the rest");
    let picked = rustgis_data::query::select(classes[0].1.as_deref().unwrap(), &a.dataset).unwrap().len();
    assert_eq!(picked, big, "the class's clause picks the points it labelled");
    // A deleted field: the class labels with Default's field, and is kept.
    s.run("layer.select", &json!({"layer": cities})).unwrap();
    s.run("labels.field", &json!({"class": "Big's", "field": "pop_max"})).unwrap();
    s.run("edit.deleteField", &json!({"layer": cities, "field": "pop_max"})).unwrap();
    let l = &s.map().layer(cities).unwrap().labels;
    assert_eq!(l.classes.len(), 1);
    assert_eq!(l.classes[0].field, l.default.field);
    assert!(l.default.field != "pop_max");
    assert!(l.enabled, "another class's field going leaves the labels on");
    // Default on a deleted field starts again on another, keeping its name, clause and visibility.
    assert_eq!(l.default.field, "name");
    s.run("labels.class.rename", &json!({"class": "Default", "name": "Rest"})).unwrap();
    s.run("labels.class.update", &json!({"where": "name <> 'x'", "visible": false})).unwrap();
    s.run("edit.deleteField", &json!({"layer": cities, "field": "name"})).unwrap();
    let l = &s.map().layer(cities).unwrap().labels;
    assert!(!l.enabled && l.default.field != "name", "Default's field going turns the labels off, as before classes");
    assert_eq!(
        (l.classes.len(), l.classes[0].field.as_str()),
        (1, l.default.field.as_str()),
        "the other classes are kept, on its new field"
    );
    assert_eq!((l.default.name.as_str(), l.default.visible, l.default.where_.as_deref()), ("Rest", false, Some("name <> 'x'")));
}

#[test]
fn graphics_draw_shapes_text_and_pictures() {
    use crate::render::View;
    use base64::Engine;
    let mut s = world();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 20_000_000})).unwrap();
    // The first graphic makes a graphics layer.
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[0, 45], [10, 55]], "fill": "#FF000040"})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).unwrap().graphics);
    for (shape, pts) in [
        ("circle", json!([[5, 50], [8, 50]])),
        ("ellipse", json!([[0, 40], [20, 48]])),
        ("line", json!([[0, 40], [5, 45], [10, 40]])),
        ("curve", json!([[0, 40], [5, 45], [10, 40]])),
        ("freehand", json!([[0, 40], [1, 41], [2, 40]])),
        ("polygon", json!([[0, 40], [5, 45], [10, 40]])),
        ("lasso", json!([[0, 40], [5, 45], [10, 40], [5, 38]])),
        ("point", json!([[2, 48]])),
    ] {
        s.run("graphics.add", &json!({"shape": shape, "points": pts})).unwrap_or_else(|e| panic!("{shape}: {e}"));
    }
    assert!(s.run("graphics.add", &json!({"shape": "polygon", "points": [[0, 0], [1, 1]]})).is_err());
    assert!(s.run("graphics.add", &json!({"shape": "rectangle", "points": [[0, 0], [0, 1]]})).is_err());
    assert!(s.run("graphics.add", &json!({"shape": "text", "points": [[0, 0]]})).is_err(), "text needs text");
    assert!(s.run("graphics.add", &json!({"shape": "blob", "points": [[0, 0]]})).is_err());
    assert!(s.run("graphics.add", &json!({"shape": "point", "points": [[0, 0]], "layer": "Cities"})).is_err(), "not a graphics layer");
    // Dynamic text expands when drawn.
    let text =
        s.run("graphics.add", &json!({"shape": "text", "points": [[10, 50]], "text": "{map} {scale}", "fontSize": 20})).unwrap()["row"]
            .clone();
    let view = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    let r = crate::render::render(s.map_mut(), &view).unwrap();
    assert!(
        r.labels.iter().any(|l| l.text.starts_with("World 1:") && l.size == 20.0),
        "{:?}",
        r.labels.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
    // A picture: a 2×2 PNG.
    let mut png = tiny_skia::Pixmap::new(2, 2).unwrap();
    png.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
    let b64 = base64::engine::general_purpose::STANDARD.encode(png.encode_png().unwrap());
    s.run("graphics.add", &json!({"shape": "picture", "points": [[0, 45], [10, 55]], "image": b64})).unwrap();
    assert!(s.run("graphics.add", &json!({"shape": "picture", "points": [[0, 45], [10, 55]], "image": "bm90IGFuIGltYWdl"})).is_err());
    let v = View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    let r = crate::render::render(s.map_mut(), &v).unwrap();
    let (x, y) = v.to_screen(rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(5.0, 50.0)));
    let px = r.pixmap.pixel(x as u32, y as u32).unwrap();
    assert!(px.red() > 200 && px.green() < 60, "the picture covers its rectangle: {px:?}");
    // Restyle the selection; keep the tool.
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    s.run("graphics.style", &json!({"fill": "#00FF00", "width": 3})).unwrap();
    assert_eq!(rustgis_map::graphics::symbol(&s.map().layer(id).unwrap().dataset, 0).fill, rustgis_map::Rgba::rgb(0, 255, 0));
    assert!(s.run("graphics.style", &json!({})).is_err(), "nothing to change");
    s.run("graphics.style", &json!({"text": "not on a rectangle"})).unwrap();
    assert!(rustgis_map::graphics::label(&s.map().layer(id).unwrap().dataset, 0).is_none(), "text only goes on text graphics");
    // Edit one text graphic by layer and row (#420): the selected rectangle stays as it was, and
    // undo takes the edit back.
    s.run("graphics.edit", &json!({"layer": id, "row": text, "text": "Edited", "fontSize": 30})).unwrap();
    let row = text.as_u64().unwrap() as usize;
    let ds = &s.map().layer(id).unwrap().dataset;
    assert_eq!((rustgis_map::graphics::label(ds, row), rustgis_map::graphics::font_size(ds, row)), (Some("Edited"), 30.0));
    assert_eq!(rustgis_map::graphics::font_size(ds, 0), 14.0, "only the graphic named");
    // A size is kept within the range it is drawn at, so Edit Text shows what the map draws.
    s.run("graphics.edit", &json!({"layer": id, "row": text, "fontSize": 0})).unwrap();
    let ds = &s.map().layer(id).unwrap().dataset;
    assert_eq!(ds.value(row, ds.field_index("font_size").unwrap()), &rustgis_data::Value::Float(*rustgis_map::graphics::FONT_SIZE.start()));
    s.run("edit.undo", &Value::Null).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    let ds = &s.map().layer(id).unwrap().dataset;
    assert_eq!((rustgis_map::graphics::label(ds, row), rustgis_map::graphics::font_size(ds, row)), (Some("{map} {scale}"), 20.0));
    for bad in [
        json!({"layer": id, "row": text, "text": "  "}),
        json!({"layer": id, "row": 999, "text": "x"}),
        json!({"layer": id, "row": 0.5, "text": "x"}),
        json!({"layer": id, "text": "x"}),
        json!({"row": text, "text": "x"}),
        json!({"layer": null, "row": text, "text": "x"}),
        json!({"layer": id, "row": 0, "text": "not on a rectangle"}),
        json!({"layer": id, "row": text, "text": format!("{}x", " ".repeat(2000))}),
        json!({"layer": id, "row": text}),
        json!({"layer": "Cities", "row": 0, "text": "x"}),
    ] {
        assert!(s.run("graphics.edit", &bad).is_err(), "{bad}");
    }
    assert_eq!(rustgis_render::picture_size(&b64), Some((2, 2)));
    assert!(s.run("graphics.keepTool", &Value::Null).unwrap()["keepTool"].as_bool().unwrap());
    s.run("tool.set", &json!({"tool": "graphicRectangle"})).unwrap();
    assert_eq!(s.tool.code(), "graphicRectangle");
    // Graphics round-trip through a project file.
    let text = serde_json::to_string(&s.project).unwrap();
    let p: crate::Project = serde_json::from_str(&text).unwrap();
    assert!(p.map().layer(id).unwrap().graphics);
    s.run("graphics.newLayer", &json!({"name": "Notes"})).unwrap();
    assert!(s.map().layer_by_name("Notes").unwrap().graphics);
}

#[test]
fn select_takes_only_drawn_features_of_selectable_layers() {
    let mut s = world();
    let selected = |s: &Session, layer: &str| s.map().layer_by_name(layer).map(|l| l.selection.clone()).unwrap_or_default();
    // A display filter shows only South America: a box over the world selects only what's drawn (#243).
    let filters = json!({"layer": "Countries", "filters": [{"name": "SA", "where": "continent = 'South America'"}]});
    s.run("layer.displayFilters", &filters).unwrap();
    let all = s.map().layer_by_name("Countries").unwrap().dataset.features.len();
    let r = s.run("select.byRect", &json!({"minLon": -180, "minLat": -85, "maxLon": 180, "maxLat": 85})).unwrap();
    let rows = selected(&s, "Countries");
    assert!(!rows.is_empty() && rows.len() < all, "{} of {all}", rows.len());
    let l = s.map().layer_by_name("Countries").unwrap();
    let continent = l.dataset.field_index("continent").unwrap();
    assert!(rows.iter().all(|r| l.dataset.value(*r, continent).to_string() == "South America"));
    assert!(r["selected"].as_u64().unwrap() >= rows.len() as u64);
    // A click on a hidden country selects nothing; on a drawn one, it.
    let r = s.run("select.atPoint", &json!({"lon": 2.35, "lat": 46.6, "tolerancePx": 0})).unwrap();
    assert!(r.is_null(), "{r}");
    assert!(selected(&s, "Countries").is_empty());
    s.run("select.atPoint", &json!({"lon": -51.0, "lat": -10.0, "tolerancePx": 0})).unwrap();
    assert_eq!(selected(&s, "Countries").len(), 1);
    // A layer that isn't selectable doesn't hide the selectable ones under it: Paris is in Cities,
    // above France in Countries.
    s.run("layer.displayFilters", &json!({"layer": "Countries", "filters": []})).unwrap();
    let paris = json!({"lon": 2.35, "lat": 48.85, "tolerancePx": 5});
    s.run("select.atPoint", &paris).unwrap();
    assert_eq!(selected(&s, "Cities").len(), 1);
    s.run("layer.selectable", &json!({"layer": "Cities", "on": false})).unwrap();
    s.run("select.atPoint", &paris).unwrap();
    assert!(selected(&s, "Cities").is_empty());
    assert_eq!(selected(&s, "Countries").len(), 1);
}

#[test]
fn zooms_to_a_layer_follow_its_definition_query() {
    let mut s = world();
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": "continent = 'South America'"})).unwrap();
    let in_south_america = |c: rustgis_geom::Coord| (-85.0..-30.0).contains(&c.x) && (-60.0..15.0).contains(&c.y);
    // The frame zooms to the countries drawn, not the whole dataset (#206).
    s.run("layout.new", &json!({"page": "a4", "orientation": "portrait", "template": "map"})).unwrap();
    s.run("layout.frameZoom", &json!({"to": "layer", "layer": "Countries"})).unwrap();
    let Some(rustgis_map::layout::ElementKind::MapFrame { center, scale, .. }) =
        s.project.layouts[0].first_frame().and_then(|id| s.project.layouts[0].element(id)).map(|e| e.kind.clone())
    else {
        panic!("no frame")
    };
    assert!(in_south_america(center), "{center:?}");
    assert!(scale < 150_000_000.0, "{scale}");
    // So does the map view.
    s.run("layer.zoomTo", &json!({"layer": "Countries"})).unwrap();
    assert!(in_south_america(s.map().center_lonlat()), "{:?}", s.map().center_lonlat());
    // A query no country passes leaves nothing to zoom to.
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": "continent = 'Atlantis'"})).unwrap();
    assert!(s.run("layer.zoomTo", &json!({"layer": "Countries"})).is_err());
    assert!(s.run("layout.frameZoom", &json!({"to": "layer", "layer": "Countries"})).is_err());
}

#[test]
fn layout_frames_turn_and_tilt() {
    use rustgis_map::layout::ElementKind;
    let mut s = world();
    let frame_of = |s: &Session| {
        let l = &s.project.layouts[0];
        let id = l.first_frame().unwrap_or_default();
        match l.element(id).map(|e| e.kind.clone()) {
            Some(ElementKind::MapFrame { bearing, pitch, .. }) => (id, bearing, pitch),
            _ => panic!("no frame"),
        }
    };
    // A new frame shows the map view as it is, turned and tilted.
    s.run("map.setBearing", &json!({"bearing": 30})).unwrap();
    s.run("map.setPitch", &json!({"pitch": 50})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "orientation": "landscape", "template": "map"})).unwrap();
    let (id, bearing, pitch) = frame_of(&s);
    assert_eq!((bearing, pitch), (30.0, 50.0));
    // Set on the frame, kept in range; a value that isn't a number changes nothing.
    s.run("layout.update", &json!({"element": id, "bearing": 400, "pitch": 200})).unwrap();
    assert_eq!(frame_of(&s), (id, 40.0, 85.0));
    assert!(s.run("layout.update", &json!({"element": id, "pitch": "steep", "bearing": 10})).is_err());
    assert_eq!(frame_of(&s), (id, 40.0, 85.0));
    // Zooming the frame to the view takes the view's bearing and pitch too.
    s.run("map.resetNorth", &json!({})).unwrap();
    s.run("layout.frameZoom", &json!({"to": "view"})).unwrap();
    assert_eq!(frame_of(&s), (id, 0.0, 0.0));
    // A flat frame saves as before; a tilted one draws differently, with its north arrow turned.
    let flat_json = serde_json::to_string(&s.project.layouts[0]).unwrap_or_default();
    assert!(!flat_json.contains("bearing") && !flat_json.contains("pitch"), "{flat_json}");
    s.run("layout.addElement", &json!({"kind": "northArrow", "frame": id})).unwrap();
    let info = rustgis_render::layout::PageInfo::default();
    let draw = |s: &mut Session| {
        rustgis_render::layout::render(&s.project.layouts[0].clone(), &mut s.project.maps, 48.0, &info).map(|p| p.data().to_vec())
    };
    let flat = draw(&mut s).expect("drawn");
    s.run("layout.update", &json!({"element": id, "bearing": 90, "pitch": 60})).unwrap();
    let tilted = draw(&mut s).expect("drawn");
    assert_ne!(flat, tilted);
    let saved = serde_json::to_string(&s.project.layouts[0]).unwrap_or_default();
    assert!(saved.contains("\"bearing\":90") && saved.contains("\"pitch\":60"), "{saved}");
}

#[test]
fn terrain_is_set_from_a_dem_or_tiles() {
    let mut s = world();
    // A source is required, and must be a raster layer or an XYZ URL.
    assert!(s.run("map.terrain", &json!({})).is_err());
    assert!(s.run("map.terrain", &json!({"layer": "Countries"})).is_err());
    assert!(s.run("map.terrain", &json!({"layer": "Nowhere"})).is_err());
    assert!(s.run("map.terrain", &json!({"tiles": "https://example.com/dem.png"})).is_err());
    assert!(s.run("map.terrain", &json!({"tiles": "file:///etc/{z}/{x}/{y}.png"})).is_err());
    assert!(s.run("map.terrain", &json!({"tiles": "https://e.com/{z}/{x}/{y}.png", "encoding": "jpeg"})).is_err());
    assert!(s.map().view.terrain.is_none());
    // Tiles, then only a new exaggeration (kept in range), then off.
    let v = s.run("map.terrain", &json!({"tiles": "https://e.com/{z}/{x}/{y}.png", "encoding": "mapbox", "exaggeration": 2})).unwrap();
    assert_eq!(v["source"]["encoding"], "mapbox");
    assert_eq!(v["exaggeration"], 2.0);
    let v = s.run("map.terrain", &json!({"exaggeration": 400})).unwrap();
    assert_eq!(v["exaggeration"], rustgis_map::view::MAX_EXAGGERATION);
    assert_eq!(v["source"]["tiles"], "https://e.com/{z}/{x}/{y}.png");
    assert!(s.run("map.terrain", &json!({"exaggeration": "steep"})).is_err());
    // Saved with the project.
    let saved = serde_json::to_string(&s.map().view).unwrap_or_default();
    let back: rustgis_map::view::ViewSettings = serde_json::from_str(&saved).unwrap_or_default();
    assert_eq!(back.terrain, s.map().view.terrain);
    s.run("map.terrain", &json!({"off": true})).unwrap();
    assert!(s.map().view.terrain.is_none());
}

#[test]
fn symbology_classes_follow_the_definition_query() {
    let mut s = world();
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": "continent = 'Europe'"})).unwrap();
    let shown = |s: &Session, field: &str| -> Vec<rustgis_data::Value> {
        let l = s.map().layer_by_name("Countries").unwrap();
        let fi = l.dataset.field_index(field).unwrap();
        l.visible_rows().iter().enumerate().filter(|(_, v)| **v).map(|(r, _)| l.dataset.value(r, fi).clone()).collect()
    };
    // The top class ends at the largest country drawn, not at China (#236).
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "graduated", "field": "pop_est", "classes": 5})).unwrap();
    let top = shown(&s, "pop_est").iter().filter_map(|v| v.as_f64()).fold(f64::MIN, f64::max);
    let rustgis_map::Renderer::Graduated { breaks, .. } = &s.map().layer_by_name("Countries").unwrap().renderer else {
        panic!("graduated renderer expected")
    };
    assert_eq!(breaks.last().copied(), Some(top));
    // Unique values list only the values drawn.
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "income_grp"})).unwrap();
    let mut drawn: Vec<String> = shown(&s, "income_grp").iter().map(|v| v.to_string()).collect();
    drawn.sort();
    drawn.dedup();
    let rustgis_map::Renderer::Unique { classes, .. } = &s.map().layer_by_name("Countries").unwrap().renderer else {
        panic!("unique renderer expected")
    };
    assert_eq!(classes.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), drawn);
    // Classes are taken once: changing the query keeps them.
    let before = s.map().layer_by_name("Countries").unwrap().renderer.clone();
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": ""})).unwrap();
    assert_eq!(s.map().layer_by_name("Countries").unwrap().renderer, before);
    // A query no country passes leaves nothing to class.
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": "continent = 'Atlantis'"})).unwrap();
    let err = s.run("layer.symbology", &json!({"layer": "Countries", "type": "graduated", "field": "pop_est"})).unwrap_err();
    assert!(err.contains("definition query"), "{err}");
}

#[test]
fn layer_class_style_edits_one_class() {
    // #314: one class's colour and legend label, by command (the Symbology pane runs the same).
    let mut s = world();
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "income_grp"})).unwrap();
    let renderer = |s: &Session| s.map().layer_by_name("Countries").unwrap().renderer.clone();
    let value = renderer(&s).legend().into_iter().next().unwrap().label;
    // Legend rows are sorted by label (#218), so a renamed class moves: find its row from its
    // value, not its label, so an edit landing on another class can't pass.
    let class = |s: &Session| {
        let r = renderer(s);
        let rustgis_map::Renderer::Unique { classes, .. } = &r else { panic!("unique renderer expected") };
        let i = classes.iter().position(|(k, _)| *k == value).unwrap();
        let row = r.class_order().into_iter().position(|o| o == i).unwrap();
        r.legend().into_iter().nth(row).unwrap()
    };
    let r = s.run("layer.classStyle", &json!({"layer": "Countries", "value": value, "color": "#123456", "label": "Rich"})).unwrap();
    assert_eq!(r["label"], "Rich");
    assert!(r["color"].as_str().is_some_and(|c| c.starts_with("#123456")), "{r}");
    assert_eq!(class(&s).label, "Rich");
    // The renamed class sorts to a new legend row and keeps its own colour there.
    assert!(class(&s).symbol.fill.hex().starts_with("#123456"), "{:?}", class(&s));
    // One undo step, as every command is.
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(class(&s).label, value);
    s.run("edit.redo", &Value::Null).unwrap();
    let edited = class(&s);
    assert_eq!(edited.label, "Rich");
    // Applying the symbology again on the same field keeps the edit; a new scheme recolours
    // every class and keeps the label.
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "income_grp"})).unwrap();
    assert_eq!(class(&s), edited);
    s.run("layer.ramp", &json!({"layer": "Countries", "ramp": "Ember"})).unwrap();
    assert_eq!(class(&s).label, "Rich");
    assert_ne!(class(&s).symbol, edited.symbol);
    // The fallback class.
    s.run("layer.classStyle", &json!({"layer": "Countries", "other": true, "label": "Rest"})).unwrap();
    assert_eq!(renderer(&s).legend().last().map(|e| e.label.clone()).as_deref(), Some("Rest"));
    // Refused: no such class, the wrong way to name one, two at once, or nothing to change.
    for bad in [
        json!({"layer": "Countries", "value": "Atlantis", "color": "#000000"}),
        json!({"layer": "Countries", "index": 0, "color": "#000000"}),
        json!({"layer": "Countries", "value": value, "other": true, "label": "x"}),
        json!({"layer": "Countries", "value": value}),
        json!({"layer": "Countries", "value": value, "color": "red"}),
        json!({"layer": "Countries", "value": value, "color": "#000000", "label": 5}),
        json!({"layer": "Countries", "value": value, "label": null}),
        json!({"layer": "Countries", "value": value, "label": "x", "color": 5}),
        json!({"layer": "Countries", "value": value, "label": "x", "outline": true}),
    ] {
        assert!(s.run("layer.classStyle", &bad).is_err(), "{bad}");
    }
    // A malformed index says what is wrong with it.
    for index in [json!(0.5), json!(-1), json!("first")] {
        let err = s.run("layer.classStyle", &json!({"layer": "Countries", "index": index, "label": "x"})).unwrap_err();
        assert!(err.contains("whole number"), "{index}: {err}");
    }
    // A numeric value names its class as the data prints it, whether sent as an integer or not.
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "pop_est"})).unwrap();
    for v in [json!(211049527), json!(211049527.0)] {
        let r = s.run("layer.classStyle", &json!({"layer": "Countries", "value": v, "label": "Brazil"})).unwrap();
        assert_eq!(r["label"], "Brazil", "{v}");
    }
    // Graduated Colors, by number: the label replaces the range.
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "graduated", "field": "pop_est", "classes": 3})).unwrap();
    s.run("layer.classStyle", &json!({"layer": "Countries", "index": 2, "label": "Most populous"})).unwrap();
    assert_eq!(renderer(&s).legend().get(2).map(|e| e.label.clone()).as_deref(), Some("Most populous"));
    assert!(s.run("layer.classStyle", &json!({"layer": "Countries", "index": 3, "label": "x"})).is_err());
}

/// Read classes by their command identity, not their sorted legend position.
fn class_style_rows(s: &Session, layer: &str) -> Vec<(Value, rustgis_map::Symbol)> {
    match &s.map().layer_by_name(layer).unwrap().renderer {
        rustgis_map::Renderer::Graduated { symbols, .. } => {
            symbols.iter().enumerate().map(|(i, symbol)| (json!({"layer": layer, "index": i}), *symbol)).collect()
        }
        rustgis_map::Renderer::Unique { classes, other, .. } => classes
            .iter()
            .map(|(value, symbol)| (json!({"layer": layer, "value": value}), *symbol))
            .chain(std::iter::once((json!({"layer": layer, "other": true}), *other)))
            .collect(),
        _ => panic!("class renderer expected"),
    }
}

#[test]
fn layer_class_style_polygon_alpha_targets_only_the_named_class() {
    use rustgis_map::Rgba;
    for (kind, field) in [("graduated", "pop_est"), ("unique", "income_grp")] {
        let mut s = world();
        s.run("layer.symbology", &json!({"layer": "Countries", "type": kind, "field": field, "classes": 3})).unwrap();
        let rows = class_style_rows(&s, "Countries");
        // A middle graduated class, a unique value, and the unique fallback all use the same rules.
        let targets = if kind == "unique" { vec![1, rows.len() - 1] } else { vec![1] };
        for target in targets {
            for (color, alpha) in [
                ("#123456", 170),
                (" #123456FF ", 255),
                ("12345600", 0),
                ("#12345640", 64),
                ("#123456", 150),
                ("#123456C8", 200),
                (" 123456 ", 200),
            ] {
                let before = class_style_rows(&s, "Countries");
                let mut params = before[target].0.clone();
                params["color"] = json!(color);
                let result = s.run("layer.classStyle", &params).unwrap();
                let after = class_style_rows(&s, "Countries");
                let fill = Rgba::rgb(0x12, 0x34, 0x56).with_alpha(alpha);
                assert_eq!(after[target].1.fill, fill, "{kind}: {params}");
                assert_eq!(Rgba::from_hex(result["color"].as_str().unwrap()), Some(fill));
                for (i, (old, new)) in before.iter().zip(&after).enumerate() {
                    assert_eq!(new.0, old.0);
                    if i != target {
                        assert_eq!(new.1, old.1, "{kind}: {params} changed class {i}");
                    }
                }
                if alpha == 0 {
                    s.run("edit.undo", &Value::Null).unwrap();
                    assert_eq!(class_style_rows(&s, "Countries"), before);
                    s.run("edit.redo", &Value::Null).unwrap();
                    assert_eq!(class_style_rows(&s, "Countries"), after);
                    let saved = serde_json::to_string(&s.project).unwrap();
                    let reopened = Session::new(Project::from_json(&saved).unwrap());
                    assert_eq!(class_style_rows(&reopened, "Countries"), after);
                }
            }
        }
    }
}

#[test]
fn layer_class_alpha_survives_reclassification_and_ramp() {
    for (kind, field, scheme, next_scheme) in [("graduated", "pop_est", "Ember", "Harbour"), ("unique", "income_grp", "Harbour", "Ember")] {
        let mut s = world();
        let mut symbology = json!({"layer": "Countries", "type": kind, "field": field, "classes": 3, "ramp": scheme});
        s.run("layer.symbology", &symbology).unwrap();
        let original = class_style_rows(&s, "Countries");
        for (i, (mut params, symbol)) in original.iter().cloned().enumerate() {
            // Keep exactly the ramp's RGB: an alpha-only edit must not be mistaken for an untouched fill.
            params["color"] = json!(symbol.fill.with_alpha([0, 64, 255][i % 3]).hex8());
            s.run("layer.classStyle", &params).unwrap();
        }
        let edited = s.map().layer_by_name("Countries").unwrap().renderer.clone();
        let before = class_style_rows(&s, "Countries");
        for (i, ((_, old), (_, new))) in original.iter().zip(&before).enumerate() {
            assert_eq!((new.fill.r(), new.fill.g(), new.fill.b()), (old.fill.r(), old.fill.g(), old.fill.b()));
            assert_eq!(new.fill.a(), [0, 64, 255][i % 3]);
        }
        // Both an omitted scheme and an explicit unchanged scheme are reapplications, not resets.
        let mut same = symbology.clone();
        same.as_object_mut().unwrap().remove("ramp");
        for params in [&same, &symbology] {
            s.run("layer.symbology", params).unwrap();
            assert_eq!(s.map().layer_by_name("Countries").unwrap().renderer, edited, "{kind}");
        }
        // Recolouring through layer.ramp keeps every class's exact alpha, including the fallback's.
        s.run("layer.ramp", &json!({"layer": "Countries", "ramp": next_scheme})).unwrap();
        let recolored = class_style_rows(&s, "Countries");
        for ((old_target, old), (new_target, new)) in before.iter().zip(&recolored) {
            assert_eq!(new_target, old_target);
            assert_eq!(new.fill.a(), old.fill.a(), "{kind}: {new_target}");
        }
        assert!(
            before
                .iter()
                .zip(&recolored)
                .any(|((_, old), (_, new))| { (old.fill.r(), old.fill.g(), old.fill.b()) != (new.fill.r(), new.fill.g(), new.fill.b()) })
        );
        // A different scheme chosen through layer.symbology still resets polygon opacity as before.
        symbology["ramp"] = json!(scheme);
        s.run("layer.symbology", &symbology).unwrap();
        assert!(class_style_rows(&s, "Countries").iter().all(|(_, symbol)| symbol.fill.a() == 170), "{kind}");
    }
}

#[test]
fn layer_class_creation_keeps_explicit_layer_fill_alpha() {
    for (kind, field) in [("graduated", "pop_est"), ("unique", "income_grp")] {
        for alpha in [0, 64, 255] {
            let mut s = world();
            s.run("layer.color", &json!({"layer": "Countries", "color": format!("#336699{alpha:02X}")})).unwrap();
            s.run("layer.symbology", &json!({"layer": "Countries", "type": kind, "field": field, "classes": 3})).unwrap();
            for (target, symbol) in class_style_rows(&s, "Countries") {
                assert_eq!(symbol.fill.a(), alpha, "{kind}: {target}");
            }
        }
    }
}

#[test]
fn layer_class_style_point_and_line_colors_keep_their_supplied_alpha() {
    use rustgis_map::Rgba;
    let mut s = world();
    for layer in ["Cities", "Rivers"] {
        let field = s.map().layer_by_name(layer).unwrap().dataset.fields[0].name.clone();
        s.run("layer.symbology", &json!({"layer": layer, "type": "unique", "field": field})).unwrap();
        let mut params = class_style_rows(&s, layer)[0].0.clone();
        for color in ["#12345600", "#12345640", "#123456FF", "#123456"] {
            let before = class_style_rows(&s, layer);
            params["color"] = json!(color);
            s.run("layer.classStyle", &params).unwrap();
            let after = class_style_rows(&s, layer);
            let fill = Rgba::from_hex(color).unwrap();
            assert_eq!(after[0].1.fill, fill, "{layer}: {color}");
            assert_eq!(after[0].1.stroke, if layer == "Rivers" { fill } else { before[0].1.stroke });
            assert_eq!(&after[1..], &before[1..], "{layer}: other classes are untouched");
        }
    }
}

#[test]
fn layer_class_style_polygon_alpha_reaches_exported_pixels() {
    isolate_plugins();
    let fixture = json!({"type": "FeatureCollection", "features": [{
        "type": "Feature", "properties": {"value": 1},
        "geometry": {"type": "Polygon", "coordinates": [[[-0.25, -0.25], [0.25, -0.25], [0.25, 0.25], [-0.25, 0.25], [-0.25, -0.25]]]}
    }]});
    for kind in ["graduated", "unique"] {
        let mut s = Session::new(Project::default());
        s.run("layer.addDataText", &json!({"fileName": "square.geojson", "text": fixture.to_string()})).unwrap();
        s.run("layer.symbology", &json!({"layer": "square", "type": kind, "field": "value", "classes": 1})).unwrap();
        s.view_size = (128.0, 128.0);
        s.map_mut().basemap = rustgis_map::Basemap::Blank;
        s.map_mut().graticule = false;
        s.map_mut().camera = rustgis_map::Camera { center: rustgis_geom::Coord::new(0.0, 0.0), res: 1000.0, bearing: 0.0, pitch: 0.0 };
        let mut params = class_style_rows(&s, "square")[0].0.clone();
        for alpha in [255, 0, 64] {
            params["color"] = json!(format!("#FF0000{alpha:02X}"));
            s.run("layer.classStyle", &params).unwrap();
            let png = crate::cmd::share::map_png(&mut s, Some(128.0), Some(128.0), None).unwrap();
            let pixels = crate::render::decode_png(&png).unwrap();
            let center = pixels.pixel(64, 64).unwrap();
            let background = pixels.pixel(0, 0).unwrap();
            assert_eq!(center.alpha(), 255, "the export composites onto the blank basemap");
            for (actual, source, behind) in
                [(center.red(), 255, background.red()), (center.green(), 0, background.green()), (center.blue(), 0, background.blue())]
            {
                let expected = (source * alpha + u32::from(behind) * (255 - alpha) + 127) / 255;
                assert!((i64::from(actual) - i64::from(expected)).abs() <= 1, "{kind}, alpha {alpha}: {center:?} over {background:?}");
            }
        }
    }
}

#[test]
fn layout_legends_list_what_the_frame_draws() {
    // #207's repro: classes taken before the query (or from a style) name values no drawn country has.
    let mut s = world();
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "income_grp"})).unwrap();
    s.run("layer.definitionQuery", &json!({"layer": "Countries", "where": "continent = 'South America'"})).unwrap();
    let l = s.map().layer_by_name("Countries").unwrap();
    let rustgis_map::Renderer::Unique { classes, .. } = &l.renderer else { panic!("unique renderer expected") };
    assert!(classes.len() > 2, "{classes:?}");
    let shown: Vec<String> = l.shown_legend(4e7).unwrap().into_iter().map(|e| e.label).collect();
    let mut drawn: Vec<String> = {
        let fi = l.dataset.field_index("income_grp").unwrap();
        l.visible_rows().iter().enumerate().filter(|(_, v)| **v).map(|(r, _)| l.dataset.value(r, fi).to_string()).collect()
    };
    drawn.sort();
    drawn.dedup();
    let mut sorted = shown.clone();
    sorted.sort();
    assert_eq!(sorted, drawn, "only the classes South America has, and no <all other values>");
    assert!(shown.len() < classes.len());
    // The legend element lists what its frame draws unless told to list every class.
    s.run("layout.new", &json!({"page": "a4", "orientation": "portrait", "template": "map"})).unwrap();
    let legend =
        |s: &Session| s.project.layouts[0].elements.iter().find(|e| e.kind.code() == "legend").map(|e| (e.id, e.kind.clone())).unwrap();
    let (id, kind) = legend(&s);
    assert!(matches!(kind, rustgis_map::layout::ElementKind::Legend { all_classes: false, .. }), "{kind:?}");
    s.run("layout.update", &json!({"element": id, "allClasses": true})).unwrap();
    assert!(matches!(legend(&s).1, rustgis_map::layout::ElementKind::Legend { all_classes: true, hide_other: false, .. }));
    // <all other values> can be left out on its own, in either mode.
    s.run("layout.update", &json!({"element": id, "hideOther": true})).unwrap();
    assert!(matches!(legend(&s).1, rustgis_map::layout::ElementKind::Legend { all_classes: true, hide_other: true, .. }));
    // Read like every other on/off parameter (`b`): a value that is neither a bool nor text keeps
    // the setting, and text other than true/yes/1/on turns it off.
    s.run("layout.update", &json!({"element": id, "hideOther": 5})).unwrap();
    assert!(matches!(legend(&s).1, rustgis_map::layout::ElementKind::Legend { hide_other: true, .. }), "a number keeps the setting");
    s.run("layout.update", &json!({"element": id, "hideOther": "off"})).unwrap();
    assert!(matches!(legend(&s).1, rustgis_map::layout::ElementKind::Legend { hide_other: false, .. }));
    let r = s.run("layout.addElement", &json!({"kind": "legend", "allClasses": true})).unwrap();
    let added = r["element"].as_u64().unwrap() as u32;
    let kind = s.project.layouts[0].element(added).map(|e| e.kind.clone()).unwrap();
    assert!(matches!(kind, rustgis_map::layout::ElementKind::Legend { all_classes: true, .. }), "{kind:?}");
    // Layouts saved before the option open with it off.
    let old: rustgis_map::layout::ElementKind = serde_json::from_str(r#"{"Legend":{"frame":1,"title":"L"}}"#).unwrap();
    assert!(matches!(old, rustgis_map::layout::ElementKind::Legend { all_classes: false, hide_other: false, .. }));
}

/// Grids, north arrows and scale bars take their cartographic options through commands (#327):
/// checked before anything changes, kept when other options change, and saved with the layout.
#[test]
fn layout_surrounds_take_cartographic_options() {
    use rustgis_map::Rgba;
    use rustgis_map::layout::{ArrowStyle, CoordFormat, ElementKind, GridEdges, GridStyle, ScaleBarStyle, ScaleUnits};
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4", "orientation": "landscape", "template": "map"})).unwrap();
    let find = |s: &Session, code: &str| s.project.layouts[0].elements.iter().find(|e| e.kind.code() == code).unwrap().clone();
    let frame = find(&s, "mapFrame").id;
    let grid = |s: &Session| match &s.project.layouts[0].element(frame).unwrap().kind {
        ElementKind::MapFrame { grid, .. } => grid.clone(),
        k => panic!("{k:?}"),
    };
    // A grid with ticks along the top and right edges, in degrees and minutes.
    s.run(
        "layout.addElement",
        &json!({"kind": "grid", "step": 0.5, "style": "ticks", "tickLength": 3, "format": "dms", "edges": ["top", "right"], "labelOrientation": "vertical"}),
    )
    .unwrap();
    let g = grid(&s).unwrap();
    assert_eq!((g.step, g.style, g.tick_length, g.format), (0.5, GridStyle::Ticks, 3.0, CoordFormat::Dms));
    assert_eq!(g.edges, GridEdges { top: true, bottom: false, left: false, right: true });
    // Adding it again changes only what is named; a bad value changes nothing.
    s.run("layout.addElement", &json!({"kind": "grid", "width": 1.5, "hemisphere": false})).unwrap();
    let g2 = grid(&s).unwrap();
    assert_eq!((g2.width, g2.hemisphere, g2.style, g2.step), (1.5, false, GridStyle::Ticks, 0.5));
    for bad in [json!({"style": "dotted"}), json!({"format": "utm"}), json!({"edges": ["middle"]}), json!({"labelOrientation": "diagonal"})]
    {
        let mut p = bad.clone();
        p["kind"] = json!("grid");
        assert!(s.run("layout.addElement", &p).is_err(), "{bad}");
        assert!(s.run("layout.update", &json!({"element": frame, "grid": bad})).is_err(), "{bad}");
    }
    assert_eq!(grid(&s).unwrap(), g2);
    // The frame's properties edit, remove and restore its grid; sizes are clamped.
    s.run("layout.update", &json!({"element": frame, "grid": {"style": "linesAndTicks", "tickLength": 1e9, "edges": "bottom, left"}}))
        .unwrap();
    let g3 = grid(&s).unwrap();
    assert_eq!((g3.style, g3.tick_length, g3.edges), (GridStyle::LinesAndTicks, rustgis_map::layout::MAX_TICK_MM, GridEdges::default()));
    s.run("layout.update", &json!({"element": frame, "grid": null})).unwrap();
    assert_eq!(grid(&s), Some(g3), "null is not set: the grid stays");
    s.run("layout.update", &json!({"element": frame, "grid": false})).unwrap();
    assert!(grid(&s).is_none());
    s.run("layout.update", &json!({"element": frame, "grid": true})).unwrap();
    assert_eq!(grid(&s), Some(rustgis_map::layout::Grid::default()));
    let arrow = find(&s, "northArrow").id;
    assert!(s.run("layout.update", &json!({"element": arrow, "grid": true})).is_err(), "only frames have grids");
    // North arrows: a style and colours.
    s.run("layout.update", &json!({"element": arrow, "style": "compass", "fill": "#AA0000"})).unwrap();
    assert!(matches!(
        find(&s, "northArrow").kind,
        ElementKind::NorthArrow { style: ArrowStyle::Compass, fill, outline, .. } if fill == Rgba::rgb(0xAA, 0, 0) && outline == Rgba::rgb(0x22, 0x22, 0x22)
    ));
    assert!(s.run("layout.update", &json!({"element": arrow, "style": "sundial"})).is_err());
    assert!(s.run("layout.update", &json!({"element": arrow, "outline": "red"})).is_err());
    assert!(matches!(find(&s, "northArrow").kind, ElementKind::NorthArrow { style: ArrowStyle::Compass, .. }));
    let star = s.run("layout.addElement", &json!({"kind": "northArrow", "style": "STAR"})).unwrap()["element"].as_u64().unwrap() as u32;
    assert!(matches!(s.project.layouts[0].element(star).unwrap().kind, ElementKind::NorthArrow { style: ArrowStyle::Star, .. }));
    // Scale bars: units, divisions and a style; divisions are clamped.
    let bar = find(&s, "scaleBar").id;
    s.run("layout.update", &json!({"element": bar, "units": "mi", "divisions": 99, "style": "stepped"})).unwrap();
    assert!(matches!(
        find(&s, "scaleBar").kind,
        ElementKind::ScaleBar { units: ScaleUnits::Miles, divisions: 10, style: ScaleBarStyle::Stepped, .. }
    ));
    s.run("layout.update", &json!({"element": bar, "divisions": "2"})).unwrap();
    assert!(matches!(find(&s, "scaleBar").kind, ElementKind::ScaleBar { units: ScaleUnits::Miles, divisions: 2, .. }));
    for bad in [json!({"units": "parsecs"}), json!({"divisions": "many"}), json!({"style": "zigzag"})] {
        let mut p = bad.clone();
        p["element"] = json!(bar);
        assert!(s.run("layout.update", &p).is_err(), "{bad}");
    }
    let added = s.run("layout.addElement", &json!({"kind": "scaleBar", "units": "feet", "style": "line"})).unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    assert!(matches!(
        s.project.layouts[0].element(added).unwrap().kind,
        ElementKind::ScaleBar { units: ScaleUnits::Feet, divisions: 4, style: ScaleBarStyle::Line, .. }
    ));
    // The page draws with them all, and they survive a save.
    let out = std::env::temp_dir().join(format!("rustgis-surrounds-{}.png", std::process::id()));
    s.run("layout.export", &json!({"path": out.to_string_lossy()})).unwrap();
    let _ = std::fs::remove_file(&out);
    let text = serde_json::to_string(&s.project.layouts[0]).unwrap();
    let back: rustgis_map::layout::Layout = serde_json::from_str(&text).unwrap();
    assert_eq!(back, s.project.layouts[0]);
}

/// A colorbar shows a raster's colour scale on a layout (#326): added and edited through
/// commands, checked against the frame's map, and saved with the layout.
#[test]
fn layout_colorbars_follow_a_raster() {
    use rustgis_map::layout::{ElementKind, Orientation, TickSide};
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4", "orientation": "landscape", "template": "map"})).unwrap();
    let kind = |s: &Session, id: u32| s.project.layouts[0].element(id).unwrap().kind.clone();
    // No raster yet: it follows the top visible one once there is one.
    let id = s.run("layout.addElement", &json!({"kind": "colorbar"})).unwrap()["element"].as_u64().unwrap() as u32;
    assert!(matches!(kind(&s, id), ElementKind::Colorbar { layer: None, ticks: 5, reverse: false, .. }), "{:?}", kind(&s, id));
    assert!(s.run("layout.addElement", &json!({"kind": "colorbar", "layer": "Countries"})).is_err(), "a feature layer has no colour scale");
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    let named = s
        .run("layout.addElement", &json!({"kind": "colorbar", "layer": dem.to_uppercase(), "orientation": "vertical", "ticks": 1e9, "tickSide": "left", "reverse": true}))
        .unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    // "" follows the top raster, as in layout.update.
    let blank = s.run("layout.addElement", &json!({"kind": "colorbar", "layer": " "})).unwrap()["element"].as_u64().unwrap() as u32;
    assert!(matches!(kind(&s, blank), ElementKind::Colorbar { layer: None, .. }), "{:?}", kind(&s, blank));
    let e = s.project.layouts[0].element(named).unwrap().clone();
    assert!(e.h > e.w, "a vertical bar is tall: {} × {}", e.w, e.h);
    match &e.kind {
        ElementKind::Colorbar { layer, title, orientation, ticks, tick_side, reverse, .. } => {
            assert_eq!(layer.as_deref(), Some(dem.as_str()), "the layer's own name");
            assert_eq!(title, &dem, "titled after the layer");
            assert_eq!(
                (*orientation, *ticks, *tick_side, *reverse),
                (Orientation::Vertical, rustgis_map::layout::MAX_COLORBAR_TICKS, TickSide::Start, true)
            );
        }
        k => panic!("{k:?}"),
    }
    // Edited: a bad value changes nothing.
    for bad in [json!({"orientation": "diagonal"}), json!({"tickSide": "inside"}), json!({"layer": "Countries"}), json!({"layer": "nope"})]
    {
        let mut p = bad.clone();
        p["element"] = json!(named);
        assert!(s.run("layout.update", &p).is_err(), "{bad}");
    }
    assert_eq!(s.project.layouts[0].element(named).unwrap(), &e);
    // Turned horizontal, it swaps its length and thickness; "" follows the top raster again.
    s.run(
        "layout.update",
        &json!({"element": named, "orientation": "horizontal", "title": "Elevation (m)", "ticks": 3, "tickSide": "below", "layer": ""}),
    )
    .unwrap();
    let turned = s.project.layouts[0].element(named).unwrap().clone();
    assert_eq!((turned.w, turned.h), (e.h, e.w));
    assert!(
        matches!(
            &turned.kind,
            ElementKind::Colorbar { layer: None, orientation: Orientation::Horizontal, ticks: 3, tick_side: TickSide::End, reverse: true, title, .. } if title == "Elevation (m)"
        ),
        "{:?}",
        turned.kind
    );
    // It draws the raster's ramp: the export differs from the page without it.
    let png = |s: &mut Session| crate::cmd::layout::layout_bytes(s, 0, false, 72.0, false).unwrap();
    let with = png(&mut s);
    s.run("layout.update", &json!({"element": named, "visible": false})).unwrap();
    s.run("layout.update", &json!({"element": id, "visible": false})).unwrap();
    assert_ne!(with, png(&mut s));
    // Saved and read back, and layouts from before colorbars had their defaults read as before.
    let text = serde_json::to_string(&s.project.layouts[0]).unwrap();
    let back: rustgis_map::layout::Layout = serde_json::from_str(&text).unwrap();
    assert_eq!(back, s.project.layouts[0]);
    let old: ElementKind = serde_json::from_str(r#"{"Colorbar":{"frame":1}}"#).unwrap();
    assert!(matches!(
        old,
        ElementKind::Colorbar { layer: None, ticks: 5, orientation: Orientation::Horizontal, tick_side: TickSide::End, reverse: false, .. }
    ));
    // Undone with the rest of the layout's edits.
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.layouts[0].element(id).map(|e| e.visible), Some(true), "undo brings it back");
}

/// `layout.publication` (#682): every profile builds a layout that fits its page, the map frame
/// at a round scale that still fits the extent, the footer boxed in two columns below the map
/// with long lines wrapped, the tokens filled from the map; a figure leaves its title to the
/// caption.
#[test]
fn publication_layouts_fit_their_pages() {
    use rustgis_map::layout::ElementKind;
    let dir = std::env::temp_dir().join(format!("rustgis-publication-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = world();
    let piaui = json!([-46.8, -11.6, -38.5, -2.2]);
    let long =
        "NDVI: MOD13Q1 v6.1 (NASA LP DAAC), 16-day composites at 250 m, read through the Brazil Data Cube (INPE) and averaged to 1 km";
    let profiles = [
        "a4-portrait",
        "a4-landscape",
        "a3-portrait",
        "a3-landscape",
        "a2-portrait",
        "a2-landscape",
        "figure-1col",
        "figure-1.5col",
        "figure-2col",
        "thesis-full",
        "thesis-half",
    ];
    for profile in profiles {
        let r = s
            .run(
                "layout.publication",
                &json!({"profile": profile, "extent": piaui, "name": profile, "title": "PIAUÍ", "subtitle": "Greening and browning of vegetation: NDVI trend, 2001–2025",
                        "sources": [long, "Boundaries: IBGE 2022"], "note": "Only cells with a significant trend (p < 0.05) are coloured."}),
            )
            .unwrap_or_else(|e| panic!("{profile}: {e}"));
        let l = s.project.layouts.last().unwrap();
        assert_eq!(r["name"], profile);
        let sheet = profile.starts_with('a');
        for e in &l.elements {
            assert!(
                e.x >= -1e-9 && e.y >= -1e-9 && e.x + e.w <= l.width + 1e-6 && e.y + e.h <= l.height + 1e-6,
                "{profile}: {} at ({}, {}, {}, {}) is off the {} × {} page",
                e.name,
                e.x,
                e.y,
                e.w,
                e.h,
                l.width,
                l.height
            );
        }
        let frame = l.elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap();
        let ElementKind::MapFrame { scale, grid: Some(grid), .. } = &frame.kind else { panic!("{profile}: no grid") };
        // The legend stops short of the scale bar's box, however narrow the frame.
        let named = |n: &str| l.elements.iter().find(|e| e.name == n).unwrap();
        assert!(named("Legend Box").x + named("Legend Box").w <= named("Scale Box").x, "{profile}: the legend reaches the scale bar");
        // A round scale, two significant figures, at which the extent still fits.
        let unit = 10f64.powf(scale.log10().floor() - 1.0);
        assert!(((scale / unit) - (scale / unit).round()).abs() < 1e-6, "{profile}: 1:{scale}");
        let extent = rustgis_geom::Extent::new(-46.8, -11.6, -38.5, -2.2);
        // Fitted within the graticule's label strips (9 mm beside the frame's sides, 6 mm along its
        // top and bottom), so the labels cover none of the extent.
        let (_, fits) = rustgis_map::layout::fit_scale(s.map(), &extent, frame.w - 18.0, frame.h - 12.0).unwrap();
        assert!(fits <= *scale && *scale < fits * 1.1, "{profile}: 1:{scale} for 1:{fits}");
        assert_eq!(grid.step, 2.0, "{profile}: Piauí's 9.4° take a 2° graticule");
        let text = |name: &str| {
            l.elements.iter().find(|e| e.name == name).map(|e| match &e.kind {
                ElementKind::Text { text, .. } => text.clone(),
                _ => panic!("{name} isn't text"),
            })
        };
        if sheet {
            assert_eq!(text("Title").as_deref(), Some("PIAUÍ"), "{profile}");
            // The footer under the map, the long source wrapped within its box.
            let foot = l.elements.iter().find(|e| e.name == "Sources Box").unwrap();
            assert!(foot.y >= frame.y + frame.h, "{profile}: the footer is under the map");
            let sources = l.elements.iter().find(|e| e.name == "Sources").unwrap();
            assert!(sources.x + sources.w <= foot.x + foot.w + 1e-6);
            let lines = text("Sources").unwrap();
            assert!(lines.lines().count() >= 3, "{profile}: {lines}");
            for line in lines.lines() {
                assert!(
                    crate::render::report::width_mm(line, 7.0 * l.width.min(l.height) as f32 / 210.0, false) <= sources.w + 1e-6,
                    "{profile}: {line}"
                );
            }
            // The tokens filled from the map: Web Mercator over WGS 84, the 2° graticule.
            let reference = text("Reference").unwrap();
            assert!(reference.contains("Web Mercator") && reference.contains("WGS 84") && reference.contains("2°"), "{reference}");
            assert!(text("Author").unwrap().contains("{date}"), "the date stays a token the layout fills");
            assert!(r["warnings"].as_array().unwrap().is_empty(), "{profile}: {r}");
        } else {
            assert!(text("Title").is_none() && text("Sources").is_none(), "{profile}: a figure's texts are its caption's");
            assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("caption")), "{r}");
            assert!(text("Reference").unwrap().contains("Datum: WGS 84"), "{profile}");
            assert_eq!(l.width, r["page"][0].as_f64().unwrap());
        }
        let png = dir.join(format!("{profile}.png"));
        s.run("layout.export", &json!({"path": png.display().to_string(), "dpi": 72})).unwrap();
        assert!(std::fs::metadata(&png).unwrap().len() > 1000, "{profile}");
    }
    assert_eq!(s.project.layouts.len(), profiles.len(), "a layout each, the earlier ones untouched");
    // A long title wraps within the frame, and pushes the map down; the result names every
    // element for layout.update.
    let r = s.run("layout.publication", &json!({"extent": piaui, "title": long})).unwrap();
    let l = s.project.layouts.last().unwrap();
    let title = l.elements.iter().find(|e| e.name == "Title").unwrap();
    let ElementKind::Text { text, size, .. } = &title.kind else { panic!() };
    assert!(text.lines().count() >= 2, "{text}");
    for line in text.lines() {
        assert!(crate::render::report::width_mm(line, *size as f32, true) <= title.w, "{line}: measured bold, as drawn");
    }
    let frame = l.elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap();
    assert!(frame.y >= title.y + title.h);
    let listed = r["elements"].as_array().unwrap();
    assert_eq!(listed.len(), l.elements.len());
    assert!(listed.iter().any(|e| e["name"] == "Map Frame" && e["element"] == frame.id && e["kind"] == "mapFrame"), "{r}");
    s.project.layouts.pop();
    // Undone as one command: the layout goes, and the map is as it was (its layers, view and
    // projection), the legend's sync of streamed layers included.
    let (layouts, map) = (s.project.layouts.len(), s.map().clone());
    s.run("layout.publication", &json!({"extent": piaui})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.layouts.len(), layouts);
    let ids = |m: &rustgis_map::Map| m.layers.iter().map(|l| (l.id, l.name.clone(), l.visible)).collect::<Vec<_>>();
    assert_eq!(ids(s.map()), ids(&map));
    assert_eq!((s.map().projection.code(), s.map().camera.center), (map.projection.code(), map.camera.center));
    // A figure says it left out an author and headings given to it, as it does a title.
    let r =
        s.run("layout.publication", &json!({"profile": "figure-1col", "extent": piaui, "author": "Me", "headings": ["A", "B"]})).unwrap();
    let said = r["warnings"].to_string();
    assert!(said.contains("author goes") && said.contains("headings goes"), "{said}");
    s.project.layouts.pop();
    let r = s.run("layout.publication", &json!({"profile": "figure-1col", "extent": piaui, "author": ""})).unwrap();
    assert!(!r["warnings"].to_string().contains("author"), "an empty author asks for none: {r}");
    s.project.layouts.pop();
    // An empty author leaves the authorship out.
    for none in [json!(""), json!([]), json!(["  "])] {
        s.run("layout.publication", &json!({"extent": piaui, "author": none})).unwrap();
        assert!(s.project.layouts.last().unwrap().elements.iter().all(|e| e.name != "Author"), "{none}");
        s.project.layouts.pop();
    }
    // With no sources, the reference alone takes the footer's width; no empty box beside it.
    s.run("layout.publication", &json!({"extent": piaui})).unwrap();
    let l = s.project.layouts.last().unwrap();
    let frame = l.elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap();
    assert!(l.elements.iter().all(|e| !e.name.starts_with("Sources")));
    let reference = l.elements.iter().find(|e| e.name == "Reference Box").unwrap();
    assert!((reference.x - frame.x).abs() < 1e-9 && (reference.w - frame.w).abs() < 1e-9, "{} of {}", reference.w, frame.w);
    s.project.layouts.pop();
    // Headings rename what the columns read, filled and wrapped; their elements keep their names.
    let r = s
        .run(
            "layout.publication",
            &json!({"extent": piaui, "sources": "IBGE", "headings": ["Fontes dos dados, {date}", "Referência cartográfica na projeção {projection}, a mais longa que se possa imaginar"]}),
        )
        .unwrap();
    let l = s.project.layouts.last().unwrap();
    let heading = l.elements.iter().find(|e| e.name == "Reference Heading").unwrap();
    let ElementKind::Text { text, .. } = &heading.kind else { panic!() };
    // Where the wrap breaks depends on the font found, so the words are read across the lines.
    assert!(text.replace('\n', " ").contains("WEB MERCATOR") && !text.contains('{') && text.lines().count() >= 2, "{text}");
    // A live token in a heading keeps its case, so the layout still fills it when it draws.
    let sources = l.elements.iter().find(|e| e.name == "Sources Heading").unwrap();
    let ElementKind::Text { text, .. } = &sources.kind else { panic!() };
    assert_eq!(text, "FONTES DOS DADOS, {date}");
    assert!(l.elements.iter().any(|e| e.name == "Sources Heading") && l.elements.iter().any(|e| e.name == "Reference"), "{r}");
    let body = l.elements.iter().find(|e| e.name == "Reference").unwrap();
    assert!(body.y >= heading.y + heading.h - 1e-9, "the reference starts under its wrapped heading");
    s.project.layouts.pop();
    // A journal figure is as tall as the map's shape asks: Piauí in Web Mercator is taller than wide.
    let one = s.project.layouts.iter().find(|l| l.name == "figure-1col").unwrap();
    assert!(one.width == 90.0 && one.height > 90.0, "{} × {}", one.width, one.height);
    // Graticule steps and round scales.
    use crate::cmd::layout::{capitals, datum_of, grid_step, round_scale, step_text};
    assert_eq!(datum_of("+proj=aea +lat_1=29.5 +datum=NAD83 +units=m").as_deref(), Some("NAD83"));
    assert_eq!(datum_of("+proj=robin +ellps=WGS84").as_deref(), Some("WGS 84"));
    assert_eq!(datum_of("+proj=eqearth +R=6371008.8").as_deref(), Some("a sphere"));
    assert_eq!(datum_of("+proj=moll"), None);
    // A preset on NAD83 says so, though its name holds no datum.
    s.run("map.projection", &json!({"type": "usaAlbers"})).unwrap();
    s.run("layout.publication", &json!({"extent": [-100, 35, -90, 45]})).unwrap();
    let l = s.project.layouts.last().unwrap();
    let ElementKind::Text { text, .. } = &l.elements.iter().find(|e| e.name == "Reference").unwrap().kind else { panic!() };
    assert!(text.contains("Datum: NAD83"), "{text}");
    s.project.layouts.pop();
    assert_eq!(capitals("Fontes, {date} e {pageName}"), "FONTES, {date} E {pageName}");
    assert_eq!(capitals("Dados {2024 e {só} }"), "DADOS {2024 E {SÓ} }", "a lone or odd brace is text");
    assert_eq!(grid_step(&rustgis_geom::Extent::new(10.0, 45.0, 11.0, 46.0)), 0.25);
    assert_eq!(grid_step(&rustgis_geom::Extent::new(-180.0, -90.0, 180.0, 90.0)), 30.0);
    assert_eq!((step_text(2.0), step_text(0.25), step_text(0.02)), ("2°".to_string(), "15′".to_string(), "1′12″".to_string()));
    assert_eq!(step_text(0.01), "36″");
    assert_eq!((round_scale(1_234_567.0), round_scale(4_500_000.0), round_scale(999.0)), (1_300_000.0, 4_500_000.0, 1_000.0));
    // Refused: an unknown profile, a malformed extent, a footer that leaves the map no room.
    for (p, said) in [
        (json!({"profile": "a5-portrait"}), "unknown profile"),
        (json!({"profile": 5}), "profile is"),
        (json!({"extent": [10, 0, 5, 1]}), "extent is"),
        (json!({"extent": [10, 0, 11], "profile": "a4-portrait"}), "extent is"),
        (json!({"sources": vec![long; 40], "profile": "a4-landscape", "extent": piaui}), "no room"),
        (json!({"reference": vec![long; 50], "profile": "thesis-half", "extent": piaui}), "no room"),
    ] {
        let e = s.run("layout.publication", &p).unwrap_err();
        assert!(e.contains(said), "{p}: {e}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn layouts_compose_export_and_round_trip() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-layout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = world();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 20_000_000})).unwrap();
    let r = s.run("layout.new", &json!({"name": "Europe", "page": "a4", "orientation": "landscape"})).unwrap();
    assert_eq!(r["layout"], 0);
    assert_eq!(s.active_layout, Some(0));
    let l = &s.project.layouts[0];
    assert!((l.width - 297.0).abs() < 1e-9 && l.elements.len() == 6, "the map template adds a frame and five surrounds");
    let frame = l.first_frame().unwrap();
    // Frames, surrounds and the rest.
    for shape in ["circle", "ellipse", "polygon", "lasso"] {
        s.run("layout.addFrame", &json!({"shape": shape, "points": [[0, 0], [1, 0], [0.5, 1]]})).unwrap();
    }
    assert!(s.run("layout.addFrame", &json!({"shape": "polygon", "points": [[0, 0], [1, 0]]})).is_err());
    let mut png = tiny_skia::Pixmap::new(4, 4).unwrap();
    png.fill(tiny_skia::Color::from_rgba8(0, 128, 255, 255));
    let b64 = base64::engine::general_purpose::STANDARD.encode(png.encode_png().unwrap());
    for p in [
        json!({"kind": "table", "layer": "Countries", "rows": 5}),
        json!({"kind": "chart", "layer": "Countries", "field": "pop_est", "label": "name"}),
        json!({"kind": "rectangle"}),
        json!({"kind": "dynamicText", "text": "Page {page} of {pages} · {scale}"}),
        json!({"kind": "picture", "image": b64}),
        json!({"kind": "grid", "step": 10}),
    ] {
        s.run("layout.addElement", &p).unwrap_or_else(|e| panic!("{p}: {e}"));
    }
    // A chart moved to a layer without its field charts that layer's first numeric field.
    let chart = s.project.layouts[0].elements.iter().find(|e| e.kind.code() == "chart").unwrap().id;
    s.run("layout.update", &json!({"element": chart, "layer": "Cities"})).unwrap();
    let Some(rustgis_map::layout::ElementKind::Chart { layer, field, .. }) = s.project.layouts[0].element(chart).map(|e| &e.kind) else {
        panic!()
    };
    let cities = &s.map().layer_by_name("Cities").unwrap().dataset;
    assert_eq!(layer, "Cities");
    assert!(cities.field_index(field).is_some_and(|i| cities.fields[i].ty.is_numeric()), "{field}");
    s.run("layout.update", &json!({"element": chart, "layer": "Countries", "field": "pop_est"})).unwrap();
    let inset = s.run("layout.addFrame", &json!({"shape": "rectangle", "x": 200, "y": 150, "w": 60, "h": 40})).unwrap()["element"]
        .as_u64()
        .unwrap();
    s.run("layout.update", &json!({"element": inset, "scale": 200_000_000})).unwrap();
    s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": frame})).unwrap();
    assert!(s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": inset})).is_err());
    // Its colour and width (#213): red and 2 pt by default, set when added or later, and kept
    // through a save; `false` takes it away.
    let indicator = |s: &Session| match s.project.layouts[0].element(inset as u32).map(|e| &e.kind) {
        Some(rustgis_map::layout::ElementKind::MapFrame { extent_of, extent_color, extent_width, .. }) => {
            (*extent_of, extent_color.hex(), *extent_width)
        }
        _ => panic!("the inset is a map frame"),
    };
    assert_eq!(indicator(&s), (Some(frame), "#D01C1C".to_string(), 2.0));
    let saved = serde_json::to_string(&s.project.layouts[0]).unwrap();
    assert!(!saved.contains("extent_color") && !saved.contains("extent_width"), "defaults aren't written");
    s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": frame, "color": "#1F4E9A", "width": 0.75}))
        .unwrap();
    assert_eq!(indicator(&s), (Some(frame), "#1F4E9A".to_string(), 0.75));
    s.run("layout.update", &json!({"element": inset, "extentIndicator": {"width": 99}})).unwrap();
    assert_eq!(indicator(&s).2, rustgis_map::layout::MAX_INDICATOR_WIDTH, "clamped, and the colour kept");
    assert_eq!(indicator(&s).1, "#1F4E9A");
    let saved: rustgis_map::layout::Layout = serde_json::from_str(&serde_json::to_string(&s.project.layouts[0]).unwrap()).unwrap();
    assert!(saved.element(inset as u32).is_some_and(
        |e| matches!(&e.kind, rustgis_map::layout::ElementKind::MapFrame { extent_color, .. } if extent_color.hex() == "#1F4E9A")
    ));
    assert!(s.run("layout.update", &json!({"element": inset, "extentIndicator": {"color": "blue-ish"}})).is_err());
    assert!(s.run("layout.update", &json!({"element": inset, "extentIndicator": 3})).is_err());
    // A value given but not a colour or a number is refused, naming it, and changes nothing.
    for (bad, field) in [(json!({"width": "wide"}), "width"), (json!({"color": 123}), "color")] {
        let err = s.run("layout.update", &json!({"element": inset, "extentIndicator": bad})).unwrap_err();
        assert!(err.starts_with(field), "{err}");
        let mut add = json!({"kind": "extentIndicator", "frame": inset, "of": frame});
        add[field] = bad[field].clone();
        assert!(s.run("layout.addElement", &add).is_err_and(|e| e.starts_with(field)));
    }
    assert_eq!((indicator(&s).1.as_str(), indicator(&s).2), ("#1F4E9A", rustgis_map::layout::MAX_INDICATOR_WIDTH));
    s.run("layout.update", &json!({"element": inset, "extentIndicator": false})).unwrap();
    assert_eq!(indicator(&s).0, None);
    let err = s.run("layout.update", &json!({"element": inset, "extentIndicator": {"color": "#000000"}})).unwrap_err();
    assert!(err.contains("no extent indicator"), "{err}");
    s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": frame})).unwrap();
    assert!(s.run("layout.addElement", &json!({"kind": "sparkles"})).is_err());
    // A hairline rule keeps its height when added and when edited (#211).
    let rule = s.run("layout.addElement", &json!({"kind": "rectangle", "color": "#000000", "w": 136, "h": 0.15})).unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    assert_eq!(s.project.layouts[0].element(rule).map(|e| e.h), Some(0.15));
    s.run("layout.update", &json!({"element": rule, "h": 0.35})).unwrap();
    assert_eq!(s.project.layouts[0].element(rule).map(|e| e.h), Some(0.35));
    let err = s.run("layout.addElement", &json!({"kind": "rectangle", "h": 0})).unwrap_err();
    assert!(err.contains("more than 0"), "{err}");
    // Frame zooms and edits.
    s.run("layout.frameZoom", &json!({"element": frame, "to": "layer", "layer": "Lakes"})).unwrap();
    s.run("layout.update", &json!({"element": frame, "x": 12, "w": 180, "center": [10, 50]})).unwrap();
    assert!(s.run("layout.update", &json!({"element": 9999, "x": 1})).is_err());
    s.run("layout.update", &json!({"element": frame, "name": "Main map"})).unwrap();
    assert!(s.run("layout.update", &json!({"element": frame, "x": 99, "center": "Paris"})).is_err());
    assert!((s.project.layouts[0].element(frame).unwrap().x - 12.0).abs() < 1e-9, "a bad update changes nothing");
    let chart = s.project.layouts[0].elements.iter().find(|e| matches!(e.kind, rustgis_map::layout::ElementKind::Chart { .. })).unwrap().id;
    assert!(s.run("layout.update", &json!({"element": chart, "field": "no_such_field"})).is_err());
    assert!(s.run("layout.update", &json!({"element": chart, "layer": "Atlantis"})).is_err());
    s.run("layout.update", &json!({"element": chart, "field": "gdp_md"})).unwrap();
    assert_eq!(s.project.layouts[0].element(frame).unwrap().name, "Main map");
    // A map series over the lakes: one page each.
    let r = s.run("layout.series", &json!({"layer": "Lakes", "nameField": "name"})).unwrap();
    let pages = r["pages"].as_u64().unwrap();
    assert!(pages > 5);
    let next = s.run("layout.seriesPage", &json!({"page": "next"})).unwrap();
    assert_eq!(next["page"], 1);
    s.run("layout.seriesRefresh", &Value::Null).unwrap();
    // A manual zoom of the series frame survives exports.
    s.run("layout.update", &json!({"element": frame, "scale": 3_000_000})).unwrap();
    let scale_of = |s: &Session| match &s.project.layouts[0].element(frame).unwrap().kind {
        rustgis_map::layout::ElementKind::MapFrame { scale, .. } => *scale,
        _ => 0.0,
    };
    // Export PNG and a PDF of every page.
    let png_path = dir.join("europe.png");
    s.run("layout.export", &json!({"path": png_path.display().to_string(), "dpi": 72})).unwrap();
    let bytes = std::fs::read(&png_path).unwrap();
    assert!(bytes.starts_with(b"\x89PNG"));
    let pdf_path = dir.join("europe.pdf");
    s.run("layout.export", &json!({"path": pdf_path.display().to_string(), "dpi": 36})).unwrap();
    let pdf = std::fs::read(&pdf_path).unwrap();
    let count = format!("/Count {pages}");
    assert!(pdf.windows(count.len()).any(|w| w == count.as_bytes()), "one PDF page per series page");
    assert_eq!(s.project.layouts[0].series.as_ref().unwrap().page, 1, "the shown page is restored");
    assert_eq!(scale_of(&s), 3_000_000.0, "and so is the frame's zoom");
    assert!(s.run("layout.export", &json!({"path": dir.join("x.svg").display().to_string()})).is_err());
    // Layout files.
    let file = dir.join("europe.rgislayout");
    s.run("layout.save", &json!({"path": file.display().to_string()})).unwrap();
    s.run("layout.import", &json!({"path": file.display().to_string()})).unwrap();
    assert_eq!(s.project.layouts.len(), 2);
    assert_eq!(s.project.layouts[1].name, "Europe 2");
    assert!(s.run("layout.import", &json!({"text": "{}"})).is_err());
    // Removing the frame removes its surrounds; undo brings them back.
    let before = s.project.layouts[1].elements.len();
    let first = s.project.layouts[1].first_frame().unwrap();
    s.run("layout.removeElement", &json!({"layout": 1, "element": first})).unwrap();
    assert!(s.project.layouts[1].elements.len() < before - 3);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.layouts[1].elements.len(), before);
    // Layouts are saved with the project.
    let text = serde_json::to_string(&s.project).unwrap();
    let p = crate::Project::from_json(&text).unwrap();
    assert_eq!(p.layouts.len(), 2);
    let r = s.run("layout.rename", &json!({"name": "Europe 2", "newName": "europe"})).unwrap();
    assert_eq!(r["name"], "europe 2", "renaming keeps names unique");
    s.run("layout.rename", &json!({"name": "europe 2", "newName": "Europe 2"})).unwrap();
    s.run("layout.close", &Value::Null).unwrap();
    assert_eq!(s.active_layout, None);
    s.run("layout.remove", &json!({"name": "Europe 2"})).unwrap();
    assert_eq!(s.project.layouts.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn custom_colour_ramps_add_apply_save_and_travel() {
    use rustgis_map::{Ramp, RasterRenderer, Renderer, Rgba};
    let dir = std::env::temp_dir().join(format!("rustgis-ramps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |n: &str| dir.join(n).display().to_string();
    let mut s = world();
    let file = r##"[
        {"name": "Phosphor Frostfire", "kind": "diverging", "stops": ["#eff6ff", "#7b5ca6", "#fff5da"]},
        {"name": "Brand", "kind": "qualitative", "stops": ["#112233", "#445566"]}
    ]"##;
    let r = s.run("ramp.add", &json!({"text": file})).unwrap();
    assert_eq!(r["added"], json!(["Phosphor Frostfire", "Brand"]));
    // One ramp on its own is a file too; the same name replaces it.
    s.run("ramp.add", &json!({"text": r##"{"name": "brand", "kind": "qualitative", "stops": ["#112233", "#445566", "#778899"]}"##}))
        .unwrap();
    assert_eq!(s.project.ramps.len(), 2);
    for bad in
        [r##"{"name": "Ember", "stops": ["#000000", "#ffffff"]}"##, r##"[]"##, r##"{"name": "x", "stops": ["#000000"]}"##, "not json"]
    {
        assert!(s.run("ramp.add", &json!({"text": bad})).is_err(), "{bad}");
    }
    assert_eq!(s.project.ramps.len(), 2, "a refused file adds nothing");
    assert_eq!(s.run("project.connections", &Value::Null).unwrap()["ramps"], json!(["Phosphor Frostfire", "brand"]));

    // Three classes from a three-stop ramp take its colours as they are.
    s.run(
        "layer.symbology",
        &json!({"layer": "Cities", "type": "graduated", "field": "pop_max", "classes": 3, "ramp": "phosphor frostfire"}),
    )
    .unwrap();
    let fills = |s: &Session| match &s.map().layer_by_name("Cities").unwrap().renderer {
        Renderer::Graduated { symbols, ramp, .. } => (symbols.iter().map(|y| y.fill).collect::<Vec<_>>(), ramp.name().to_string()),
        r => panic!("{r:?}"),
    };
    let stops = vec![Rgba::rgb(0xef, 0xf6, 0xff), Rgba::rgb(0x7b, 0x5c, 0xa6), Rgba::rgb(0xff, 0xf5, 0xda)];
    assert_eq!(fills(&s), (stops.clone(), "Phosphor Frostfire".to_string()));
    // Color Scheme recolours with a custom ramp too.
    s.run("layer.ramp", &json!({"layer": "Countries", "ramp": "Brand"})).unwrap();
    assert!(s.run("layer.ramp", &json!({"layer": "Countries", "ramp": "Nope"})).is_err());

    // Saved with the project, and inside the layer's renderer.
    let p = crate::Project::from_json(&serde_json::to_string(&s.project).unwrap()).unwrap();
    assert_eq!(p.ramps, s.project.ramps);
    // A layer file carries the ramp to a project that doesn't have it.
    s.run("layer.save", &json!({"layer": "Cities", "path": path("cities.rgislyr")})).unwrap();
    s.run("ramp.save", &json!({"path": path("mine.rgisramp")})).unwrap();
    s.run("ramp.remove", &json!({"name": "PHOSPHOR FROSTFIRE"})).unwrap();
    assert!(s.run("ramp.remove", &json!({"name": "Phosphor Frostfire"})).is_err());
    assert_eq!(fills(&s).0, stops, "removing a ramp leaves layers that use it alone");
    // ...which can still be restyled with it, by name or by leaving the ramp out.
    let grad = json!({"layer": "Cities", "type": "graduated", "field": "pop_max", "classes": 3, "method": "quantile"});
    let mut named = grad.clone();
    named["ramp"] = json!("Phosphor Frostfire");
    s.run("layer.symbology", &named).unwrap();
    assert_eq!(fills(&s), (stops.clone(), "Phosphor Frostfire".to_string()));
    s.run("layer.symbology", &grad).unwrap();
    assert_eq!(fills(&s).1, "Phosphor Frostfire", "a ramp left out is kept");
    named["ramp"] = json!("Nope");
    assert!(s.run("layer.symbology", &named).is_err(), "an unknown ramp is an error");
    // A hand-edited project's repeated or built-in names are made unique, keeping every ramp.
    let mut p = s.project.clone();
    p.ramps = serde_json::from_str(
        r##"[{"name": "Brand", "stops": ["#000000", "#ffffff"]}, {"name": "brand", "stops": ["#111111", "#ffffff"]},
            {"name": "Ember", "stops": ["#222222", "#ffffff"]}]"##,
    )
    .unwrap();
    p.normalize();
    assert_eq!(p.ramps.iter().map(|r| r.name()).collect::<Vec<_>>(), ["Brand", "brand 2", "Ember (custom)"]);
    let mut t = world();
    t.run("layer.addData", &json!({"path": path("cities.rgislyr")})).unwrap();
    match &t.map().layer_by_name("Cities (2)").unwrap().renderer {
        Renderer::Graduated { symbols, ramp: Ramp::Custom(c), .. } => {
            assert_eq!(c.name(), "Phosphor Frostfire");
            assert_eq!(symbols.iter().map(|y| y.fill).collect::<Vec<_>>(), stops);
        }
        r => panic!("{r:?}"),
    }
    // The saved file adds them back.
    t.run("ramp.add", &json!({"path": path("mine.rgisramp")})).unwrap();
    assert_eq!(t.project.ramps.len(), 2);
    assert!(Session::new(Project::default()).run("ramp.save", &json!({"path": path("none.rgisramp")})).is_err());

    // Rasters: stretch and classify with a custom ramp.
    let mut r = with_raster();
    r.run("ramp.add", &json!({"path": path("mine.rgisramp")})).unwrap();
    r.run("raster.symbology", &json!({"type": "stretch", "ramp": "Phosphor Frostfire"})).unwrap();
    let style = |r: &Session| r.map().layers.last().unwrap().raster.as_ref().unwrap().style.renderer.clone();
    assert!(matches!(style(&r), RasterRenderer::Stretch { ramp: Ramp::Custom(_), .. }));
    r.run("raster.symbology", &json!({"type": "classify", "classes": 3})).unwrap();
    assert!(matches!(style(&r), RasterRenderer::Classify { ramp: Ramp::Custom(_), .. }), "classify keeps the ramp");
    r.run("ramp.remove", &json!({"name": "Phosphor Frostfire"})).unwrap();
    r.run("raster.symbology", &json!({"type": "classify", "classes": 4, "ramp": "Phosphor Frostfire"})).unwrap();
    assert!(matches!(style(&r), RasterRenderer::Classify { ramp: Ramp::Custom(_), .. }), "the raster's own copy is found by name");
    assert!(r.run("raster.symbology", &json!({"type": "stretch", "ramp": "Nope"})).is_err());
    assert!(crate::cmd::share::map_png(&mut r, Some(200.0), Some(150.0), None).unwrap().len() > 1000);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn colour_ramps_from_qgis_gdal_and_gmt_files() {
    use rustgis_map::color::RampKind;
    let dir = std::env::temp_dir().join(format!("rustgis-palettes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |n: &str| dir.join(n).display().to_string();
    let mut s = world();
    // A GMT palette by path: named after the file, with the kind given.
    std::fs::write(path("relief.cpt"), "-100 0/0/255 0 255/255/255\n0 255/255/255 100 255/0/0\n").unwrap();
    let r = s.run("ramp.add", &json!({"path": path("relief.cpt"), "kind": "diverging"})).unwrap();
    assert_eq!(r["added"], json!(["relief"]));
    assert_eq!(s.project.ramps[0].kind(), RampKind::Diverging);
    // GDAL colour-relief text, as the web app passes a picked file: its bytes and name.
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode("0, 0, 0, 0, 255\n10, 255, 255, 255, 255\nnv, 0, 0, 0, 0\n");
    let r = s.run("ramp.add", &json!({"data": data, "file": "Grey.txt"})).unwrap();
    assert_eq!(r["added"], json!(["Grey"]));
    // A QGIS style as text, with the format named.
    let xml = r#"<!DOCTYPE qgis_style><qgis_style version="2"><colorramps><colorramp type="gradient" name="Q"><Option type="Map">
        <Option type="QString" name="color1" value="0,0,0,255"/><Option type="QString" name="color2" value="255,255,255,255"/>
        </Option></colorramp></colorramps></qgis_style>"#;
    s.run("ramp.add", &json!({"text": xml, "format": "qgis", "name": "From QGIS"})).unwrap();
    assert_eq!(s.project.ramps.iter().map(|r| r.name()).collect::<Vec<_>>(), ["relief", "Grey", "From QGIS"]);
    s.run("layer.ramp", &json!({"layer": "Countries", "ramp": "relief"})).unwrap();
    for bad in [
        json!({"text": xml, "format": "svg"}),
        json!({"text": xml, "kind": "round"}),
        json!({"text": r##"{"name": "Ember", "stops": ["#000000", "#ffffff"]}"##}),
    ] {
        assert!(s.run("ramp.add", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_items_files_styles_packages_tasks_and_3d() {
    let dir = std::env::temp_dir().join(format!("rustgis-items-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |n: &str| dir.join(n).display().to_string();
    let mut s = world();
    // Folder connections.
    s.run("project.addFolder", &json!({"path": dir.display().to_string()})).unwrap();
    assert!(s.run("project.addFolder", &json!({"path": path("missing")})).is_err());
    assert_eq!(s.run("project.connections", &Value::Null).unwrap()["folders"][0]["available"], true);
    // Map and layer files.
    s.run("map.save", &json!({"path": path("world.rgismap")})).unwrap();
    s.run("map.import", &json!({"path": path("world.rgismap")})).unwrap();
    assert_eq!(s.project.maps.len(), 2);
    assert_eq!(s.project.maps[1].name, "World (2)");
    s.run("layer.symbology", &json!({"layer": "Cities", "type": "single", "color": "#AA00FF"})).ok();
    s.run("layer.save", &json!({"layer": "Cities", "path": path("cities.rgislyr")})).unwrap();
    let n = s.map().layers.len();
    s.run("layer.addData", &json!({"path": path("cities.rgislyr")})).unwrap();
    assert_eq!(s.map().layers.len(), n + 1);
    assert!(s.map().layer_by_name("Cities (2)").is_some());
    // Styles.
    s.run("style.new", &json!({"layer": "Countries", "name": "Choropleth"})).unwrap();
    s.run("style.save", &json!({"path": path("styles.rgisstyle")})).unwrap();
    s.run("style.remove", &json!({"name": "Choropleth"})).unwrap();
    s.run("style.add", &json!({"path": path("styles.rgisstyle")})).unwrap();
    assert!(s.run("style.apply", &json!({"name": "Choropleth", "layer": "Cities"})).is_err(), "polygon style on points");
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "single"})).unwrap();
    s.run("style.apply", &json!({"name": "Choropleth", "layer": "Countries"})).unwrap();
    assert_eq!(s.map().layer_by_name("Countries").unwrap().renderer.kind_label(), "Graduated Colors");
    let r = s.run("style.import", &json!({"path": path("world.rgismap")})).unwrap();
    assert!(r["imported"].as_u64().unwrap() >= 4);
    // Templates keep the setup, not the features.
    s.run("project.saveTemplate", &json!({"path": path("atlas.rgistemplate")})).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.fromTemplate", &json!({"path": path("atlas.rgistemplate"), "name": "From template"})).unwrap();
    assert_eq!(t.project.name, "From template");
    assert!(t.map().layer_by_name("Countries").unwrap().dataset.features.is_empty());
    assert!(!t.project.styles.is_empty());
    // Packages open again.
    s.run("project.package", &json!({"path": path("atlas.rgispkg")})).unwrap();
    let mut o = Session::new(Project::default());
    o.run("project.open", &json!({"path": path("atlas.rgispkg")})).unwrap();
    assert_eq!(o.project.maps.len(), 2);
    assert!(o.path.is_none(), "a package opens unsaved");
    // The desktop app's command line and a dropped file open it as a project too (#411).
    let pkg = path("atlas.rgispkg");
    assert_eq!(crate::project::open_command(&pkg), "project.open");
    let mut d = Session::new(Project::default());
    d.run(crate::project::open_command(&pkg), &json!({"path": pkg})).unwrap();
    assert_eq!(d.project.maps.len(), 2);
    assert!(d.path.is_none(), "a package opens unsaved, so Save asks where to write a project");
    s.run("map.package", &json!({"path": path("world.rgismpk"), "mobile": true, "tolerance": 0.05})).unwrap();
    s.run("map.import", &json!({"path": path("world.rgismpk")})).unwrap();
    let full: usize = s.project.maps[0]
        .layer_by_name("Countries")
        .unwrap()
        .dataset
        .features
        .iter()
        .filter_map(|f| f.geometry.as_ref())
        .map(|g| g.coord_count())
        .sum();
    let mobile: usize = s
        .project
        .maps
        .last()
        .unwrap()
        .layer_by_name("Countries")
        .unwrap()
        .dataset
        .features
        .iter()
        .filter_map(|f| f.geometry.as_ref())
        .map(|g| g.coord_count())
        .sum();
    assert!(mobile < full, "mobile packages simplify: {mobile} < {full}");
    assert!(s.run("gp.package", &json!({"path": path("results.rgisgpk")})).is_err(), "no runs yet");
    s.run("map.activate", &json!({"name": "World"})).unwrap();
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100}})).unwrap();
    s.run("gp.package", &json!({"path": path("results.rgisgpk")})).unwrap();
    let entries = rustgis_data::zip::read(&std::fs::read(path("results.rgisgpk")).unwrap()).unwrap();
    assert!(entries.iter().any(|(n, _)| n == "history.json") && entries.iter().any(|(n, _)| n.starts_with("results/")));
    // Layer templates.
    let id = s.run("layer.template", &json!({"template": "places"})).unwrap()["layer"].as_u64().unwrap() as u32;
    assert_eq!(s.map().layer(id).unwrap().dataset.fields.len(), 4);
    assert!(s.run("layer.template", &json!({"template": "unicorns"})).is_err());
    // Tasks.
    s.run("task.new", &json!({"name": "Explore", "steps": [{"title": "Zoom to Europe", "command": "map.goToXY", "params": {"lon": 10, "lat": 50, "scale": 20000000}}, {"title": "Select big countries", "command": "select.byAttributes", "params": {"layer": "Countries", "where": "pop_est > 50000000"}}]})).unwrap();
    assert!(s.run("task.new", &json!({"steps": [{"title": "x", "command": "no.such"}]})).is_err());
    let r = s.run("task.run", &json!({"task": "Explore"})).unwrap();
    assert_eq!(r["ran"].as_array().unwrap().len(), 2);
    assert!(s.map().selection_count() > 5);
    s.run("task.addStep", &json!({"task": "Explore", "title": "Clear", "command": "select.clear"})).unwrap();
    s.run("task.run", &json!({"task": "Explore", "step": 2})).unwrap();
    assert_eq!(s.map().selection_count(), 0);
    s.run("task.new", &json!({"name": "Replay", "fromHistory": true})).unwrap();
    assert!(s.trusted_tasks.contains("replay"), "history run in this session");
    // History that came with an opened project is someone else's.
    s.history_from_file = 1;
    s.run("task.new", &json!({"name": "Replay file", "fromHistory": true})).unwrap();
    assert!(!s.trusted_tasks.contains("replay file"));
    s.history_from_file = 0;
    assert_eq!(s.project.tasks.last().unwrap().steps.len(), 1);
    s.run("task.save", &json!({"task": "Explore", "path": path("explore.rgistask")})).unwrap();
    s.run("task.import", &json!({"path": path("explore.rgistask")})).unwrap();
    assert_eq!(s.project.tasks.last().unwrap().name, "Explore 2");
    // A task file may not touch files or run scripts.
    let sneaky =
        json!({"name": "Sneaky", "steps": [{"title": "Overwrite", "command": "share.exportMap", "params": {"path": path("x.png")}}]});
    assert!(s.run("task.import", &json!({"text": sneaky.to_string()})).is_err());
    let nested = json!({"name": "Nested", "steps": [{"title": "Run", "command": "script.run", "params": {"script": "map.zoomFull"}}]});
    assert!(s.run("task.import", &json!({"text": nested.to_string()})).is_err());
    // A task arriving inside a project file is vetted when it runs.
    s.project.tasks.push(crate::project::Task {
        name: "From a file".into(),
        description: String::new(),
        steps: vec![crate::project::TaskStep {
            title: "Export".into(),
            instructions: String::new(),
            command: "share.exportMap".into(),
            params: json!({"path": path("y.png")}),
        }],
    });
    assert!(s.run("task.run", &json!({"task": "From a file"})).is_err());
    assert!(!std::path::Path::new(&path("y.png")).exists());
    // A step naming a feature layer runs even when a raster is selected.
    s.run("task.new", &json!({"name": "Layered", "steps": [{"title": "Zoom", "command": "layer.zoomTo", "params": {"layer": "Lakes"}}]}))
        .unwrap();
    s.run("task.run", &json!({"task": "Layered"})).unwrap();
    // 3D export: polygons extruded by a field.
    s.run("share.export3d", &json!({"layer": "Countries", "path": path("countries.obj"), "heightField": "pop_est", "zScale": 0.00001}))
        .unwrap();
    let obj = std::fs::read_to_string(path("countries.obj")).unwrap();
    assert!(obj.lines().filter(|l| l.starts_with("v ")).count() > 1000 && obj.contains("\nf "));
    // Caps are triangles, and every face refers to a written vertex.
    let verts = obj.lines().filter(|l| l.starts_with("v ")).count();
    assert!(obj.lines().filter(|l| l.starts_with("f ")).all(|l| {
        let ids: Vec<usize> = l[2..].split_whitespace().map(|x| x.parse().unwrap()).collect();
        (3..=4).contains(&ids.len()) && ids.iter().all(|i| (1..=verts).contains(i))
    }));
    let (name, bytes) = crate::cmd::items::item_bytes(&mut s, "obj", &json!({"layer": "Cities"})).unwrap();
    assert!(name.ends_with(".obj") && !bytes.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn data_design_rules_hold_on_every_edit() {
    let mut s = world();
    s.run("layer.template", &json!({"template": "routes", "name": "Roads"})).unwrap();
    s.run("edit.addField", &json!({"layer": "Roads", "name": "kind", "type": "long"})).unwrap();
    s.run("edit.addField", &json!({"layer": "Roads", "name": "lanes", "type": "long"})).unwrap();
    // Domains.
    s.run("data.domain", &json!({"layer": "Roads", "field": "surface", "coded": [["paved", "Paved"], ["gravel", "Gravel"]]})).unwrap();
    s.run("data.domain", &json!({"layer": "Roads", "field": "lanes", "min": 1, "max": 8})).unwrap();
    assert!(s.run("data.domain", &json!({"layer": "Roads", "field": "name", "min": 1, "max": 2})).is_err(), "text takes coded values");
    let line = json!({"type": "LineString", "coordinates": [[0, 0], [1, 1]]});
    s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"surface": "paved", "lanes": 2}})).unwrap();
    assert!(s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"surface": "ice"}})).is_err());
    assert!(s.run("edit.setValue", &json!({"layer": "Roads", "row": 0, "field": "lanes", "value": 12})).is_err());
    assert_eq!(s.map().layer_by_name("Roads").unwrap().dataset.features.len(), 1, "the failed edits were undone");
    // Subtypes fill defaults on new features.
    s.run("data.subtypes", &json!({"layer": "Roads", "field": "kind", "subtypes": [{"code": 1, "name": "Highway", "defaults": {"lanes": 4}}, {"code": 2, "name": "Street", "defaults": {"lanes": 1}}]})).unwrap();
    assert!(s.run("data.subtypes", &json!({"layer": "Roads", "field": "name", "subtypes": [{"code": 1, "name": "x"}]})).is_err());
    let r = s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"kind": 1}})).unwrap();
    let row = r["row"].as_u64().unwrap() as usize;
    let lanes = |s: &Session, row: usize| {
        let l = s.map().layer_by_name("Roads").unwrap();
        l.dataset.value(row, l.dataset.field_index("lanes").unwrap()).clone()
    };
    assert_eq!(lanes(&s, row).as_f64(), Some(4.0));
    // Attribute rules: a calculation and a constraint.
    s.run(
        "data.attributeRule",
        &json!({"layer": "Roads", "name": "Length", "kind": "calculation", "field": "length_km", "expression": "lanes * 10"}),
    )
    .unwrap();
    s.run(
        "data.attributeRule",
        &json!({"layer": "Roads", "name": "Named highways", "kind": "constraint", "expression": "kind <> 1 OR name IS NOT NULL"}),
    )
    .unwrap();
    assert!(
        s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"kind": 1}})).is_err(),
        "highways need names"
    );
    let r = s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"kind": 1, "name": "A1"}})).unwrap();
    let row = r["row"].as_u64().unwrap() as usize;
    let l = s.map().layer_by_name("Roads").unwrap();
    assert_eq!(l.dataset.value(row, l.dataset.field_index("length_km").unwrap()).as_f64(), Some(40.0));
    assert!(s.run("data.attributeRule", &json!({"layer": "Roads", "name": "Bad", "kind": "constraint", "expression": "(("})).is_err());
    s.run("data.attributeRule", &json!({"layer": "Roads", "name": "Named highways", "remove": true})).unwrap();
    // Deleting a field a rule uses drops the rule, so editing isn't blocked.
    s.run("edit.addField", &json!({"layer": "Roads", "name": "temp", "type": "double"})).unwrap();
    s.run("data.attributeRule", &json!({"layer": "Roads", "name": "Temp", "kind": "constraint", "expression": "temp IS NULL OR temp > 0"}))
        .unwrap();
    let r = s.run("edit.deleteField", &json!({"layer": "Roads", "field": "temp"})).unwrap();
    assert_eq!(r["rulesRemoved"], 1);
    s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"kind": 2}})).unwrap();
    // Contingent values.
    s.run("data.contingentValues", &json!({"layer": "Roads", "name": "Surface by kind", "fields": ["kind", "surface"], "combos": [[1, "paved"], [2, "paved"], [2, "gravel"]]})).unwrap();
    assert!(
        s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"kind": 1, "surface": "gravel"}})).is_err()
    );
    s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "attributes": {"kind": 2, "surface": "gravel"}})).unwrap();
    // Templates.
    s.run("edit.template", &json!({"layer": "Roads", "name": "Gravel street", "defaults": {"kind": 2, "surface": "gravel"}})).unwrap();
    let r = s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "template": "Gravel street"})).unwrap();
    assert_eq!(lanes(&s, r["row"].as_u64().unwrap() as usize).as_f64(), Some(1.0), "template, then subtype defaults");
    // Archive and restore.
    s.run("data.archive", &json!({"layer": "Roads"})).unwrap();
    let t0 = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap().as_secs_f64() - 0.001;
    let n = s.map().layer_by_name("Roads").unwrap().dataset.features.len();
    s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line, "template": "Gravel street"})).unwrap();
    s.run("edit.setValue", &json!({"layer": "Roads", "row": 0, "field": "name", "value": "Main"})).unwrap();
    let h = s.run("data.archiveHistory", &json!({"layer": "Roads"})).unwrap();
    assert_eq!(h["edits"], 2);
    s.run("data.archiveRestore", &json!({"layer": "Roads", "time": t0})).unwrap();
    let l = s.map().layer_by_name("Roads").unwrap();
    assert_eq!(l.dataset.features.len(), n);
    assert!(l.dataset.value(0, l.dataset.field_index("name").unwrap()).is_null());
    // Edit status, and turning editing off.
    let st = s.run("edit.status", &Value::Null).unwrap();
    assert!(st["layers"].as_array().unwrap().iter().any(|l| l["layer"] == "Roads" && l["added"].as_u64().unwrap() >= 5));
    s.run("edit.enable", &json!({"on": false})).unwrap();
    assert!(s.run("edit.addFeature", &json!({"layer": "Roads", "geometry": line})).is_err());
    s.run("edit.enable", &json!({"on": true})).unwrap();
}

#[test]
fn modify_tools_snapping_topology_relationships_and_elevation() {
    let mut s = with_raster();
    s.run("layer.template", &json!({"template": "lines", "name": "Paths"})).unwrap();
    let add = |s: &mut Session, coords: Value| {
        s.run("edit.addFeature", &json!({"layer": "Paths", "geometry": {"type": "LineString", "coordinates": coords}})).unwrap()["row"]
            .as_u64()
            .unwrap() as usize
    };
    let a = add(&mut s, json!([[0, 0], [1, 0]]));
    // Move one degree east (about 111 km at the equator), then rotate and scale.
    s.run("select.rows", &json!({"layer": "Paths", "rows": [a]})).unwrap();
    s.run("edit.move", &json!({"dx": 111_319.5, "dy": 0})).unwrap();
    let first = |s: &Session, row: usize| match s.map().layer_by_name("Paths").unwrap().dataset.features[row].geometry.as_ref().unwrap() {
        rustgis_data::Geometry::Line(p) => p[0][0],
        _ => panic!("not a line"),
    };
    assert!((first(&s, a).x - 1.0).abs() < 1e-3, "{:?}", first(&s, a));
    s.run("edit.rotate", &json!({"angle": 90})).unwrap();
    s.run("edit.scale", &json!({"factor": 2})).unwrap();
    s.run("edit.flip", &Value::Null).unwrap();
    // Split, then merge back, then explode.
    let b = add(&mut s, json!([[10, 10], [12, 10]]));
    s.run("select.rows", &json!({"layer": "Paths", "rows": [b]})).unwrap();
    s.run("edit.split", &json!({"lon": 11, "lat": 10.2})).unwrap();
    let n = s.map().layer_by_name("Paths").unwrap().dataset.features.len();
    s.run("select.rows", &json!({"layer": "Paths", "rows": [b, n - 1]})).unwrap();
    // A merge the rules reject leaves the layer as it was.
    s.run("data.attributeRule", &json!({"layer": "Paths", "name": "Never", "kind": "constraint", "expression": "name = 'never'"})).unwrap();
    let before = s.map().layer_by_name("Paths").unwrap().dataset.features.clone();
    assert!(s.run("edit.merge", &Value::Null).is_err());
    assert_eq!(s.map().layer_by_name("Paths").unwrap().dataset.features, before);
    s.run("data.attributeRule", &json!({"layer": "Paths", "name": "Never", "remove": true})).unwrap();
    s.run("select.rows", &json!({"layer": "Paths", "rows": [b, n - 1]})).unwrap();
    s.run("edit.merge", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Paths").unwrap().dataset.features.len(), n - 1);
    s.run("edit.explode", &Value::Null).unwrap();
    assert_eq!(s.map().layer_by_name("Paths").unwrap().dataset.features.len(), n);
    assert!(s.run("edit.setGeometry", &json!({"layer": "Paths", "row": 0, "geometry": {"type": "Point", "coordinates": [0, 0]}})).is_err());
    s.run("edit.setGeometry", &json!({"layer": "Paths", "row": 0, "geometry": {"type": "LineString", "coordinates": [[0, 0], [0, 1]]}}))
        .unwrap();
    // Vertex editing (the Edit Vertices tool): insert, move and delete, one undo step each.
    let line = |s: &Session| match s.map().layer_by_name("Paths").unwrap().dataset.features[0].geometry.as_ref().unwrap() {
        rustgis_data::Geometry::Line(p) => p[0].iter().map(|c| [c.x, c.y]).collect::<Vec<_>>(),
        _ => panic!("not a line"),
    };
    let r = s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "insert", "index": 0, "lon": 0.5, "lat": 0.5})).unwrap();
    assert_eq!(r["index"], 1, "the new vertex");
    assert_eq!(line(&s), [[0.0, 0.0], [0.5, 0.5], [0.0, 1.0]]);
    s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "move", "index": 2, "lon": 1, "lat": 1})).unwrap();
    assert_eq!(line(&s), [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
    s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "delete", "index": 1})).unwrap();
    assert_eq!(line(&s), [[0.0, 0.0], [1.0, 1.0]]);
    assert!(s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "delete", "index": 1})).is_err(), "a line keeps two");
    assert!(s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "move", "index": 7, "lon": 1, "lat": 1})).is_err());
    assert!(s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "move", "index": 0, "lon": 1e9, "lat": 1})).is_err());
    for bad in [json!(-1), json!("2"), json!(0.5)] {
        assert!(
            s.run("edit.vertex", &json!({"layer": "Paths", "row": 0, "action": "move", "part": bad, "index": 0, "lon": 1, "lat": 1}))
                .is_err(),
            "part {bad}"
        );
    }
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(line(&s), [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]], "undo brings the deleted vertex back");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(line(&s), [[0.0, 0.0], [0.5, 0.5], [0.0, 1.0]]);
    // Traverse: two legs from a start, ground to grid off and on.
    s.run("layer.select", &json!({"layer": "Paths"})).unwrap();
    let r = s
        .run("edit.traverse", &json!({"start": [20, 0], "legs": [{"distance": 1000, "bearing": 90}, {"distance": 1000, "bearing": 0}]}))
        .unwrap();
    let row = r["row"].as_u64().unwrap() as usize;
    let end = |s: &Session, row: usize| match s.map().layer_by_name("Paths").unwrap().dataset.features[row].geometry.as_ref().unwrap() {
        rustgis_data::Geometry::Line(p) => *p[0].last().unwrap(),
        _ => panic!("not a line"),
    };
    let e = end(&s, row);
    assert!((e.x - 20.008983).abs() < 1e-5 && (e.y - 0.009043).abs() < 1e-4, "{e:?}");
    s.run("edit.groundToGrid", &json!({"scale": 1.5, "rotation": 0})).unwrap();
    let r = s.run("edit.traverse", &json!({"start": [20, 0], "legs": [{"distance": 1000, "bearing": 90}]})).unwrap();
    assert!((end(&s, r["row"].as_u64().unwrap() as usize).x - 20.013475).abs() < 1e-5);
    assert!(s.run("edit.groundToGrid", &json!({"scale": 9})).is_err());
    s.run("edit.groundToGrid", &json!({"on": false})).unwrap();
    // Snapping to a vertex near the pointer.
    s.run("map.goToXY", &json!({"lon": 20, "lat": 0, "scale": 50_000})).unwrap();
    let off = s.run("edit.snap", &json!({"lon": 20.0001, "lat": 0.0001})).unwrap();
    assert_eq!(off["snapped"], false, "snapping is off");
    s.run("edit.snapping", &json!({"on": true, "tolerancePx": 20})).unwrap();
    let on = s.run("edit.snap", &json!({"lon": 20.0001, "lat": 0.0001})).unwrap();
    assert_eq!(on["snapped"], true);
    assert!((on["lon"].as_f64().unwrap() - 20.0).abs() < 1e-9 && on["lat"].as_f64().unwrap().abs() < 1e-9, "{on}");
    // A dragged vertex doesn't snap to the feature it belongs to.
    s.run("map.goToXY", &json!({"lon": 0.5, "lat": 0.5, "scale": 50_000})).unwrap();
    let paths = s.map().layer_by_name("Paths").unwrap().id;
    let near = rustgis_geom::Coord::new(0.5001, 0.5001);
    let own = crate::cmd::design::snap_except(&mut s, near, None).map(|(c, _)| c);
    assert!(own.is_some_and(|c| (c.x - 0.5).abs() < 1e-9 && (c.y - 0.5).abs() < 1e-9), "the line's own vertex: {own:?}");
    assert_eq!(crate::cmd::design::snap_except(&mut s, near, Some((paths, 0))), None);
    // Topology: overlapping countries? Lakes inside countries? Dangling paths.
    s.run("topology.rules", &json!({"add": {"rule": "mustNotHaveDangles", "layer": "Paths"}})).unwrap();
    s.run("topology.rules", &json!({"add": {"rule": "mustBeCoveredBy", "layer": "Cities", "by": "Countries"}})).unwrap();
    assert!(s.run("topology.rules", &json!({"add": {"rule": "mustNotOverlap", "layer": "Paths"}})).is_err(), "overlap is for polygons");
    let v = s.run("topology.validate", &Value::Null).unwrap();
    assert!(v["errors"].as_u64().unwrap() > 0);
    assert_eq!(s.topology_errors.len() as u64, v["errors"].as_u64().unwrap());
    // Relationships: cities to their country by ISO code.
    s.run("data.relate", &json!({"name": "Country of city", "origin": "Countries", "originKey": "name", "destination": "Cities", "destinationKey": "adm0name"})).unwrap();
    assert!(
        s.run(
            "data.relate",
            &json!({"name": "x", "origin": "Countries", "originKey": "nope", "destination": "Cities", "destinationKey": "adm0name"})
        )
        .is_err()
    );
    s.run("select.byAttributes", &json!({"layer": "Countries", "where": "name = 'France'"})).unwrap();
    let rel = s.run("data.related", &Value::Null).unwrap();
    assert!(rel["related"].as_u64().unwrap() >= 1, "{rel}");
    // Elevation from the raster surface goes in z.
    s.run("edit.elevation", &json!({"mode": "constant", "z": 125})).unwrap();
    s.run("layer.template", &json!({"template": "points", "name": "Spots"})).unwrap();
    s.run("edit.addFeature", &json!({"layer": "Spots", "geometry": {"type": "Point", "coordinates": [0, 0]}})).unwrap();
    let l = s.map().layer_by_name("Spots").unwrap();
    assert_eq!(l.dataset.value(0, l.dataset.field_index("z").unwrap()).as_f64(), Some(125.0));
    assert!(s.run("edit.elevation", &json!({"mode": "surface", "surface": "Cities"})).is_err(), "not a raster");
    let tools = s.run("edit.tools", &Value::Null).unwrap();
    assert!(tools.as_array().unwrap().len() >= 9);
}

/// A 40×40 lon/lat DEM near (10°E, 45°N): a hill in the middle, with a wall to its east.
fn dem_b64() -> String {
    use base64::Engine;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let gt = GeoTransform { origin_x: 10.0, origin_y: 45.04, pixel_width: 0.001, pixel_height: -0.001 };
    let z: Vec<f32> = (0..1600)
        .map(|i| {
            let (c, r) = ((i % 40) as f32 - 20.0, (i / 40) as f32 - 20.0);
            let hill = 100.0 * (-(c * c + r * r) / 60.0).exp();
            if i % 40 == 30 { 400.0 } else { 200.0 + hill }
        })
        .collect();
    let r = Raster::new("dem", 40, 40, vec![z], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    base64::engine::general_purpose::STANDARD.encode(geotiff::write(&r, 0).unwrap())
}

#[test]
fn analysis_workflows_models_scripts_and_raster_functions() {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    // Raster functions and a chain.
    for (func, extra) in [
        ("slope", json!({})),
        ("aspect", json!({})),
        ("hillshade", json!({"azimuth": 300})),
        ("threshold", json!({"value": 250})),
        ("bandMath", json!({"expression": "b1 * 2 - 100"})),
    ] {
        let mut p = json!({"layer": dem, "function": func});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        s.run("raster.function", &p).unwrap_or_else(|e| panic!("{func}: {e}"));
    }
    assert!(s.run("raster.function", &json!({"layer": dem, "function": "warp"})).is_err());
    assert!(s.run("raster.function", &json!({"layer": dem, "function": "bandMath", "expression": "b2"})).is_err());
    let r = s
        .run(
            "raster.functionChain",
            &json!({"layer": dem, "steps": [{"function": "slope"}, {"function": "threshold", "value": 10}], "name": "Steep"}),
        )
        .unwrap();
    let steep = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
    assert_eq!(steep.name, "Steep");
    let data = steep.raster.as_ref().unwrap().data.as_ref().unwrap();
    assert!(data.bands[0].stats.max == 1.0 && data.bands[0].stats.min == 0.0);
    // Rasters made by functions save with the project.
    let text = serde_json::to_string(&s.project).unwrap();
    let back = crate::Project::from_json(&text).unwrap();
    assert!(back.map().layer_by_name("Steep").unwrap().raster.as_ref().unwrap().data.is_some());
    // Suitability: high and flat.
    let slope_name = format!("{dem}_slope");
    let r = s
        .run(
            "analysis.suitability",
            &json!({"criteria": [
        {"layer": dem, "ranges": [[0, 250, 1], [250, 1000, 10]], "weight": 2},
        {"layer": slope_name, "ranges": [[0, 5, 10], [5, 90, 1]], "weight": 1}]}),
        )
        .unwrap();
    assert!(r["layer"].as_u64().is_some());
    // Viewshed and line of sight: the wall at column 30 hides what's beyond it.
    // On the flat northern edge: everything west of the wall is in view.
    let v = s.run("analysis.viewshed", &json!({"layer": dem, "lon": 10.0055, "lat": 45.0395, "height": 2})).unwrap();
    assert!(v["visibleCells"].as_u64().unwrap() > 100, "{v}");
    let los = s.run("analysis.lineOfSight", &json!({"layer": dem, "from": [10.0205, 45.0395], "to": [10.0355, 45.0395]})).unwrap();
    assert_eq!(los["visible"], false);
    let los = s.run("analysis.lineOfSight", &json!({"layer": dem, "from": [10.0205, 45.0395], "to": [10.0055, 45.0395]})).unwrap();
    assert_eq!(los["visible"], true);
    // IDW of city populations, with an environment extent and cell size.
    s.run("gp.env", &json!({"extent": [-10, 35, 30, 60], "cellSize": 0.5})).unwrap();
    let r = s.run("analysis.interpolate", &json!({"layer": "Cities", "field": "pop_max"})).unwrap();
    let idw = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert_eq!((idw.width, idw.height), (80, 50));
    // The environment extent limits tools to the features meeting it.
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 10, "output": "Europe buffers"}})).unwrap();
    let europe = s.map().layer_by_name("Europe buffers").unwrap().dataset.features.len();
    let all = s.map().layer_by_name("Cities").unwrap().dataset.features.len();
    assert!(europe > 10 && europe < all / 2, "{europe} of {all}");
    s.run("gp.env", &json!({"extent": null, "cellSize": null})).unwrap();
    // Network route along the rivers: two points on the same river.
    let rivers = s.map().layer_by_name("Rivers").unwrap();
    let (a, b2) = match rivers.dataset.features.iter().filter_map(|f| f.geometry.as_ref()).find_map(|g| match g {
        rustgis_data::Geometry::Line(p) if p[0].len() > 10 => Some((p[0][0], p[0][p[0].len() - 1])),
        _ => None,
    }) {
        Some(x) => x,
        None => panic!("no river"),
    };
    let r = s.run("analysis.route", &json!({"layer": "Rivers", "from": [a.x, a.y], "to": [b2.x, b2.y]})).unwrap();
    assert!(r["lengthKm"].as_f64().unwrap() > 10.0, "{r}");
    // Neighbourhood, simulation, profile, exploration, similarity, conversion.
    let n = s
        .run("analysis.neighborhood", &json!({"lon": 2.35, "lat": 48.85, "radius": 300000, "layers": ["Cities"], "select": true}))
        .unwrap();
    assert!(n["layers"][0]["count"].as_u64().unwrap() >= 2);
    s.run("select.byAttributes", &json!({"layer": "Countries", "where": "name = 'France'"})).unwrap();
    let sim = s.run("analysis.simulate", &json!({"layer": "Countries", "count": 20, "seed": 3})).unwrap();
    assert_eq!(sim["points"], 20);
    s.run("select.clear", &Value::Null).unwrap();
    let prof = s.run("data.profile", &json!({"layer": "Countries"})).unwrap();
    assert!(prof["fields"].as_array().unwrap().iter().any(|f| f["field"] == "pop_est" && f["min"].as_f64().is_some()));
    let ex = s.run("data.explore", &json!({"layer": "Countries", "fields": ["pop_est", "gdp_md"]})).unwrap();
    assert!(ex["correlation"][0][1].as_f64().unwrap() > 0.3, "population and GDP correlate");
    let fr = s.map().layer_by_name("Countries").unwrap().dataset.features.iter().position(|_| true).unwrap();
    let sm = s.run("data.similar", &json!({"layer": "Countries", "row": fr, "fields": ["pop_est", "gdp_md"], "count": 3})).unwrap();
    assert_eq!(sm["similar"].as_array().unwrap().len(), 3);
    for fmt in ["kml", "gpx", "wkt", "geojson", "csv"] {
        let t = s.run("data.convert", &json!({"layer": "Cities", "format": fmt})).unwrap();
        assert!(t["text"].as_str().unwrap().len() > 1000, "{fmt}");
    }
    // Models chain tools; scripts run commands.
    s.run(
        "model.new",
        &json!({"name": "Buffer then dissolve", "steps": [
        {"tool": "buffer", "params": {"input": "Cities", "distance": 50}},
        {"tool": "dissolve", "params": {"input": "%1"}}]}),
    )
    .unwrap();
    let r = s.run("model.run", &json!({"name": "Buffer then dissolve"})).unwrap();
    assert_eq!(r["ran"].as_array().unwrap().len(), 2);
    assert!(s.run("model.new", &json!({"steps": [{"tool": "nope", "params": {}}]})).is_err());
    // A model carried in a file can't touch files.
    s.project.models.push(crate::project::Model {
        name: "From a file".into(),
        steps: vec![crate::project::ModelStep {
            tool: "buffer".into(),
            params: json!({"input": "Cities", "distance": 5, "output": "/tmp/x.shp"}),
        }],
        ..Default::default()
    });
    let e = s.run("model.run", &json!({"name": "From a file"})).unwrap_err();
    assert!(e.contains("from a file"), "{e}");
    let m = s.run("model.new", &json!({"name": "Replay", "fromHistory": true})).unwrap();
    assert!(m["steps"].as_u64().unwrap() >= 2);
    let dissolve = s.project.models.last().unwrap().steps.iter().find(|st| st.tool == "dissolve").unwrap();
    assert!(dissolve.params["input"].as_str().unwrap().starts_with('%'), "history inputs wire to earlier steps");
    let r = s.run("script.run", &json!({"script": "# zoom then select\nmap.goToXY {\"lon\": 2, \"lat\": 48, \"scale\": 5000000}\nselect.byAttributes {\"layer\": \"Cities\", \"where\": \"pop_max > 1000000\"}"})).unwrap();
    assert_eq!(r["ran"].as_array().unwrap().len(), 2);
    assert!(s.run("script.run", &json!({"script": "no.such {}"})).is_err());
    model_graphs(&mut s);
    let g = s.run("gp.gallery", &json!({"kind": "raster"})).unwrap();
    assert!(g.as_array().unwrap().iter().any(|t| t["id"] == "raster.function"));
}

/// A 100 × 80 scan whose value is `ramp(col, row)` (so a bilinear warp reproduces it exactly), with
/// a bright pixel at (50, 40), added as a layer with a placeholder position.
fn scan_layer(s: &mut Session, name: &str) -> (fn(f64, f64) -> f32, (f64, f64)) {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    fn ramp(c: f64, r: f64) -> f32 {
        (c * 2.0 + r * 0.5) as f32
    }
    let (w, h) = (100u32, 80u32);
    let mut z: Vec<f32> = (0..w * h).map(|i| ramp(f64::from(i % w) + 0.5, f64::from(i / w) + 0.5)).collect();
    z[(40 * w + 50) as usize] = 5000.0;
    let gt = GeoTransform { origin_x: 0.0, origin_y: 80.0, pixel_width: 1.0, pixel_height: -1.0 };
    let r = Raster::new(name, w, h, vec![z], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let bytes = geotiff::write(&r, 0).unwrap();
    crate::cmd::raster::add_raster(s, name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
    (ramp, (50.5, 40.5))
}

/// The control points of a known map from the scan's corners and middle: `f` takes a pixel position
/// to map coordinates.
fn points_of(f: impl Fn(f64, f64) -> (f64, f64)) -> Value {
    let pts: Vec<Value> = [(0.0, 0.0), (100.0, 0.0), (0.0, 80.0), (100.0, 80.0), (50.0, 40.0), (25.0, 60.0)]
        .iter()
        .map(|&(c, r)| {
            let (x, y) = f(c, r);
            json!([c, r, x, y])
        })
        .collect();
    Value::Array(pts)
}

fn out_raster(s: &Session, v: &Value) -> std::sync::Arc<rustgis_data::raster::Raster> {
    let id = v["layer"].as_u64().unwrap() as u32;
    s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap()
}

/// Warping through a known sheared affine puts every pixel where the matrix says: a bilinear
/// warp of a ramp reproduces the ramp at the pixel each output cell comes from, and the bright
/// pixel lands within half a cell of its place.
#[test]
fn a_warp_through_a_known_affine_puts_pixels_where_the_matrix_says() {
    let mut s = world();
    let (ramp, bright) = scan_layer(&mut s, "scan");
    // x = 500000 + 10 col + 2 row, y = 4000000 - 10 row + 1 col, in UTM 13N.
    let map = |c: f64, r: f64| (500_000.0 + 10.0 * c + 2.0 * r, 4_000_000.0 - 10.0 * r + c);
    let v = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "transform": "affine", "pointsCrs": 32613, "crs": 32613, "points": points_of(map), "name": "warped", "resampling": "bilinear"}),
        )
        .unwrap();
    assert_eq!(v["crs"], "EPSG:32613", "{v}");
    let out = out_raster(&s, &v);
    assert!(s.map().layer_by_name("scan").unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().width == 100, "the scan is kept");
    // The cell is the root of the pixel's area: |10 × -10 - 2 × 1| = 102.
    let cell = 102f64.sqrt();
    assert!((out.transform.pixel_width - cell).abs() < 1e-9 && (out.transform.pixel_height + cell).abs() < 1e-9);
    // The grid covers the footprint, whose corners are the matrix's corners.
    let corners = [(0.0, 0.0), (100.0, 0.0), (0.0, 80.0), (100.0, 80.0)].map(|(c, r)| map(c, r));
    let (min_x, max_x) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), (x, _)| (a.min(*x), b.max(*x)));
    let (min_y, max_y) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), (_, y)| (a.min(*y), b.max(*y)));
    assert_eq!((out.transform.origin_x, out.transform.origin_y), (min_x, max_y));
    assert!(out.transform.origin_x + out.transform.pixel_width * f64::from(out.width) >= max_x);
    assert!(out.transform.origin_y + out.transform.pixel_height * f64::from(out.height) <= min_y);
    // The matrix's inverse takes a map position back to the scan.
    let det = 10.0 * -10.0 - 2.0 * 1.0;
    let back = |x: f64, y: f64| {
        let (dx, dy) = (x - 500_000.0, y - 4_000_000.0);
        ((-10.0 * dx - 2.0 * dy) / det, (-dx + 10.0 * dy) / det)
    };
    let (mut checked, mut empty) = (0, 0);
    for row in (0..i64::from(out.height)).step_by(7) {
        for col in (0..i64::from(out.width)).step_by(5) {
            let c = out.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
            let (sc, sr) = back(c.x, c.y);
            match out.value(0, col, row) {
                // Away from the scan's edge and its bright pixel the value is the ramp's there.
                Some(v) if (1.5..98.5).contains(&sc) && (1.5..78.5).contains(&sr) && (sc - 50.5).abs() > 2.0 && (sr - 40.5).abs() > 2.0 => {
                    assert!((v - ramp(sc, sr)).abs() < 1e-3, "cell {col},{row}: {v} against {}", ramp(sc, sr));
                    checked += 1;
                }
                Some(_) => {}
                None => {
                    assert!(!(0.0..100.0).contains(&sc) || !(0.0..80.0).contains(&sr), "cell {col},{row} comes from inside the scan");
                    empty += 1;
                }
            }
        }
    }
    assert!(checked > 100 && empty > 0, "{checked} checked, {empty} outside the scan");
    // The bright pixel: its intensity-weighted position is within half a cell of the matrix's.
    let (mut sum, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
    for row in 0..i64::from(out.height) {
        for col in 0..i64::from(out.width) {
            if let Some(v) = out.value(0, col, row).filter(|v| *v > 1000.0) {
                let c = out.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
                sum += f64::from(v);
                sx += f64::from(v) * c.x;
                sy += f64::from(v) * c.y;
            }
        }
    }
    let (ex, ey) = map(bright.0, bright.1);
    assert!(
        sum > 0.0 && (sx / sum - ex).abs() <= cell / 2.0 && (sy / sum - ey).abs() <= cell / 2.0,
        "({}, {}) against ({ex}, {ey})",
        sx / sum,
        sy / sum
    );
}

/// A 30° rotation, nearest sampling, replacing the layer's raster: the cells of the bright pixel
/// are centred within half a cell of its place, and the grid's corners outside the turned image
/// are no data.
#[test]
fn a_warp_through_a_known_rotation_puts_pixels_where_the_turn_says() {
    let mut s = world();
    let (_, bright) = scan_layer(&mut s, "scan");
    let (sin, cos) = 30f64.to_radians().sin_cos();
    // Ten metres a pixel, turned 30° about the origin (rows still run down the picture).
    let map = |c: f64, r: f64| (500_000.0 + 10.0 * (c * cos + r * sin), 4_000_000.0 + 10.0 * (c * sin - r * cos));
    let v = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "transform": "similarity", "pointsCrs": 32613, "crs": "EPSG:32613", "points": points_of(map), "resampling": "nearest", "cellSize": 2.5}),
        )
        .unwrap();
    let out = out_raster(&s, &v);
    assert_eq!(
        s.map().layer_by_name("scan").unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().width,
        out.width,
        "replaced in place"
    );
    assert!(out.width > 400, "the turned image needs a wider grid than its 1000 m / 2.5 m: {}", out.width);
    // Nearest keeps the values: the bright pixel's cells are 5000, a square 10 m across turned 30°.
    let (mut n, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
    for row in 0..i64::from(out.height) {
        for col in 0..i64::from(out.width) {
            if out.value(0, col, row) == Some(5000.0) {
                let c = out.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
                n += 1.0;
                sx += c.x;
                sy += c.y;
            }
        }
    }
    let (ex, ey) = map(bright.0, bright.1);
    let cell = out.transform.pixel_width;
    // About 100 / 6.25 = 16 cells.
    assert!((10.0..=22.0).contains(&n), "{n} cells");
    assert!((sx / n - ex).abs() <= cell / 2.0 && (sy / n - ey).abs() <= cell / 2.0, "({}, {}) against ({ex}, {ey})", sx / n, sy / n);
    // The turned rectangle leaves the box's corners empty.
    assert_eq!(out.value(0, 0, 0), None);
    assert_eq!(out.value(0, i64::from(out.width) - 1, i64::from(out.height) - 1), None);
    // At its default the cell is the pixel's 10 m.
    let d = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "transform": "similarity", "pointsCrs": 32613, "crs": 32613, "points": points_of(map), "name": "d"}),
        )
        .unwrap();
    assert!((out_raster(&s, &d).transform.pixel_width - 10.0).abs() < 1e-6);
}

/// Control points in longitude/latitude, warped into UTM: every cell holds the scan's value at
/// the pixel the projection takes the cell's centre back to.
#[test]
fn a_warp_reprojects_from_the_points_crs() {
    use rustgis_data::raster::Crs;
    let mut s = world();
    let (ramp, bright) = scan_layer(&mut s, "scan");
    // A north-up block at 40°N, 105°W, a thousandth of a degree a pixel.
    let map = |c: f64, r: f64| (-105.0 + c * 0.001, 40.0 - r * 0.001);
    let v = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "transform": "affine", "crs": 32613, "pointsCrs": "EPSG:4326", "points": points_of(map), "name": "utm"}),
        )
        .unwrap();
    let out = out_raster(&s, &v);
    assert_eq!(out.crs.epsg(), 32613);
    let t = Crs::from_epsg(32613).unwrap().transformer().unwrap();
    let mut checked = 0;
    for row in (0..i64::from(out.height)).step_by(3) {
        for col in (0..i64::from(out.width)).step_by(3) {
            let c = out.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
            let ll = t.to_lonlat(c).unwrap();
            let (sc, sr) = ((ll.x + 105.0) / 0.001, (40.0 - ll.y) / 0.001);
            if (2.0..98.0).contains(&sc) && (2.0..78.0).contains(&sr) && (sc - 50.5).abs() > 2.5 && (sr - 40.5).abs() > 2.5 {
                let got = out.value(0, col, row).unwrap();
                assert!((got - ramp(sc, sr)).abs() < 0.05, "cell {col},{row}: {got} against {}", ramp(sc, sr));
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "{checked}");
    // The bright pixel's own UTM position is within a cell of a bright cell.
    let (lon, lat) = map(bright.0, bright.1);
    let at = t.from_lonlat(rustgis_geom::Coord::new(lon, lat)).unwrap();
    let (col, row) = out.transform.to_pixel(at);
    let (col, row) = (col.floor() as i64, row.floor() as i64);
    let near = (-1..=1)
        .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
        .filter_map(|(dx, dy)| out.value(0, col + dx, row + dy))
        .fold(f32::MIN, f32::max);
    assert!(near > 500.0, "{near}");
}

/// A 48-bit colour scan (UInt16 RGB) warps with alpha the renderer reads as opaque (it takes
/// alpha as 0–255 in every type), and the COG keeps the 16-bit type.
#[test]
fn a_sixteen_bit_colour_warp_is_opaque_to_the_renderer() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let mut s = world();
    let n = 60 * 40;
    let bands = vec![vec![60_000.0; n], vec![30_000.0; n], vec![1000.0; n]];
    let gt = GeoTransform { origin_x: 0.0, origin_y: 40.0, pixel_width: 1.0, pixel_height: -1.0 };
    let r = Raster::new("deep", 60, 40, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "UInt16").unwrap();
    let bytes = rustgis_data::raster::geotiff::write(&r, 0).unwrap();
    crate::cmd::raster::add_raster(&mut s, "deep", rustgis_map::RasterLayer::from_bytes(r, "deep.tif", bytes)).unwrap();
    let (sin, cos) = 20f64.to_radians().sin_cos();
    let map = |c: f64, r: f64| (500_000.0 + 10.0 * (c * cos + r * sin), 4_000_000.0 + 10.0 * (c * sin - r * cos));
    let pts: Vec<Value> =
        [(0.0, 0.0), (60.0, 0.0), (0.0, 40.0), (60.0, 40.0)].iter().map(|&(c, r)| json!([c, r, map(c, r).0, map(c, r).1])).collect();
    let path = std::env::temp_dir().join(format!("rustgis-deep-{}.tif", std::process::id()));
    let v = s
        .run("raster.georeference", &json!({"layer": "deep", "transform": "similarity", "pointsCrs": 32613, "points": pts, "name": "w", "path": path.display().to_string()}))
        .unwrap();
    let out = out_raster(&s, &v);
    assert_eq!(out.color, ColorHint::Rgb { alpha: true });
    let (mid_c, mid_r) = (i64::from(out.width) / 2, i64::from(out.height) / 2);
    // The renderer clamps alpha to 0–255: inside is 255, the collar 0.
    let alpha = |c, r| out.value(3, c, r).map(|v| v.clamp(0.0, 255.0) as u8);
    assert_eq!((alpha(mid_c, mid_r), alpha(0, 0)), (Some(255), Some(0)));
    let back = rustgis_data::raster::read_path(&path).unwrap();
    assert_eq!((back.data_type.as_str(), back.band_count()), ("UInt16", 4));
    assert_eq!(back.value(0, mid_c, mid_r), Some(60_000.0));
    std::fs::remove_file(&path).ok();
}

/// A scan that would span the globe in degrees is refused, and a long thin grid can't slip past
/// the size limit through its row buffers.
#[test]
fn a_warp_refuses_a_globe_in_degrees_and_bounds_its_memory() {
    use rustgis_analysis::georef::{self, Gcp, Kind};
    use rustgis_analysis::warp::{self, Options, Resampling};
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let (w, h) = (40u32, 40u32);
    let gt = GeoTransform { origin_x: 0.0, origin_y: 40.0, pixel_width: 1.0, pixel_height: -1.0 };
    let r = Raster::new("s", w, h, vec![vec![1.0; 1600]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    // A square of 2000 km centred on the pole, in polar stereographic (EPSG:3413).
    let gcps = |f: &dyn Fn(f64, f64) -> (f64, f64)| -> Vec<Gcp> {
        [(0.0, 0.0), (40.0, 0.0), (0.0, 40.0), (40.0, 40.0)]
            .iter()
            .map(|&(c, r)| {
                let (x, y) = f(c, r);
                Gcp { col: c, row: r, x, y, on: true }
            })
            .collect()
    };
    let fit = georef::fit(Kind::Affine, &gcps(&|c, r| (-1_000_000.0 + 50_000.0 * c, 1_000_000.0 - 50_000.0 * r))).unwrap();
    let o = |crs: Crs, points: Crs, cell| Options { crs, points_crs: points, resampling: Resampling::Nearest, cell, name: "o".into() };
    let polar = Crs::from_epsg(3413).unwrap();
    // In degrees the scan spans every longitude: refused, with a hint.
    let e = warp::warp(&r, &fit, &o(Crs::LonLat, polar, Some(0.05))).unwrap_err();
    assert!(e.contains("projected crs"), "{e}");
    // In Web Mercator it is placed (to its latitude limit), with the pole inside it.
    assert!(warp::warp(&r, &fit, &o(Crs::WebMercator, polar, Some(100_000.0))).is_ok());
    // A strip a hundred million metres long and a metre tall, from hostile points.
    let strip = georef::fit(Kind::Affine, &gcps(&|c, r| (500_000.0 + c * 2.5e6, 4_000_000.0 - r * 0.025))).unwrap();
    let utm13 = Crs::from_epsg(32613).unwrap();
    let e = warp::warp(&r, &strip, &o(utm13, utm13, Some(1.0))).unwrap_err();
    assert!(e.contains("cellSize"), "{e}");
}

/// A warp written as a COG becomes the layer's file; the options are checked.
#[test]
fn a_warp_can_write_a_cog_and_refuses_what_it_cannot_place() {
    let mut s = world();
    scan_layer(&mut s, "scan");
    let map = |c: f64, r: f64| (500_000.0 + 10.0 * c, 4_000_000.0 - 10.0 * r);
    let dir = std::env::temp_dir().join(format!("rustgis-warp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("warped.tif");
    let v = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "transform": "affine", "pointsCrs": 32613, "crs": 32613, "points": points_of(map), "resampling": "cubic", "cellSize": 5, "name": "w", "path": path.display().to_string()}),
        )
        .unwrap();
    assert_eq!(v["cog"]["overviews"], 0, "200 × 160 is one tile: {v}");
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(200), Some(160)), "half-size cells double the grid");
    let back = rustgis_data::raster::read_path(&path).unwrap();
    assert_eq!((back.width, back.height, back.crs.epsg()), (200, 160, 32613));
    assert_eq!(back.transform.origin_x, 500_000.0);
    let layer = s.map().layer_by_name("w").unwrap().raster.as_ref().unwrap();
    assert_eq!(layer.path.as_deref(), Some(path.display().to_string().as_str()), "the file is the layer's source");
    // The same command without a transform is the old north-up fit.
    s.run("raster.georeference", &json!({"layer": "scan", "extent": [10.0, 45.0, 10.1, 45.08]})).unwrap();
    let ok = json!({"layer": "scan", "transform": "affine", "pointsCrs": 32613, "crs": 32613, "points": points_of(map)});
    s.run("raster.georeference", &ok).unwrap();
    for (k, bad) in [
        ("transform", json!("warp")),
        ("transform", json!(7)),
        ("crs", json!("EPSG:99999999")),
        ("crs", json!(-1)),
        ("crs", json!(1e30)),
        ("pointsCrs", json!([])),
        ("resampling", json!("lanczos")),
        ("cellSize", json!(0)),
        ("cellSize", json!(-3)),
        ("cellSize", json!(1e-9)),
        ("cellSize", json!("x")),
        ("path", json!("warped.png")),
        ("path", json!(5)),
        ("points", json!([[0, 0, 1, 1]])),
    ] {
        let mut p = ok.clone();
        p[k] = bad.clone();
        assert!(s.run("raster.georeference", &p).is_err(), "{k} = {bad}");
    }
    // A fit that collapses the image onto a line has no grid.
    assert!(
        s.run(
            "raster.georeference",
            &json!({"layer": "scan", "transform": "affine", "pointsCrs": 32613, "crs": 32613, "points": points_of(|c, _| (c, c))})
        )
        .is_err()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A scan opened from a PNG and warped onto its points has coordinates for real: the cell is the
/// fit's pixel on the ground, not the stand-in size it was laid over the view at; the pixels land
/// where the fit says; it can be exported; and the project reopens it where the warp put it, not
/// laid over the view again.
#[test]
fn a_scan_warped_onto_its_points_is_placed_for_real() {
    let dir = std::env::temp_dir().join(format!("rustgis-scan-warp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("plate.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(40, 20, |x, y| image::Rgb([x as u8, y as u8, 9])))
        .save_with_format(&png, image::ImageFormat::Png)
        .unwrap();
    let mut s = world();
    let r = s.run("layer.addData", &json!({"path": png.display().to_string()})).unwrap();
    assert_eq!(r["georeferenced"], false, "{r}");
    let laid = s.map().layer_by_name("plate").unwrap().raster.clone().unwrap();
    assert!(laid.placement.is_some());
    let stand_in = laid.data.as_ref().unwrap().transform.pixel_width;
    // 7.5 m a pixel in UTM 13N, from the scan's corners and two points inside it; the output's
    // system is the points'.
    let map = |c: f64, r: f64| (500_000.0 + 7.5 * c, 4_000_000.0 - 7.5 * r);
    let pts: Vec<Value> = [(0.0, 0.0), (40.0, 0.0), (0.0, 20.0), (40.0, 20.0), (20.0, 10.0), (10.0, 15.0)]
        .iter()
        .map(|&(c, r)| {
            let (x, y) = map(c, r);
            json!([c, r, x, y])
        })
        .collect();
    let v = s
        .run(
            "raster.georeference",
            &json!({"layer": "plate", "transform": "affine", "pointsCrs": 32613, "points": pts, "resampling": "nearest"}),
        )
        .unwrap();
    assert_eq!(v["crs"], "EPSG:32613", "{v}");
    let l = s.map().layer_by_name("plate").unwrap().raster.clone().unwrap();
    assert!(l.placement.is_none(), "no longer laid over the view");
    let out = l.data.clone().unwrap();
    assert!(out.georeferenced && out.crs.epsg() == 32613);
    assert!(
        (out.transform.pixel_width - 7.5).abs() < 1e-6 && (out.transform.pixel_height + 7.5).abs() < 1e-6 && (stand_in - 7.5).abs() > 0.5,
        "{:?}, laid at {stand_in}",
        out.transform
    );
    // Red is the column and green the row, so a pixel reads back where the fit puts it; the scan
    // had no alpha or no data, so it gains an opaque alpha band.
    assert_eq!(out.band_count(), 4);
    let (x, y) = map(10.5, 5.5);
    let (col, row) = out.transform.to_pixel(rustgis_geom::Coord { x, y });
    let (col, row) = (col.floor() as i64, row.floor() as i64);
    let px: Vec<Option<f32>> = (0..4).map(|b| out.value(b, col, row)).collect();
    assert_eq!(px, [Some(10.0), Some(5.0), Some(9.0), Some(255.0)], "cell {col},{row}");
    // Exporting is no longer refused.
    let tif = dir.join("plate.tif");
    s.run("share.exportRaster", &json!({"layer": "plate", "path": tif.display().to_string(), "cog": true})).unwrap();
    let back = rustgis_data::raster::read_path(&tif).unwrap();
    assert!(back.georeferenced && back.crs.epsg() == 32613 && back.transform == out.transform);
    // The project reopens it where the warp put it.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("plate").unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert!(again.georeferenced && again.transform == out.transform, "{:?} vs {:?}", again.transform, out.transform);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn exporting_a_raster_as_a_cog() {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    let dir = std::env::temp_dir().join(format!("rustgis-cog-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (plain, cog) = (dir.join("plain.tif"), dir.join("cog.tif"));
    let v = s.run("share.exportRaster", &json!({"layer": dem, "path": plain.display().to_string()})).unwrap();
    assert!(v.get("cog").is_none(), "an untiled GeoTIFF unless asked");
    let v = s.run("share.exportRaster", &json!({"layer": dem, "path": cog.display().to_string(), "cog": true})).unwrap();
    assert_eq!(v["cog"]["bigTiff"], false, "{v}");
    let (a, b) = (rustgis_data::raster::read_path(&plain).unwrap(), rustgis_data::raster::read_path(&cog).unwrap());
    assert_eq!((a.width, a.height, a.transform), (b.width, b.height, b.transform));
    assert!(a.bands.iter().zip(&b.bands).all(|(x, y)| x.data == y.data));
    // Tiled, not stripped: the tile tags are in the file.
    let bytes = std::fs::read(&cog).unwrap();
    assert!(bytes.windows(2).any(|w| w == [0x42, 0x01]), "TileWidth (322) is in a directory");
    assert!(s.run("share.exportRaster", &json!({"layer": dem, "path": cog.display().to_string(), "cog": true, "band": 99})).is_err());
    assert!(
        s.run("share.exportRaster", &json!({"layer": dem, "path": dir.join("no").join("x.tif").display().to_string(), "cog": true}))
            .is_err()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn layer_effects_and_imagery_tools() {
    use crate::render::View;
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 48.0, "scale": 20_000_000})).unwrap();
    let view = |s: &Session| View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    let png = |s: &mut Session| {
        let v = view(s);
        crate::render::render(s.map_mut(), &v).unwrap().pixmap
    };
    // Every effect renders.
    s.run("layer.blend", &json!({"layer": "Countries", "mode": "multiply"})).unwrap();
    assert!(s.run("layer.blend", &json!({"layer": "Countries", "mode": "dodge"})).is_err());
    s.run("layer.mask", &json!({"layer": "Cities", "mask": "Countries"})).unwrap();
    assert!(s.run("layer.mask", &json!({"layer": "Countries", "mask": "Countries"})).is_err());
    assert!(s.run("layer.mask", &json!({"layer": "Countries", "mask": "Cities"})).is_err(), "masks are polygons");
    s.run("layer.swipe", &json!({"layer": "Countries", "position": 0.5})).unwrap();
    let swiped = png(&mut s);
    // Right of the divider the countries are gone: only water and basemap land remain.
    s.run("layer.swipe", &json!({"layer": "Countries", "off": true})).unwrap();
    let full = png(&mut s);
    assert_ne!(swiped.data(), full.data());
    s.run("layer.displayFilters", &json!({"layer": "Cities", "filters": [{"name": "Big at small scales", "minScale": 1e12, "maxScale": 5_000_000, "where": "pop_max > 5000000"}]})).unwrap();
    assert!(s.run("layer.displayFilters", &json!({"layer": "Cities", "filters": [{"where": "(("}]})).is_err());
    s.run("layer.aggregation", &json!({"layer": "Cities", "shape": "hexagon", "size": 30})).unwrap();
    assert!(s.run("layer.aggregation", &json!({"layer": "Countries", "shape": "hexagon"})).is_err());
    png(&mut s);
    s.run("layer.aggregation", &json!({"layer": "Cities", "shape": "square"})).unwrap();
    png(&mut s);
    s.run("layer.extrusion", &json!({"layer": "Countries", "type": "field", "field": "pop_est", "factor": 0.002})).unwrap();
    s.run("layer.faces", &json!({"layer": "Countries", "culling": false, "lighting": true})).unwrap();
    s.run("layer.lightShadow", &json!({"layer": "Countries", "shadows": true})).unwrap();
    let extruded = png(&mut s);
    assert_ne!(extruded.data(), full.data());
    assert!(s.run("layer.faces", &json!({"layer": "Lakes", "culling": true})).is_err(), "no extrusion yet");
    s.run("raster.rotation", &json!({"layer": dem, "angle": 30})).unwrap();
    s.run("map.goToXY", &json!({"lon": 10.02, "lat": 45.02, "scale": 50_000})).unwrap();
    png(&mut s);
    assert!(s.run("raster.rotation", &json!({"layer": "Countries", "angle": 30})).is_err());
    s.run("raster.rotation", &json!({"layer": dem, "angle": 0})).unwrap();
    // Imagery.
    let r = s.run("raster.georeference", &json!({"layer": dem, "points": [[0, 0, 20.0, 50.0], [40, 40, 20.4, 49.6]]})).unwrap();
    let e = r["extent"].as_array().unwrap();
    assert!((e[0].as_f64().unwrap() - 20.0).abs() < 1e-6 && (e[3].as_f64().unwrap() - 50.0).abs() < 1e-6, "{r}");
    assert!(
        s.run("raster.georeference", &json!({"layer": dem, "points": [[0, 0, 20.0, 50.0], [40, 40, 19.0, 51.0]]})).is_err(),
        "north up only"
    );
    s.run("raster.georeference", &json!({"layer": dem, "extent": [10.0, 45.0, 10.04, 45.04]})).unwrap();
    // Control point fits: a sheared grid is affine, a point switched off still gets a residual,
    // and comparing the transforms recommends the simplest that predicts the points left out.
    let sheared: Vec<Value> = (0..16)
        .map(|i| {
            let (c, r) = (f64::from(i % 4) * 100.0, f64::from(i / 4) * 100.0);
            json!([c, r, 20.0 + c * 0.001 + r * 0.0002, 50.0 - r * 0.001])
        })
        .collect();
    let mut pts = sheared.clone();
    pts[5] = json!([100, 100, 20.5, 49.0, 0]);
    let fit = s.run("raster.gcpFit", &json!({"points": pts, "transform": "affine"})).unwrap();
    assert_eq!(fit["used"], 15);
    assert!(fit["rmse"]["map"].as_f64().unwrap() < 1e-9, "{fit}");
    assert!(fit["residuals"][5]["map"][0].as_f64().unwrap() < -0.3, "the switched-off point misses");
    let g: Vec<f64> = fit["affine"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!((g[1] - 0.001).abs() < 1e-12 && (g[2] - 0.0002).abs() < 1e-12 && (g[5] + 0.001).abs() < 1e-12, "{g:?}");
    let cmp = s.run("raster.gcpFit", &json!({"points": sheared, "compare": true})).unwrap();
    assert_eq!(cmp["recommended"], "affine", "{cmp}");
    assert!(s.run("raster.gcpFit", &json!({"points": [[0, 0, 1, 1]]})).is_err());
    assert!(s.run("raster.gcpFit", &json!({"points": [[0, 0, 1, 1], [1, 1, 2, 2]], "transform": "magic"})).is_err());
    let sp = s.run("raster.spectral", &json!({"layer": dem, "lon": 10.0205, "lat": 45.0195})).unwrap();
    assert!(sp["values"][0].as_f64().unwrap() > 290.0);
    s.run("raster.index", &json!({"layer": dem, "index": "ndvi", "nir": 1, "red": 1})).unwrap();
    let r = s.run("raster.classify", &json!({"layer": dem, "classes": 3})).unwrap();
    // Forms send numbers as floats; whole ones are accepted.
    s.run("raster.classify", &json!({"layer": dem, "classes": 3.0, "name": "float_classes"})).unwrap();
    assert!(s.run("raster.classify", &json!({"layer": dem, "classes": 2.5})).is_err());
    assert!(r["layer"].as_u64().is_some());
    s.run("raster.function", &json!({"layer": dem, "function": "bandMath", "expression": "b1 + 10", "name": "dem_later"})).unwrap();
    let ch = s.run("raster.change", &json!({"before": dem, "after": "dem_later", "method": "difference"})).unwrap();
    let ch_layer = s.map().layer(ch["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert!((ch_layer.bands[0].stats.mean - 10.0).abs() < 0.01);
    s.run("raster.process", &json!({"layer": dem, "operation": "clip", "extent": [10.005, 45.005, 10.02, 45.02]})).unwrap();
    let r = s.run("raster.process", &json!({"layer": dem, "operation": "resample", "factor": 4})).unwrap();
    let small = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert_eq!((small.width, small.height), (10, 10));
    let r = s
        .run(
            "raster.editPixels",
            &json!({"layer": dem, "polygon": [[10.0, 45.0], [10.01, 45.0], [10.01, 45.01], [10.0, 45.01]], "value": 0}),
        )
        .unwrap();
    assert_eq!(r["pixels"], 100);
    // Supervised classification from two training squares.
    s.run("layer.template", &json!({"template": "polygons", "name": "Training"})).unwrap();
    for (name, x) in [("low", 10.002), ("high", 10.018)] {
        s.run("edit.addFeature", &json!({"layer": "Training", "geometry": {"type": "Polygon", "coordinates": [[[x, 45.018], [x + 0.004, 45.018], [x + 0.004, 45.022], [x, 45.022], [x, 45.018]]]}, "attributes": {"name": name}})).unwrap();
    }
    let r = s.run("raster.classifySupervised", &json!({"layer": dem, "training": "Training", "classField": "name"})).unwrap();
    assert_eq!(r["classes"].as_array().unwrap().len(), 2, "{r}");
    // Mensuration.
    let m = s.run("raster.measure", &json!({"layer": dem, "kind": "height", "points": [[10.0015, 45.0385], [10.0205, 45.0195]]})).unwrap();
    assert!(m["metres"].as_f64().unwrap() > 50.0, "{m}");
    let volume =
        json!({"layer": dem, "kind": "volume", "points": [[10.015, 45.015], [10.025, 45.015], [10.025, 45.025], [10.015, 45.025]]});
    let on_earth = s.run("raster.measure", &volume).unwrap()["cubicMetresAbove"].as_f64().unwrap();
    s.run("raster.measure", &json!({"kind": "area", "points": [[0, 0], [1, 0], [1, 1]]})).unwrap();
    assert_eq!(s.run("raster.measurements", &Value::Null).unwrap().as_array().unwrap().len(), 3);
    // On Mars the degree cells of this raster are Mars' size: the same heights over a smaller
    // ground (#223).
    s.run("map.body", &json!({"name": "mars", "basemap": false})).unwrap();
    let on_mars = s.run("raster.measure", &volume).unwrap()["cubicMetresAbove"].as_f64().unwrap();
    let k = rustgis_geom::Body::get("mars").ratio();
    assert!(on_earth > 0.0 && (on_mars / on_earth - k * k).abs() < 1e-9, "{on_earth} {on_mars}");
    s.run("map.body", &json!({"name": "earth"})).unwrap();
    // Workspaces: an orthomosaic of two rasters, a DSM mosaic and its mesh.
    s.run("imagery.workspace", &json!({"kind": "ortho", "name": "Valley", "images": [dem, "dem_later"]})).unwrap();
    s.run("imagery.workspaceOptions", &json!({"name": "Valley", "cellSize": 0.002})).unwrap();
    let r = s.run("imagery.mosaic", &json!({"name": "Valley"})).unwrap();
    let mo = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert_eq!((mo.width, mo.height), (20, 20));
    s.run("imagery.workspace", &json!({"kind": "reality", "images": ["dem_later"]})).unwrap();
    let r = s.run("imagery.mosaic", &json!({"name": "Reality Mapping"})).unwrap();
    let dsm = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().name.clone();
    let mesh = s.run("imagery.mesh", &json!({"layer": dsm})).unwrap();
    assert!(mesh["faces"].as_u64().unwrap() > 100);
    // Meshes go to .obj files; workspace methods are first or mean, checked on creation too.
    assert!(s.run("imagery.mesh", &json!({"layer": dsm, "path": "/tmp/dsm.png"})).is_err());
    assert!(s.run("imagery.workspace", &json!({"kind": "ortho", "images": [dem], "method": "median"})).is_err());
    assert!(s.run("imagery.workspace", &json!({"kind": "ortho", "images": [dem], "cellSize": 20})).is_err());
    // A mean mosaic keeps fractional values.
    s.run("imagery.workspaceOptions", &json!({"name": "Valley", "method": "mean"})).unwrap();
    let r = s.run("imagery.mosaic", &json!({"name": "Valley"})).unwrap();
    let mo = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert_eq!(mo.data_type, "Float32");
    assert!(s.run("imagery.workspace", &json!({"kind": "lidar", "images": [dem]})).is_err());
    // Effects and workspaces save with the project.
    let p = crate::Project::from_json(&serde_json::to_string(&s.project).unwrap()).unwrap();
    assert!(p.map().layer_by_name("Countries").unwrap().extrusion.is_some() && p.workspaces.len() == 2);
}

#[test]
fn the_globe_has_stars_that_turn_off() {
    use crate::render::View;
    let mut s = world();
    s.run("map.viewMode", &json!({"mode": "global"})).unwrap();
    // The space in the corner, away from the globe.
    let corner = |s: &mut Session| {
        let v = View { width: 1200.0, height: 800.0, pixels_per_point: 1.0, camera: s.map().camera, sky: true };
        let pm = crate::render::render(s.map_mut(), &v).unwrap().pixmap;
        let mut px = Vec::new();
        for y in 0..150 {
            for x in 0..150 {
                px.push(pm.pixel(x, y).map(|c| (c.red(), c.green(), c.blue())));
            }
        }
        px
    };
    let starry = corner(&mut s);
    assert!(s.map().view.effects.stars && starry.iter().any(|p| p != &starry[0]), "stars by default");
    s.run("map.effects", &json!({"stars": false})).unwrap();
    let plain = corner(&mut s);
    assert!(plain.iter().all(|p| p == &plain[0]), "plain space");
    // Saved with the project; a project from before stars has them.
    let p = crate::Project::from_json(&serde_json::to_string(&s.project).unwrap()).unwrap();
    assert!(!p.map().view.effects.stars);
    assert!(serde_json::from_str::<rustgis_map::view::ViewEffects>("{}").unwrap().stars);
    s.run("map.effects", &json!({"reset": true})).unwrap();
    assert!(corner(&mut s) == starry);
}

#[test]
fn exports_draw_the_globe_in_light_space() {
    // #352: the dark, starry space is the interactive view's; exports and layouts print a light
    // one, as globe insets on a light page need.
    let mut s = world();
    s.run("map.viewMode", &json!({"mode": "global"})).unwrap();
    let rgb = |pm: &tiny_skia::Pixmap, x: u32, y: u32| pm.pixel(x, y).map(|c| (c.red(), c.green(), c.blue())).unwrap();
    let corner = |s: &mut Session| {
        let png = crate::cmd::share::map_png(s, Some(1200.0), Some(800.0), None).unwrap();
        let pm = crate::render::decode_png(&png).unwrap();
        let px: Vec<_> = (0..100).flat_map(|y| (0..100).map(move |x| (x, y))).map(|(x, y)| rgb(&pm, x, y)).collect();
        px
    };
    let light = corner(&mut s);
    assert!(light.iter().all(|p| *p == (0xEC, 0xEF, 0xF3)), "light space without stars: {:?}", light[0]);
    // The Midnight basemap keeps a dark space, still without stars.
    s.run("map.basemap", &json!({"name": "midnight"})).unwrap();
    let dark = corner(&mut s);
    assert!(dark.iter().all(|p| *p == (0x08, 0x0B, 0x10)), "dark space without stars: {:?}", dark[0]);
    s.run("map.basemap", &json!({"name": "paper"})).unwrap();
    // A layout's map frame on the globe map: the space in its corner is light too.
    s.run("layout.new", &json!({"page": "a4", "orientation": "landscape", "template": "blank"})).unwrap();
    s.run("layout.addFrame", &json!({"x": 20, "y": 20, "w": 150, "h": 150})).unwrap();
    let dir = std::env::temp_dir().join(format!("rustgis-globe-space-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("inset.png");
    s.run("layout.export", &json!({"path": path.display().to_string(), "dpi": 72})).unwrap();
    let pm = crate::render::decode_png(&std::fs::read(&path).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // 25 mm from the page's corner: 5 mm inside the frame's.
    let at = (25.0 * 72.0 / 25.4) as u32;
    assert_eq!(rgb(&pm, at, at), (0xEC, 0xEF, 0xF3), "a light space around the globe in the layout");
}

#[test]
fn view_effects_scene_and_navigation() {
    use crate::render::View;
    let mut s = world();
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 48.0, "scale": 20_000_000})).unwrap();
    let png = |s: &mut Session| {
        let v = View { width: 300.0, height: 200.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
        crate::render::render(s.map_mut(), &v).unwrap().pixmap.data().to_vec()
    };
    let plain = png(&mut s);
    for p in [
        json!({"sunTime": 1_710_979_200.0}),
        json!({"weather": "fog"}),
        json!({"weather": "snow", "vignette": true, "edges": true}),
        json!({"tiltShift": true, "grain": true}),
        json!({"brightness": 20, "contrast": 10, "saturation": -50, "sepia": 30}),
    ] {
        s.run("map.effects", &p).unwrap_or_else(|e| panic!("{p}: {e}"));
        assert!(png(&mut s) != plain, "{p}");
    }
    assert!(s.run("map.effects", &json!({"weather": "hail"})).is_err());
    s.run("map.effects", &json!({"reset": true})).unwrap();
    assert!(png(&mut s) == plain, "reset restores the map");
    s.run("map.colorVision", &json!({"mode": "deuteranopia"})).unwrap();
    assert!(png(&mut s) != plain);
    // Exports aren't simulated.
    let export = crate::cmd::share::map_png(&mut s, Some(300.0), Some(200.0), None).unwrap();
    s.run("map.colorVision", &json!({"mode": "normal"})).unwrap();
    assert!(export == crate::cmd::share::map_png(&mut s, Some(300.0), Some(200.0), None).unwrap());
    s.run("map.drawingMode", &json!({"mode": "wireframe"})).unwrap();
    assert!(png(&mut s) != plain);
    s.run("map.drawingMode", &json!({"mode": "standard"})).unwrap();
    s.run("layer.priority", &json!({"layer": "Countries", "value": 5})).unwrap();
    assert!(png(&mut s) != plain, "countries now draw over the cities");
    s.run("layer.priority", &json!({"layer": "Countries", "value": 0})).unwrap();
    s.run("map.clipping", &json!({"mode": "distance", "distance": 500_000})).unwrap();
    assert!(png(&mut s) != plain);
    s.run("map.clipping", &json!({"mode": "none"})).unwrap();
    s.run("map.displayType", &json!({"mode": "speed"})).unwrap();
    assert!(png(&mut s) != plain, "no antialiasing");
    s.run("map.displayType", &json!({"mode": "quality"})).unwrap();
    assert!(png(&mut s) == plain);
    // Views and navigation.
    if rustgis_map::projection::local_views_supported() {
        s.run("map.viewMode", &json!({"mode": "local"})).unwrap();
        assert!(s.map().projection.code().starts_with("local:"));
        let local = png(&mut s);
        // The local view is azimuthal equidistant, not Web Mercator under another name (#208).
        s.run("map.projection", &json!({"type": "mercator"})).unwrap();
        assert!(png(&mut s) != local, "a local view draws in its own projection");
    } else {
        let e = s.run("map.viewMode", &json!({"mode": "local"})).unwrap_err();
        assert!(e.contains("web app"), "{e}");
    }
    s.run("map.viewMode", &json!({"mode": "global"})).unwrap();
    assert!(s.map().projection.is_globe());
    s.run("map.navigation", &json!({"mode": "planar"})).unwrap();
    assert!(!s.map().projection.is_globe(), "planar navigation is flat");
    s.run("map.navigation", &json!({"mode": "firstPerson"})).unwrap();
    let before = s.map().center_lonlat();
    s.run("map.walk", &json!({"forward": 1})).unwrap();
    assert!(s.map().center_lonlat().y > before.y, "walking forward goes north");
    let cam = s.run("map.camera", &json!({"lon": 2.35, "lat": 48.85, "scale": 1_000_000})).unwrap();
    assert!((cam["lon"].as_f64().unwrap() - 2.35).abs() < 1e-6);
    assert!(s.run("map.navigator", &Value::Null).unwrap()["navigator"].as_bool().unwrap());
}

#[test]
fn workspace_links_thumbnails_animation_profiles_offline_and_cache() {
    let dir = std::env::temp_dir().join(format!("rustgis-ws-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    // Linked views.
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    s.run("map.activate", &json!({"name": "World"})).unwrap();
    s.run("view.linkViews", &json!({"on": true})).unwrap();
    s.run("map.goToXY", &json!({"lon": 12.5, "lat": 41.9, "scale": 2_000_000})).unwrap();
    let second = s.project.maps.iter().find(|m| m.name == "Second").unwrap();
    let c = second.center_lonlat();
    assert!((c.x - 12.5).abs() < 1e-6 && (second.scale() / 2_000_000.0 - 1.0).abs() < 0.01, "{c:?}");
    // Thumbnails.
    s.run("project.thumbnail", &json!({"create": true})).unwrap();
    assert!(s.project.thumbnail.as_ref().is_some_and(|t| t.len() > 100));
    s.run("map.thumbnail", &json!({"image": s.project.thumbnail.clone().unwrap()})).unwrap();
    assert!(s.run("map.thumbnail", &json!({"image": "bm90"})).is_err());
    s.run("project.thumbnail", &json!({"clear": true})).unwrap();
    assert!(s.project.thumbnail.is_none());
    // Jobs.
    let j = s.run("job.new", &json!({"name": "Check roads", "viewExtent": true})).unwrap()["job"].as_u64().unwrap();
    s.run("job.update", &json!({"job": j, "status": "in progress", "assigned": "field team"})).unwrap();
    assert!(s.run("job.update", &json!({"job": j, "status": "abandoned"})).is_err());
    assert_eq!(s.project.jobs[0].status, "in progress");
    // Animation: two keyframes, exported as frames.
    s.run("animation.add", &Value::Null).unwrap();
    s.run("map.goToXY", &json!({"lon": 2.35, "lat": 48.85, "scale": 4_000_000})).unwrap();
    s.run("animation.add", &json!({"seconds": 1})).unwrap();
    let path = dir.join("anim.zip");
    s.run("animation.export", &json!({"path": path.display().to_string(), "fps": 5, "width": 160, "height": 100})).unwrap();
    let frames = rustgis_data::zip::read(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(frames.len(), 6, "five frames for the second, then the last view");
    assert!(frames.iter().all(|(n, b)| n.ends_with(".png") && b.starts_with(b"\x89PNG")));
    s.run("animation.remove", &json!({"all": true})).unwrap();
    // Profiles over the hill.
    let p = s.run("profile.create", &json!({"layer": dem, "points": [[10.0015, 45.0195], [10.0385, 45.0195]]})).unwrap();
    assert!(p["max"].as_f64().unwrap() > 380.0 && p["gain"].as_f64().unwrap() > 150.0, "{}", p["max"]);
    let auto = s.run("profile.autoComplete", &json!({"layer": dem, "points": [[10.0015, 45.0195], [10.005, 45.0195]]})).unwrap();
    assert!(auto["length"].as_f64().unwrap() > 2500.0, "continues to the edge");
    let cross =
        s.run("profile.crossSection", &json!({"layer": dem, "points": [[10.0015, 45.0195], [10.0385, 45.0195]], "width": 2000})).unwrap();
    assert!((cross["length"].as_f64().unwrap() - 2000.0).abs() < 50.0);
    s.run("layer.template", &json!({"template": "lines", "name": "Trails"})).unwrap();
    s.run(
        "edit.addFeature",
        &json!({"layer": "Trails", "geometry": {"type": "LineString", "coordinates": [[10.002, 45.002], [10.038, 45.038]]}}),
    )
    .unwrap();
    let along = s.run("profile.alongLine", &json!({"layer": dem, "lines": "Trails"})).unwrap();
    assert!(along["samples"].as_array().unwrap().len() > 10);
    assert_eq!(s.run("profile.alongSelected", &json!({"layer": dem, "lines": "Trails"})).unwrap().as_array().unwrap().len(), 1);
    // Infographics and diagnostics.
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 48.0, "scale": 20_000_000})).unwrap();
    let info = s.run("info.infographic", &Value::Null).unwrap();
    assert!(info["layers"].as_array().unwrap().iter().any(|c| c["layer"] == "Cities" && c["count"].as_u64().unwrap() > 10));
    let d = s.run("info.diagnostics", &Value::Null).unwrap();
    assert!(d["features"].as_u64().unwrap() > 100 && d["rasterPixels"].as_u64().unwrap() == 1600);
    // Offline: download, edit offline, sync back.
    let r = s.run("offline.download", &json!({"name": "Paris", "extent": [1.0, 48.0, 4.0, 50.0]})).unwrap();
    assert!(r["features"].as_u64().unwrap() >= 2);
    s.run("offline.open", &json!({"name": "Paris"})).unwrap();
    assert_eq!(s.map().name, "Paris (offline)");
    s.run("select.byAttributes", &json!({"layer": "Cities", "where": "name = 'Paris'"})).unwrap();
    let row = s.map().layer_by_name("Cities").unwrap().selection[0];
    s.run("edit.setValue", &json!({"layer": "Cities", "row": row, "field": "pop_max", "value": 1})).unwrap();
    s.run(
        "edit.addFeature",
        &json!({"layer": "Cities", "geometry": {"type": "Point", "coordinates": [2.0, 49.0]}, "attributes": {"name": "Field camp"}}),
    )
    .unwrap();
    // Meanwhile France is edited in the source map; the area's untouched copy mustn't undo that.
    let set_france = |s: &mut Session, v: i64| {
        let world = s.project.maps.iter_mut().find(|m| m.name == "World").unwrap();
        let countries = world.layers.iter_mut().find(|l| l.name == "Countries").unwrap();
        let (ni, pi) = (countries.dataset.field_index("name").unwrap(), countries.dataset.field_index("pop_est").unwrap());
        let f = countries.dataset.features.iter_mut().find(|f| f.attrs[ni].to_string() == "France").unwrap();
        if v >= 0 {
            f.attrs[pi] = rustgis_data::Value::Int(v);
        }
        f.attrs[pi].clone()
    };
    set_france(&mut s, 12_345);
    let sync = s.run("offline.sync", &json!({"name": "Paris"})).unwrap();
    assert_eq!((sync["updated"].as_u64(), sync["added"].as_u64()), (Some(1), Some(1)), "{sync}");
    assert_eq!(set_france(&mut s, -1), rustgis_data::Value::Int(12_345), "the source edit survives the sync");
    let world_map = s.project.maps.iter().find(|m| m.name == "World").unwrap();
    let cities = world_map.layer_by_name("Cities").unwrap();
    assert!(cities.dataset.features.iter().any(|f| f.attrs.iter().any(|v| v.to_string() == "Field camp")));
    s.run("offline.remove", &json!({"name": "Paris"})).unwrap();
    assert!(s.project.maps.iter().all(|m| m.name != "Paris (offline)"));
    // Feature cache.
    s.run("map.activate", &json!({"name": "World"})).unwrap();
    s.run("cache.empty", &Value::Null).unwrap();
    assert_eq!(s.run("info.diagnostics", &Value::Null).unwrap()["cachedLayers"], 0);
    s.run("cache.fill", &Value::Null).unwrap();
    assert!(s.run("info.diagnostics", &Value::Null).unwrap()["cachedLayers"].as_u64().unwrap() >= 4);
    s.run("cache.auto", &json!({"on": true})).unwrap();
    s.run("map.projection", &json!({"type": "mollweide"})).unwrap();
    assert!(s.map().layers.iter().filter(|l| !l.is_raster()).all(|l| l.is_cached("mollweide")));
    s.run("cache.cancel", &Value::Null).unwrap();
    assert!(!s.auto_cache);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// plugins

/// Loads the test plugin as `id` with `perms`.
fn plugin(id: &str, perms: &[&str]) {
    let pkg = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package(id, perms)).unwrap();
    rustgis_plugins::load(pkg, "test").unwrap();
}

/// Runs the test plugin's forwarding command: it sends `op` with `params` to the host.
fn forward(s: &mut Session, id: &str, op: &str, params: Value) -> crate::CmdResult {
    s.run(&format!("plugin:{id}/f"), &json!({"op": op, "params": params}))
}

#[test]
fn plugin_commands_run_like_commands() {
    let mut s = world();
    plugin("eng-view", &[]);
    assert_eq!(s.run("plugin:eng-view/h", &Value::Null).unwrap(), json!({"hello": "world"}));
    assert!(!s.can_undo(), "a plugin without 'edit' doesn't take undo snapshots");
    // Reading the map needs no permission.
    let layers = forward(&mut s, "eng-view", "host.layers", json!({})).unwrap();
    assert!(layers.as_array().unwrap().iter().any(|l| l["name"] == "Cities"));
    let fc = forward(&mut s, "eng-view", "host.features", json!({"layer": "Cities", "limit": 2})).unwrap();
    assert_eq!(fc["features"].as_array().unwrap().len(), 2);
    assert_eq!(fc["features"][1]["row"], 1);
    assert!(forward(&mut s, "eng-view", "map.zoomIn", json!({})).is_ok(), "view commands are fine");
    // Changing the project isn't.
    let e = forward(&mut s, "eng-view", "layer.remove", json!({"layer": "Cities"})).unwrap_err();
    assert!(e.contains("'edit' permission"), "{e}");
    let e = forward(&mut s, "eng-view", "host.addLayer", json!({"geojson": {"type": "Point", "coordinates": [1, 2]}})).unwrap_err();
    assert!(e.contains("'edit' permission"), "{e}");
    assert!(s.map().layer_by_name("Cities").is_some());
    // Listed with the built-in commands; unknown and disabled ones are errors.
    let all = s.run("info.commands", &Value::Null).unwrap();
    assert!(all.as_array().unwrap().iter().any(|c| c["id"] == "plugin:eng-view/h"));
    assert!(s.run("plugin:eng-view/nope", &Value::Null).unwrap_err().contains("unknown command"));
    s.run("plugin.enable", &json!({"id": "eng-view", "enabled": false})).unwrap();
    assert!(s.run("plugin:eng-view/h", &Value::Null).unwrap_err().contains("disabled"));
    rustgis_plugins::unload("eng-view");
}

#[test]
fn plugin_edits_are_one_undo_step() {
    let mut s = world();
    plugin("eng-edit", &["edit"]);
    let r =
        forward(&mut s, "eng-edit", "host.addLayer", json!({"name": "Pins", "geojson": {"type": "Point", "coordinates": [1, 2]}})).unwrap();
    assert!(r["layer"].as_u64().is_some());
    assert!(s.map().layer_by_name("Pins").is_some());
    assert_eq!(s.undo_label(), Some("Forward"));
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layer_by_name("Pins").is_none());
    forward(&mut s, "eng-edit", "layer.remove", json!({"layer": "Cities"})).unwrap();
    assert!(s.map().layer_by_name("Cities").is_none());
    // A failing command restores the project.
    let n = s.map().layers.len();
    assert!(forward(&mut s, "eng-edit", "layer.remove", json!({"layer": "Nope"})).is_err());
    assert_eq!(s.map().layers.len(), n);
    // Some things are off limits whatever the permissions.
    for op in ["script.run", "project.new", "edit.undo", "plugin.uninstall", "plugin:eng-edit/h", "gp.setGeolibre", "gp.uninstallGeolibre"]
    {
        assert!(forward(&mut s, "eng-edit", op, json!({})).is_err(), "{op}");
    }
    // Files need their own permission.
    let e = forward(&mut s, "eng-edit", "layer.addData", json!({"path": "/etc/passwd"})).unwrap_err();
    assert!(e.contains("'files' permission"), "{e}");
    for (op, params) in [
        ("share.exportMap", json!({})),
        ("project.save", json!({})),
        ("layer.rename", json!({"layer": "Cities", "name": "x", "output": 1})),
        ("layer.rename", json!({"layer": "Cities", "name": "https://example.com/x"})),
        ("layer.rename", json!({"layer": "Cities", "name": "../../x"})),
        ("layer.rename", json!({"layer": "Cities", "name": {"nested": {"url": "x"}}})),
    ] {
        let e = forward(&mut s, "eng-edit", op, params.clone()).unwrap_err();
        assert!(e.contains("'files' permission"), "{op} {params}: {e}");
    }
    assert!(forward(&mut s, "eng-edit", "layer.rename", json!({"layer": "Rivers", "name": "Streams"})).is_ok());
    rustgis_plugins::unload("eng-edit");
}

#[test]
fn plugin_panels_keep_their_state_between_calls() {
    let mut s = world();
    let pkg = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::panel_package("eng-panel")).unwrap();
    rustgis_plugins::load(pkg, "test").unwrap();
    // Opening it runs its state command, whose answer the session keeps.
    let st = s.run("plugin.panel", &json!({"id": "eng-panel", "refresh": true})).unwrap();
    assert_eq!(st["values"]["release"], 12);
    assert_eq!(st["items"]["release"]["labels"][12], "2025-12-18");
    assert_eq!(st["items"]["newer"]["enabled"], false);
    assert!(!s.can_undo(), "a state command that changes nothing is no undo step");
    // A widget's command gets the values, the one it set among them, and not as a parameter.
    let got = s.run("plugin:eng-panel/q", &json!({"panel": {"release": 4, "nope": 1}, "keep": true})).unwrap();
    assert_eq!(got["panel"], json!({"values": {"local": false, "release": 4}, "changed": ["release"]}));
    assert_eq!(got["params"], json!({"keep": true}));
    assert_eq!(s.plugin_panels["eng-panel"].slider("release"), 4);
    // Any command gets them, from the ribbon or a script, and widgets that run nothing set them.
    s.run("plugin.panel", &json!({"id": "eng-panel", "values": {"local": true, "release": 99}})).unwrap();
    let got = s.run("plugin:eng-panel/h", &json!({})).unwrap();
    assert_eq!(got, json!({"hello": "world"}));
    let got = s.run("plugin:eng-panel/q", &json!({})).unwrap();
    assert_eq!(got["panel"], json!({"values": {"local": true, "release": 12}, "changed": []}), "kept within the slider's ends");
    // A failed command leaves the values as they were.
    assert!(s.run("plugin:eng-panel/e", &json!({"panel": {"release": 1}})).is_err());
    assert_eq!(s.plugin_panels["eng-panel"].slider("release"), 12);
    // Hostile values and answers never panic, and never leave the slider outside its ends.
    for junk in [json!(null), json!(7), json!("x"), json!([1]), json!({"release": "1"}), json!({"release": 1e300}), json!({"local": {}})] {
        let _ = s.run("plugin:eng-panel/q", &json!({"panel": junk.clone()}));
        let _ = s.run("plugin.panel", &json!({"id": "eng-panel", "values": junk}));
        let st = &s.plugin_panels["eng-panel"];
        assert!((0..=12).contains(&st.slider("release")));
    }
    // Plugins without a panel get no `panel`, and keep a `panel` parameter of their own.
    plugin("eng-nopanel", &[]);
    assert!(s.run("plugin.panel", &json!({"id": "eng-nopanel"})).unwrap_err().contains("no panel"));
    assert!(s.plugin_panel("eng-nopanel").is_none());
    assert_eq!(forward(&mut s, "eng-nopanel", "host.info", json!({})).unwrap()["plugin"], "eng-nopanel");
    assert!(s.run("plugin.panel", &json!({"id": "nope"})).is_err());
    assert!(s.run("plugin.panel", &json!({})).is_err());
    // A plugin can't drive panels.
    let e = forward(&mut s, "eng-nopanel", "plugin.panel", json!({"id": "eng-panel"})).unwrap_err();
    assert!(e.contains("can't run"), "{e}");
    // A disabled plugin's panel is gone.
    rustgis_plugins::set_enabled("eng-panel", false).unwrap();
    assert!(s.plugin_panel("eng-panel").is_none());
    assert!(s.run("plugin.panel", &json!({"id": "eng-panel"})).unwrap_err().contains("disabled"));
    rustgis_plugins::unload("eng-panel");
    rustgis_plugins::unload("eng-nopanel");
}

/// Loads the panel test plugin with `edit`, its panel's state command being `state`.
fn panel_plugin(id: &str, state: &str) {
    let mut m: Value = serde_json::from_str(&rustgis_plugins::testing::panel_manifest(id)).unwrap();
    m["permissions"] = json!(["edit"]);
    m["panel"]["state"] = json!(state);
    let zip =
        rustgis_plugins::testing::zip_of(&[("plugin.json", m.to_string().as_bytes()), ("plugin.wasm", &rustgis_plugins::testing::wasm())]);
    rustgis_plugins::load(rustgis_plugins::Package::from_zip(&zip).unwrap(), "test").unwrap();
}

#[test]
fn panel_state_commands_that_change_nothing_are_no_undo_step() {
    let mut s = world();
    panel_plugin("eng-noop", "h");
    s.run("plugin.panel", &json!({"id": "eng-noop", "refresh": true})).unwrap();
    s.run("plugin:eng-noop/h", &Value::Null).unwrap();
    assert!(!s.can_undo(), "its state command changed nothing");
    // Its other commands keep their undo step, as plugins without a panel do.
    s.run("plugin:eng-noop/q", &Value::Null).unwrap();
    assert_eq!(s.undo_label(), Some("Echo"));
    rustgis_plugins::unload("eng-noop");
    plugin("eng-nopanel-noop", &["edit"]);
    s.undo.clear();
    s.run("plugin:eng-nopanel-noop/h", &Value::Null).unwrap();
    assert_eq!(s.undo_label(), Some("Hello"));
    rustgis_plugins::unload("eng-nopanel-noop");
}

#[test]
fn a_failed_panel_refresh_leaves_the_values() {
    let mut s = world();
    panel_plugin("eng-badstate", "e");
    let e = s.run("plugin.panel", &json!({"id": "eng-badstate", "values": {"local": true}, "refresh": true}));
    assert!(e.is_err());
    assert!(!s.plugin_panels["eng-badstate"].checked("local"), "the values it set are undone");
    rustgis_plugins::unload("eng-badstate");
}

#[test]
fn plugin_tile_layers_need_network() {
    let mut s = world();
    let osm = json!({"url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "attribution": "© OpenStreetMap contributors"});
    // Without `network`, a plugin can't add a tile layer, directly or by importing a map file.
    plugin("eng-tiles-edit", &["edit", "files"]);
    let e = forward(&mut s, "eng-tiles-edit", "map.addTiles", osm.clone()).unwrap_err();
    assert!(e.contains("'network' permission"), "{e}");
    let mut m = rustgis_map::Map::new("Imported");
    m.tiles.push(rustgis_map::TileLayer::new("Leak", rustgis_map::TileKind::Raster, "https://evil.example.com/{z}/{x}/{y}.png?d=secret"));
    let text = serde_json::to_string(&m).unwrap();
    let e = forward(&mut s, "eng-tiles-edit", "map.import", json!({"text": text})).unwrap_err();
    assert!(e.contains("'network' permission"), "{e}");
    assert!(s.project.maps.iter().all(|m| m.tiles.is_empty()));
    // Nor by changing how a layer it can see fetches (here, an imported map with the same
    // address as an existing layer but another subdomain list).
    s.run("map.addTiles", &json!({"url": "https://{s}.tiles.example.com/{z}/{x}/{y}.png"})).unwrap();
    let mut m = s.map().clone();
    m.name = "Copy".into();
    m.tiles[0].subdomains = vec!["evil".into()];
    let e = forward(&mut s, "eng-tiles-edit", "map.import", json!({"text": serde_json::to_string(&m).unwrap()})).unwrap_err();
    assert!(e.contains("'network' permission"), "{e}");
    assert!(s.project.maps.iter().flat_map(|m| &m.tiles).all(|t| t.subdomains.is_empty()));
    // Changing a layer's opacity is only an edit.
    forward(&mut s, "eng-tiles-edit", "map.setTiles", json!({"index": 0, "opacity": 0.5})).unwrap();
    assert_eq!(s.map().tiles[0].opacity, 0.5);
    s.run("map.removeTiles", &json!({})).unwrap();
    rustgis_plugins::unload("eng-tiles-edit");
    // With it (and `edit`), it can; it needs no `files` for an address.
    plugin("eng-tiles-net", &["edit", "network"]);
    forward(&mut s, "eng-tiles-net", "map.addTiles", osm).unwrap();
    assert_eq!(s.map().tiles.len(), 1);
    let listed = forward(&mut s, "eng-tiles-net", "map.tiles", json!({})).unwrap();
    assert_eq!(listed[0]["attribution"], "© OpenStreetMap contributors");
    forward(&mut s, "eng-tiles-net", "map.removeTiles", json!({})).unwrap();
    assert!(s.map().tiles.is_empty());
    rustgis_plugins::unload("eng-tiles-net");
}

/// Serves documents for `host.fetch` on a local port, counting the requests: `/json`, `/text`,
/// `/latin1` (not UTF-8), `/big` (over the size cap), `/inward` (a redirect to a forbidden
/// address) and `/missing` (404). Returns the base address and the count.
fn serve_documents() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let read = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, extra, body): (&str, String, Vec<u8>) = match path.as_str() {
                p if p.starts_with("/json") => {
                    ("200 OK", String::new(), br#"{"data":[1],"select":[49059],"size":[17053],"valid":true}"#.to_vec())
                }
                "/text" => ("200 OK", String::new(), b"plain text".to_vec()),
                "/latin1" => ("200 OK", String::new(), b"caf\xe9".to_vec()),
                "/big" => ("200 OK", String::new(), vec![b'x'; crate::cmd::plugins::fetch::MAX_BYTES + 10]),
                "/inward" => ("302 Found", "Location: http://169.254.169.254/latest/meta-data/\r\n".into(), Vec::new()),
                _ => ("404 Not Found", String::new(), b"no".to_vec()),
            };
            let _ = write!(s, "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
    });
    (format!("http://{addr}"), count)
}

/// Lets this thread's plugin calls fetch from this computer (the test server) and, when `wait`
/// is off, fetch as the browser does.
fn fetch_mode(local: bool, wait: bool) {
    crate::cmd::plugins::fetch::TEST.with(|t| t.set((local, wait)));
}

/// `host.fetch` needs `network`, takes only public http(s) addresses (redirects too), caps the
/// size, wants UTF-8, answers JSON parsed, and keeps answers.
#[test]
fn plugins_fetch_documents_with_network() {
    let mut s = world();
    plugin("eng-fetch-none", &["edit"]);
    let e = forward(&mut s, "eng-fetch-none", "host.fetch", json!({"url": "https://example.com/x.json"})).unwrap_err();
    assert!(e.contains("'network' permission"), "{e}");
    rustgis_plugins::unload("eng-fetch-none");

    plugin("eng-fetch", &["network"]);
    // Never this computer or the local network, by default; never another scheme.
    for bad in [
        json!({}),
        json!({"url": 7}),
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "s3://bucket/key"}),
        json!({"url": "ftp://example.com/"}),
        json!({"url": "http://127.0.0.1:9/x"}),
        json!({"url": "http://localhost/x"}),
        json!({"url": "http://192.168.0.1/"}),
        json!({"url": "http://169.254.169.254/latest/meta-data/"}),
    ] {
        assert!(forward(&mut s, "eng-fetch", "host.fetch", bad.clone()).is_err(), "{bad}");
    }
    // Hostile parameters are errors, never a crash.
    for junk in [Value::Null, json!([1, 2]), json!({"url": null}), json!({"url": {"nested": 1}}), json!({"url": "x".repeat(100_000)})] {
        assert!(forward(&mut s, "eng-fetch", "host.fetch", junk.clone()).is_err(), "{junk}");
    }

    let (base, count) = serve_documents();
    fetch_mode(true, true);
    let a = forward(&mut s, "eng-fetch", "host.fetch", json!({"url": format!("{base}/json?r=1")})).unwrap();
    assert_eq!(a["json"]["select"][0], 49059);
    assert!(a["text"].as_str().unwrap().starts_with('{'));
    let n = count.load(std::sync::atomic::Ordering::SeqCst);
    // Asked again: the cache answers.
    let again = forward(&mut s, "eng-fetch", "host.fetch", json!({"url": format!("{base}/json?r=1#ignored")})).unwrap();
    assert_eq!(again, a);
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), n, "answered from the cache");
    let t = forward(&mut s, "eng-fetch", "host.fetch", json!({"url": format!("{base}/text")})).unwrap();
    assert_eq!(t["text"], "plain text");
    assert!(t.get("json").is_none());
    for (path, why) in [("/latin1", "UTF-8"), ("/big", "larger than"), ("/missing", "404"), ("/inward", "never fetched")] {
        let e = forward(&mut s, "eng-fetch", "host.fetch", json!({"url": format!("{base}{path}")})).unwrap_err();
        assert!(e.contains(why), "{path}: {e}");
    }
    fetch_mode(false, true);
    rustgis_plugins::unload("eng-fetch");
}

/// Where nothing may wait (the browser), `host.fetch` answers pending: the command is undone,
/// quietly, and runs again once the answer is in, when the cache answers.
#[test]
fn plugin_fetches_run_the_command_again_when_they_land() {
    let mut s = world();
    plugin("eng-fetch-web", &["network", "edit"]);
    let (base, _) = serve_documents();
    fetch_mode(true, false);
    let url = format!("{base}/json?web=1");
    let notices = s.notice_mark();
    let undo = s.undo.len();
    let r = forward(&mut s, "eng-fetch-web", "host.fetch", json!({"url": url})).unwrap();
    assert_eq!(r["pending"], true);
    assert_eq!(r["fetching"][0], url.as_str());
    assert_eq!(s.undo.len(), undo, "no undo step for a run that waits");
    assert!(s.notices_since(notices).is_empty(), "{:?}", s.notices_since(notices));
    let t0 = std::time::Instant::now();
    let ran = loop {
        let ran = s.poll_plugin_retries();
        if !ran.is_empty() || t0.elapsed() > std::time::Duration::from_secs(10) {
            break ran;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].0, "plugin:eng-fetch-web/f");
    assert_eq!(ran[0].1.as_ref().unwrap()["json"]["valid"], true);
    assert!(s.poll_plugin_retries().is_empty(), "run once");
    // A failed fetch is given to the run again as its error.
    let r = forward(&mut s, "eng-fetch-web", "host.fetch", json!({"url": format!("{base}/missing")})).unwrap();
    assert_eq!(r["pending"], true);
    let t0 = std::time::Instant::now();
    let ran = loop {
        let ran = s.poll_plugin_retries();
        if !ran.is_empty() || t0.elapsed() > std::time::Duration::from_secs(10) {
            break ran;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(ran[0].1.as_ref().unwrap_err().contains("404"), "{:?}", ran[0].1);
    // A command forever waiting gives up.
    s.retry_tries = crate::cmd::plugins::fetch::MAX_RETRIES;
    let e = forward(&mut s, "eng-fetch-web", "host.fetch", json!({"url": format!("{base}/json?web=2")})).unwrap_err();
    assert!(e.contains("gave up"), "{e}");
    s.retry_tries = 0;
    fetch_mode(false, true);
    rustgis_plugins::unload("eng-fetch-web");
}

#[test]
fn plugin_tools_run_in_the_geoprocessing_pane() {
    let mut s = world();
    plugin("eng-tools", &[]);
    let tools = s.run("gp.tools", &Value::Null).unwrap();
    assert!(tools.as_array().unwrap().iter().any(|t| t["id"] == "plugin:eng-tools/m" && t["toolbox"] == "Plugins › Test eng-tools"));
    s.run("gp.run", &json!({"tool": "plugin:eng-tools/m", "params": {"input": "Cities"}})).unwrap();
    assert_eq!(s.map().layer_by_name("Made").map(|l| l.dataset.features.len()), Some(1));
    let h = s.project.gp_history.last().unwrap();
    assert!(h.ok && h.messages == ["made"], "{h:?}");
    s.run("gp.run", &json!({"tool": "plugin:eng-tools/x", "params": {"input": "Cities"}})).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().selection, vec![0, 2]);
    assert!(s.run("gp.run", &json!({"tool": "plugin:eng-tools/m", "params": {}})).unwrap_err().contains("required"));
    rustgis_plugins::unload("eng-tools");
}

#[test]
fn plugin_tools_run_off_the_session() {
    use crate::cmd::gp::{plugin_complete, plugin_start};
    use rustgis_plugins::tools::Control;
    let mut s = world();
    plugin("eng-bg", &[]);
    // Started: its layers are read now; it runs elsewhere while the project goes on.
    let run = plugin_start(&mut s, "plugin:eng-bg/m", &json!({"input": "Cities"})).unwrap();
    assert_eq!(run.tool, "plugin:eng-bg/m");
    let made = std::thread::spawn(move || {
        let r = run.run(&Control::default());
        (run.id, r)
    });
    s.run("layer.remove", &json!({"layer": "Cities"})).unwrap();
    let (id, r) = made.join().unwrap();
    plugin_complete(&mut s, id, r).unwrap();
    // Applied as gp.run applies a run: its layer, its history, undo.
    s.run("gp.finish", &json!({"id": id})).unwrap();
    assert_eq!(s.map().layer_by_name("Made").map(|l| l.dataset.features.len()), Some(1));
    let h = s.project.gp_history.last().unwrap();
    assert!(h.ok && h.tool == "Make Points" && h.messages == ["made"] && h.params["input"] == "Cities", "{h:?}");
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layer_by_name("Made").is_none());
    // Selections too.
    let mut s = world();
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    let r = run.run(&Control::default());
    plugin_complete(&mut s, run.id, r).unwrap();
    s.run("gp.finish", &json!({"id": run.id})).unwrap();
    assert_eq!(s.map().layer_by_name("Cities").unwrap().selection, vec![0, 2]);
    // A cancelled run fails, whatever the tool returned, and adds nothing.
    let run = plugin_start(&mut s, "plugin:eng-bg/m", &json!({"input": "Cities"})).unwrap();
    let control = Control::default();
    control.cancel();
    let r = run.run(&control);
    plugin_complete(&mut s, run.id, r).unwrap();
    let e = s.run("gp.finish", &json!({"id": run.id})).unwrap_err();
    assert_eq!(e, "Make Points: cancelled");
    assert!(s.map().layer_by_name("Made").is_none());
    // Parameters are checked when it starts; only plugin tools start so; a run completes once.
    assert!(plugin_start(&mut s, "plugin:eng-bg/m", &json!({})).err().unwrap().contains("required"));
    assert!(plugin_start(&mut s, "buffer", &json!({"input": "Cities"})).err().unwrap().contains("no plugin tool"));
    assert!(plugin_complete(&mut s, run.id, Err("late".into())).unwrap_err().contains("dropped"));
    // Edits made while it ran: a selection follows the features it chose, by id...
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    let ids: Vec<u64> = s.map().layer_by_name("Cities").unwrap().dataset.features.iter().map(|f| f.id).collect();
    fn cities(s: &mut Session) -> &mut rustgis_map::Layer {
        s.map_mut().layers.iter_mut().find(|l| l.name == "Cities").unwrap()
    }
    cities(&mut s).dataset.features.remove(0);
    let r = run.run(&Control::default());
    plugin_complete(&mut s, run.id, r).unwrap();
    s.run("gp.finish", &json!({"id": run.id})).unwrap();
    let l = s.map().layer_by_name("Cities").unwrap();
    assert_eq!(l.selection.iter().map(|&i| l.dataset.features[i].id).collect::<Vec<_>>(), vec![ids[2]]);
    // ...and an update, which would discard them, is refused.
    let update = |s: &Session| {
        let dataset = s.map().layer_by_name("Cities").unwrap().dataset.clone();
        Ok(rustgis_analysis::ToolResult { output: rustgis_analysis::Output::Update { layer: "Cities".into(), dataset }, messages: vec![] })
    };
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    let r = update(&s);
    plugin_complete(&mut s, run.id, r).unwrap();
    s.run("gp.finish", &json!({"id": run.id})).unwrap();
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    let r = update(&s);
    cities(&mut s).dataset.features.remove(0);
    plugin_complete(&mut s, run.id, r).unwrap();
    let e = s.run("gp.finish", &json!({"id": run.id})).unwrap_err();
    assert_eq!(e, "Cities changed while Select Two ran; run it again");
    assert_eq!(s.map().layer_by_name("Cities").unwrap().dataset.features.len(), ids.len() - 2);
    // A layer renamed meanwhile is still the one it read; one removed isn't replaced by another
    // of its name.
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    cities(&mut s).name = "Towns".into();
    let r = run.run(&Control::default());
    plugin_complete(&mut s, run.id, r).unwrap();
    s.run("gp.finish", &json!({"id": run.id})).unwrap();
    assert_eq!(s.map().layer_by_name("Towns").unwrap().selection, vec![0, 2]);
    s.map_mut().layers.iter_mut().find(|l| l.name == "Towns").unwrap().name = "Cities".into();
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    s.run("layer.remove", &json!({"layer": "Cities"})).unwrap();
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Countries", "distance": 1}})).unwrap();
    s.map_mut().layers.last_mut().unwrap().name = "Cities".into();
    let r = run.run(&Control::default());
    plugin_complete(&mut s, run.id, r).unwrap();
    assert_eq!(s.run("gp.finish", &json!({"id": run.id})).unwrap_err(), "Cities was removed while Select Two ran");
    // A run that won't complete can be forgotten.
    let run = plugin_start(&mut s, "plugin:eng-bg/x", &json!({"input": "Cities"})).unwrap();
    crate::cmd::gp::plugin_abandon(&mut s, run.id);
    assert!(!crate::cmd::gp::plugin_waiting(&s, run.id));
    // A run of a project closed while it ran doesn't land in the next one.
    let run = plugin_start(&mut s, "plugin:eng-bg/m", &json!({"input": "Cities"})).unwrap();
    assert!(crate::cmd::gp::plugin_waiting(&s, run.id));
    s.run("project.new", &Value::Null).unwrap();
    assert!(!crate::cmd::gp::plugin_waiting(&s, run.id));
    let r = run.run(&Control::default());
    assert!(plugin_complete(&mut s, run.id, r).unwrap_err().contains("dropped"));
    assert!(s.run("gp.finish", &json!({"id": run.id})).is_err() && s.map().layer_by_name("Made").is_none());
    rustgis_plugins::unload("eng-bg");
}

#[test]
fn plugin_file_parameters_reach_files_and_stay_out_of_the_history() {
    use crate::cmd::plugins::{Reach, reaches};
    let mut s = world();
    plugin("eng-file", &["files"]);
    let run = json!({"tool": "plugin:eng-file/r", "params": {"model": "/data/pism.nc"}});
    // Running a tool that reads a file reaches files (the assistant asks first)...
    let gp = s.registry.get("gp.run").copied().unwrap();
    assert!(matches!(reaches(&gp, &run), Some(Reach::Files)));
    assert!(reaches(&gp, &json!({"tool": "plugin:eng-file/m", "params": {"input": "Cities"}})).is_none());
    // ...so a plugin without `files` can't run it.
    plugin("eng-file-edit", &["edit"]);
    let e = forward(&mut s, "eng-file-edit", "gp.run", run).unwrap_err();
    assert!(e.contains("'files' permission"), "{e}");
    // A file picked in the browser travels with its contents, but the history keeps its name.
    let picked = json!({"tool": "plugin:eng-file/r", "params": {"model": {"name": "pism.nc", "data": "Q0RGAQ=="}}});
    s.run("gp.run", &picked).unwrap();
    let h = s.project.gp_history.last().unwrap();
    assert!(h.ok && h.params["model"] == "pism.nc", "{h:?}");
    rustgis_plugins::unload("eng-file");
    rustgis_plugins::unload("eng-file-edit");
}

/// This test binary, started as the process test plugin, answers one request and exits.
#[test]
fn plugin_process_child() {
    rustgis_plugins::testing::serve_if_child();
}

#[test]
fn process_plugins_reach_the_project_like_modules() {
    let mut s = world();
    let dir = std::env::temp_dir().join(format!("rustgis-test-eng-process-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = rustgis_plugins::testing::child_run("tests::plugin_process_child");
    let manifest = |perms: &[&str]| rustgis_plugins::testing::process_manifest("eng-proc", perms, &run);
    std::fs::write(dir.join("plugin.json"), manifest(&["edit"])).unwrap();
    rustgis_plugins::load(rustgis_plugins::Package::from_dir(&dir).unwrap(), "test").unwrap();
    assert_eq!(s.run("plugin:eng-proc/h", &Value::Null).unwrap(), json!({"hello": "world"}));
    // Its host requests go through the same checks, and its edits are one undo step.
    let fc = json!({"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [3, 4]}}]});
    forward(&mut s, "eng-proc", "host.addLayer", json!({"name": "From Process", "geojson": fc})).unwrap();
    assert!(s.map().layer_by_name("From Process").is_some());
    assert!(s.undo());
    assert!(s.map().layer_by_name("From Process").is_none());
    let e = forward(&mut s, "eng-proc", "layer.addData", json!({"path": "/etc/passwd"})).unwrap_err();
    assert!(e.contains("'files' permission"), "{e}");
    // Its tools run in the Geoprocessing pane.
    s.run("gp.run", &json!({"tool": "plugin:eng-proc/m", "params": {"input": "Cities"}})).unwrap();
    assert_eq!(s.map().layer_by_name("Made").map(|l| l.dataset.features.len()), Some(1));
    rustgis_plugins::unload("eng-proc");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plugins_install_and_uninstall_from_files() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    let dir = std::env::temp_dir().join(format!("rustgis-test-pkg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("eng-install.rgplugin");
    std::fs::write(&file, rustgis_plugins::testing::package("eng-install", &[])).unwrap();
    let info = s.run("plugin.install", &json!({"path": file.display().to_string()})).unwrap();
    assert_eq!(info["id"], "eng-install");
    let list = s.run("plugin.list", &Value::Null).unwrap();
    assert!(list["plugins"].as_array().unwrap().iter().any(|p| p["id"] == "eng-install"));
    let installed = rustgis_plugins::store::dir().unwrap().join("eng-install.rgplugin");
    assert!(installed.is_file(), "copied into the plugins folder");
    // Loaded again at startup, remembering that it was disabled.
    s.run("plugin.enable", &json!({"id": "eng-install", "enabled": false})).unwrap();
    rustgis_plugins::unload("eng-install");
    assert!(crate::cmd::plugins::load_installed().iter().all(|e| !e.contains("eng-install")));
    assert_eq!(rustgis_plugins::get("eng-install").map(|p| p.enabled), Some(false));
    s.run("plugin.uninstall", &json!({"id": "eng-install"})).unwrap();
    assert!(!installed.exists());
    assert!(rustgis_plugins::get("eng-install").is_none());
    // Uninstalling a development folder that shadows an installed package keeps the package.
    s.run("plugin.install", &json!({"path": file.display().to_string()})).unwrap();
    let folder = dir.join("eng-install-dev");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("plugin.json"), rustgis_plugins::testing::manifest("eng-install", &[])).unwrap();
    std::fs::write(folder.join("plugin.wasm"), rustgis_plugins::testing::wasm()).unwrap();
    s.run("plugin.install", &json!({"path": folder.display().to_string()})).unwrap();
    s.run("plugin.uninstall", &json!({"id": "eng-install"})).unwrap();
    assert!(installed.is_file(), "the installed package stays");
    assert!(crate::cmd::plugins::load_installed().iter().all(|e| !e.contains("eng-install")));
    assert_eq!(rustgis_plugins::get("eng-install").map(|p| p.source.clone()), Some(installed.display().to_string()));
    s.run("plugin.uninstall", &json!({"id": "eng-install"})).unwrap();
    assert!(!installed.exists());
    // A file that isn't a package isn't installed.
    std::fs::write(dir.join("bad.rgplugin"), b"nope").unwrap();
    assert!(s.run("plugin.install", &json!({"path": dir.join("bad.rgplugin").display().to_string()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registry_downloads_are_checked() {
    let bytes = rustgis_plugins::testing::package("eng-reg", &[]);
    let mut e = rustgis_plugins::registry::Entry {
        id: "eng-reg".into(),
        name: "Reg".into(),
        version: "1.0.0".into(),
        description: String::new(),
        author: String::new(),
        homepage: String::new(),
        license: String::new(),
        categories: Vec::new(),
        package: "https://example.com/x.rgplugin".into(),
        sha256: "0".repeat(64),
        size: 0,
        min_rustgis_version: String::new(),
        permissions: Vec::new(),
        i18n: Default::default(),
        runtime: rustgis_plugins::manifest::Runtime::Wasm,
        python: None,
    };
    isolate_plugins();
    assert!(crate::cmd::plugins::install_checked(&e, &bytes).unwrap_err().contains("doesn't match"));
    e.sha256 = rustgis_plugins::package::sha256_hex(&bytes);
    e.id = "other".into();
    assert!(crate::cmd::plugins::install_checked(&e, &bytes).unwrap_err().contains("not 'other'"));
    e.id = "eng-reg".into();
    // What the registry said about how it runs (shown before installing) must be so.
    e.runtime = rustgis_plugins::manifest::Runtime::Process;
    let err = crate::cmd::plugins::install_checked(&e, &bytes).unwrap_err();
    assert!(err.contains("is a WebAssembly plugin, but the registry lists it as a process plugin"), "{err}");
    assert_eq!(crate::cmd::plugins::entry_status(&e)["runtime"], "process");
    e.runtime = rustgis_plugins::manifest::Runtime::Wasm;
    crate::cmd::plugins::install_checked(&e, &bytes).unwrap();
    assert_eq!(crate::cmd::plugins::entry_status(&e)["installed"], "1.0.0");
    assert_eq!(crate::cmd::plugins::entry_status(&e)["runtime"], "wasm");
    let _ = rustgis_plugins::store::uninstall("eng-reg");
}
#[test]
fn reports_group_summarise_relate_export_and_reopen() {
    let dir = std::env::temp_dir().join(format!("rustgis-report-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = world();
    assert!(s.run("report.autofill", &Value::Null).is_err(), "no report yet");
    let cities = s.map().layer_by_name("Cities").unwrap();
    let fields: Vec<String> = cities.dataset.fields.iter().map(|f| f.name.clone()).collect();
    let numeric = cities.dataset.fields.iter().find(|f| f.name.to_ascii_lowercase().starts_with("pop")).unwrap().name.clone();
    let n = cities.dataset.features.len();
    s.run(
        "report.new",
        &json!({"layer": "Cities", "name": "Cities", "fields": ["name", numeric], "groups": ["adm0name"], "sort": numeric, "descending": true, "page": "a4"}),
    )
    .unwrap();
    assert_eq!((s.project.reports[0].width, s.project.reports[0].height), (210.0, 297.0));
    assert!(s.run("report.field", &json!({"field": "no_such_field"})).is_err());
    s.run("report.statistics", &json!({"field": numeric, "kind": "sum"})).unwrap();
    s.run("report.chart", &json!({"category": "adm0name"})).unwrap();
    s.run("report.supplementalPage", &json!({"title": "About", "text": "Cities of the world."})).unwrap();
    s.run("report.newTemplate", &json!({"end": true})).unwrap();
    assert!(s.active_layout.is_none(), "adding a layout page leaves the map view showing");
    // The blocks: a heading per country, the sum, every city once.
    let doc = crate::cmd::report::build(&s, 0).unwrap();
    use rustgis_map::report::Block;
    let rows = doc.blocks.iter().filter(|b| matches!(b, Block::Row(_))).count();
    assert_eq!(rows, n);
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Heading { level: 0, .. })));
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Stats(v) if v.iter().any(|l| l.starts_with("Sum of")))));
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Chart { bars, .. } if !bars.is_empty())));
    assert!(matches!(doc.blocks.last(), Some(Block::Layout(_))));
    // A report on countries with each country's cities beneath it, followed by the cities report.
    s.run("data.relate", &json!({"name": "Country of city", "origin": "Countries", "originKey": "name", "destination": "Cities", "destinationKey": "adm0name"}))
        .unwrap();
    s.run("report.new", &json!({"layer": "Countries", "name": "Countries", "fields": ["name"], "where": "name LIKE 'F%'"})).unwrap();
    s.run("report.subreport", &json!({"relationship": "Country of city", "fields": ["name"]})).unwrap();
    s.run("report.relatedReport", &json!({"related": "Cities"})).unwrap();
    assert!(s.run("report.relatedReport", &json!({"related": "Countries"})).is_err(), "not itself");
    let doc = crate::cmd::report::build(&s, 1).unwrap();
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::SubRow(_))), "France's cities are listed under it");
    assert!(doc.blocks.iter().filter(|b| matches!(b, Block::Row(_))).count() > n, "the cities report follows");
    // Moving a report to the other end of its subreport's relationship drops the related fields
    // the new related layer lacks.
    s.run("report.new", &json!({"layer": "Countries", "name": "Swap"})).unwrap();
    s.run("report.subreport", &json!({"relationship": "Country of city", "fields": ["name", "adm0name"]})).unwrap();
    s.run("report.update", &json!({"layer": "Cities"})).unwrap();
    let swap = s.project.reports.iter().find(|r| r.name == "Swap").unwrap();
    assert!(swap.sections.iter().any(|x| matches!(x, rustgis_map::report::Section::Subreport { fields, .. } if fields == &["name"])));
    s.run("report.remove", &Value::Null).unwrap();
    // Picture paths are read relative to the saved project's folder only.
    let png = {
        let mut pm = tiny_skia::Pixmap::new(4, 4).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(200, 0, 0, 255));
        pm.encode_png().unwrap()
    };
    std::fs::write(dir.join("pic.png"), &png).unwrap();
    let pics = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"photo": "pic.png"}, "geometry": {"type": "Point", "coordinates": [0, 0]}},
        {"type": "Feature", "properties": {"photo": dir.join("pic.png").to_str().unwrap()}, "geometry": {"type": "Point", "coordinates": [1, 0]}},
        {"type": "Feature", "properties": {"photo": "../pic.png"}, "geometry": {"type": "Point", "coordinates": [2, 0]}}]});
    s.run("layer.addDataText", &json!({"fileName": "pics.geojson", "text": pics.to_string()})).unwrap();
    s.run("report.new", &json!({"layer": "pics", "name": "Pics"})).unwrap();
    s.run("report.dynamicPicture", &json!({"field": "photo"})).unwrap();
    let pi = s.project.reports.iter().position(|r| r.name == "Pics").unwrap();
    let count =
        |s: &Session| crate::cmd::report::build(s, pi).unwrap().blocks.iter().filter(|b| matches!(b, Block::Picture { .. })).count();
    assert_eq!(count(&s), 0, "an unsaved project reads no picture files");
    s.run("project.saveAs", &json!({"path": dir.join("p.rgis").to_str().unwrap()})).unwrap();
    assert_eq!(count(&s), 1, "only the relative path inside the project folder");
    // Renaming a report keeps links to it; exports are .pdf or .png only.
    s.run("report.update", &json!({"report": "Cities", "name": "World cities"})).unwrap();
    let countries = s.project.reports.iter().find(|r| r.name == "Countries").unwrap();
    assert!(countries.sections.iter().any(|x| matches!(x, rustgis_map::report::Section::Related { report } if report == "World cities")));
    assert!(s.run("report.export", &json!({"report": "Countries", "path": dir.join("c.jpg").to_str().unwrap()})).is_err());
    s.run("report.update", &json!({"report": "World cities", "name": "Cities"})).unwrap();
    // Sorting a column that mixes numbers and text (9, 10, "1a"…) is a total order, never a panic.
    let mixed = json!({"type": "FeatureCollection", "features": (0..200).map(|i| json!({"type": "Feature",
        "properties": {"v": if i % 3 == 0 { json!(format!("{}a", i % 7)) } else { json!(i % 11) }},
        "geometry": {"type": "Point", "coordinates": [i as f64 * 0.01, 0.0]}})).collect::<Vec<_>>()});
    s.run("layer.addDataText", &json!({"fileName": "mixed.geojson", "text": mixed.to_string()})).unwrap();
    s.run("report.new", &json!({"layer": "mixed", "name": "Mixed", "sort": "v", "groups": ["v"]})).unwrap();
    let i = s.project.reports.iter().position(|r| r.name == "Mixed").unwrap();
    assert!(crate::cmd::report::build(&s, i).is_ok());
    // A related report that loops back stops.
    s.run("report.relatedReport", &json!({"report": "Cities", "related": "Countries"})).unwrap();
    assert!(crate::cmd::report::build(&s, 1).is_ok());
    // Export: PDF pages and a PNG.
    let pdf = dir.join("countries.pdf");
    let r = s.run("report.export", &json!({"report": "Countries", "path": pdf.to_str().unwrap(), "dpi": 72})).unwrap();
    assert!(r["pages"].as_u64().unwrap() >= 3);
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    let png = dir.join("cities.png");
    s.run("report.export", &json!({"report": "Cities", "path": png.to_str().unwrap(), "dpi": 72})).unwrap();
    assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
    // Report file and template, reopened.
    let file = dir.join("cities.rgisreport");
    let tmpl = dir.join("cities.rgisrpttemplate");
    s.run("report.save", &json!({"report": "Cities", "path": file.to_str().unwrap()})).unwrap();
    s.run("report.saveTemplate", &json!({"report": "Cities", "path": tmpl.to_str().unwrap()})).unwrap();
    let r = s.run("report.open", &json!({"path": file.to_str().unwrap()})).unwrap();
    assert_eq!(r["name"], "Cities 2");
    s.run("report.open", &json!({"path": tmpl.to_str().unwrap(), "layer": "Cities"})).unwrap();
    let t = s.project.reports.last().unwrap();
    assert_eq!(t.layer, "Cities");
    assert!(t.sections.iter().all(|x| matches!(x, rustgis_map::report::Section::Page { .. })));
    // Items, undo and fields kept.
    let (name, bytes) = crate::cmd::items::item_bytes(&mut s, "reportTemplate", &json!({"report": "Cities"})).unwrap();
    assert!(name.ends_with(".rgisrpttemplate") && !bytes.is_empty());
    assert!(fields.iter().any(|f| f == "adm0name"));
    let before = s.project.reports.len();
    s.run("report.remove", &json!({"report": "Cities 2"})).unwrap();
    assert_eq!(s.project.reports.len(), before - 1);
    s.undo();
    assert_eq!(s.project.reports.len(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn presentations_and_notebooks() {
    let dir = std::env::temp_dir().join(format!("rustgis-pres-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = world();
    assert!(s.run("presentation.mapPage", &Value::Null).is_err(), "no presentation yet");
    s.run("presentation.new", &json!({"name": "Tour", "title": "World tour"})).unwrap();
    s.run("map.goToXY", &json!({"lon": 12.5, "lat": 41.9, "scale": 5_000_000})).unwrap();
    s.run("presentation.mapPage", &json!({"title": "Rome"})).unwrap();
    s.run("presentation.blankPage", &json!({"title": "Facts", "text": "Rome is the capital of Italy.", "background": "#FFF8E0"})).unwrap();
    assert!(s.run("presentation.blankPage", &json!({"background": "beige"})).is_err());
    let png = s.run("project.thumbnail", &json!({"create": true})).map(|_| s.project.thumbnail.clone().unwrap()).unwrap();
    s.run("presentation.imagePage", &json!({"image": png, "caption": "The map", "at": 1})).unwrap();
    assert!(s.run("presentation.imagePage", &json!({"image": "bm90"})).is_err());
    s.run("presentation.videoPage", &json!({"source": "https://example.org/rome.mp4", "posterImage": png})).unwrap();
    s.run("presentation.videoPage", &json!({"source": "C:\\videos\\walk.webm"})).unwrap();
    let pres = &s.project.presentations[0];
    let kinds: Vec<&str> = pres.pages.iter().map(|p| p.kind.code()).collect();
    assert_eq!(kinds, ["blank", "image", "map", "blank", "video", "video"]);
    assert_eq!(pres.pages[5].title, "walk.webm");
    s.run("presentation.updatePage", &json!({"page": 2, "notes": "Zoom in", "seconds": 9, "moveTo": 0})).unwrap();
    assert_eq!(s.project.presentations[0].pages[0].title, "Rome");
    assert!(s.run("presentation.updatePage", &json!({"page": 99})).is_err());
    // The map page draws the map: not one flat colour.
    let img = crate::cmd::presentation::draw_page(&mut s, 0, 0, 40.0).unwrap();
    let first = img.pixels()[img.pixels().len() / 2];
    assert!(img.pixels().iter().any(|p| *p != first));
    // Exports.
    for (file, magic) in [("tour.pdf", &b"%PDF"[..]), ("tour.html", &b"<!doctype html>"[..]), ("tour.png", &b"\x89PNG"[..])] {
        let path = dir.join(file);
        let r = s.run("presentation.export", &json!({"path": path.to_str().unwrap(), "dpi": 48})).unwrap();
        assert_eq!(r["pages"], if file.ends_with(".png") { 1 } else { 6 });
        assert!(std::fs::read(&path).unwrap().starts_with(magic), "{file}");
    }
    assert!(s.run("presentation.export", &json!({"path": dir.join("deck.pptx").to_str().unwrap()})).is_err());
    let html = std::fs::read_to_string(dir.join("tour.html")).unwrap();
    use crate::cmd::presentation::playable;
    for (source, kept) in [
        ("https://a.org/v.mp4", true),
        ("clip.mp4", true),
        ("C:\\v.mp4", true),
        ("/v/a.mp4", true),
        ("file:///etc/v", false),
        ("data:x", false),
    ] {
        assert_eq!(playable(source).is_empty(), !kept, "{source}");
    }
    assert!(html.contains("<video") && html.contains("https://example.org/rome.mp4") && html.matches("<section").count() == 6);
    // Save and import.
    let file = dir.join("tour.rgispres");
    s.run("presentation.save", &json!({"path": file.to_str().unwrap()})).unwrap();
    let r = s.run("presentation.import", &json!({"path": file.to_str().unwrap()})).unwrap();
    assert_eq!(r["name"], "Tour 2");
    assert_eq!(s.project.presentations[1].pages.len(), 6);
    s.run("presentation.removePage", &json!({"page": 0})).unwrap();
    assert_eq!(s.project.presentations[1].pages.len(), 5);

    // Notebooks: command cells run and keep their output; Jupyter round trip.
    s.run("notebook.new", &json!({"name": "Analysis"})).unwrap();
    s.run("notebook.addCell", &json!({"source": "map.goToXY {\"lon\": 2.35, \"lat\": 48.85, \"scale\": 1000000}\nselect.byAttributes {\"layer\": \"Cities\", \"where\": \"name = 'Paris'\"}"}))
        .unwrap();
    s.run("notebook.addCell", &json!({"source": "no.such.command"})).unwrap();
    let r = s.run("notebook.run", &Value::Null).unwrap();
    assert_eq!(r["error"], 3);
    let nb = &s.project.notebooks[0];
    assert!(nb.cells[2].output.as_deref().is_some_and(|o| o.contains("\"selected\":1")), "{:?}", nb.cells[2].output);
    assert!(nb.cells[3].error);
    assert!((s.map().center_lonlat().x - 2.35).abs() < 1e-6);
    // A cell can't run notebooks (no recursion).
    s.run("notebook.editCell", &json!({"cell": 3, "source": "notebook.run {}"})).unwrap();
    s.run("notebook.run", &json!({"cell": 3})).unwrap();
    assert!(s.project.notebooks[0].cells[3].error);
    // Nor through a task that runs the notebook (no unbounded recursion).
    s.run(
        "task.new",
        &json!({"name": "Loop", "steps": [{"title": "again", "command": "notebook.run", "params": {"notebook": "Analysis"}}]}),
    )
    .unwrap();
    s.run("notebook.editCell", &json!({"cell": 3, "source": "task.run {\"task\": \"Loop\"}"})).unwrap();
    s.run("notebook.run", &json!({"cell": 3})).unwrap();
    let cell = &s.project.notebooks[0].cells[3];
    assert!(cell.error && cell.output.as_deref().is_some_and(|o| o.contains("already running")), "{:?}", cell.output);
    assert!(s.run("notebook.run", &json!({"cell": 2})).is_ok(), "the guard is cleared after a run");
    s.run("task.remove", &json!({"task": "Loop"})).unwrap();
    let ipynb = dir.join("analysis.ipynb");
    s.run("notebook.save", &json!({"path": ipynb.to_str().unwrap()})).unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(&ipynb).unwrap()).unwrap();
    assert_eq!(v["nbformat"], 4);
    assert_eq!(v["cells"].as_array().unwrap().len(), 4);
    s.run("notebook.open", &json!({"path": ipynb.to_str().unwrap()})).unwrap();
    let nb = s.project.notebooks.last().unwrap();
    assert_eq!(nb.name, "Analysis 2");
    assert_eq!(nb.cells.len(), 4);
    assert!(nb.cells[2].output.is_some());
    assert!(nb.cells[3].error, "a failed cell stays failed through Jupyter");
    // A notebook from a file can't touch files; one made here can.
    let png = dir.join("nb.png");
    let cell = format!("share.exportMap {{\"path\": {:?}}}", png.to_str().unwrap());
    s.run("notebook.addCell", &json!({"notebook": "Analysis 2", "source": cell})).unwrap();
    let r = s.run("notebook.run", &json!({"notebook": "Analysis 2", "cell": 4})).unwrap();
    assert_eq!(r["error"], 4);
    assert!(!png.exists());
    s.run("notebook.addCell", &json!({"notebook": "Analysis", "source": cell})).unwrap();
    let last = s.project.notebooks[0].cells.len() - 1;
    s.run("notebook.run", &json!({"notebook": "Analysis", "cell": last})).unwrap();
    assert!(png.exists());
    assert!(s.run("notebook.open", &json!({"text": "{\"cells\": 5}"})).is_err());
    // Trust left by an undone notebook doesn't pass to a file's notebook of the same name.
    s.run("notebook.new", &json!({"name": "Notebook"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    let text = json!({"nbformat": 4, "cells": [{"cell_type": "code", "source": cell}]}).to_string();
    s.run("notebook.open", &json!({"text": text})).unwrap();
    let r = s.run("notebook.run", &json!({"notebook": "Notebook"})).unwrap();
    assert_eq!(r["error"], 0, "{r}");
    // Nor can it make a trusted notebook to run its lines later.
    let text = json!({"nbformat": 4, "cells": [{"cell_type": "code", "source": "notebook.new {\"name\": \"Laundered\"}"}]}).to_string();
    s.run("notebook.open", &json!({"text": text})).unwrap();
    let opened = s.project.notebooks.last().unwrap().name.clone();
    let r = s.run("notebook.run", &json!({"notebook": opened})).unwrap();
    assert_eq!(r["error"], 0, "{r}");
    assert!(!s.project.notebooks.iter().any(|nb| nb.name == "Laundered"));
    // Nor borrow trust from a notebook whose name differs only by non-ASCII case.
    let text = json!({"nbformat": 4, "metadata": {"title": "éa"}, "cells": [{"cell_type": "code", "source": cell}]}).to_string();
    s.run("notebook.open", &json!({"text": text})).unwrap();
    s.run("notebook.new", &json!({"name": "Éa"})).unwrap();
    let r = s.run("notebook.run", &json!({"notebook": "éa"})).unwrap();
    assert_eq!(r["error"], 0, "{r}");
    for name in ["Éa", "éa"] {
        let at = s.project.notebooks.iter().position(|nb| nb.name == name).unwrap();
        s.run("notebook.remove", &json!({"notebook": at})).unwrap();
    }
    // Or plant a cell in a trusted notebook.
    let plant = format!("notebook.addCell {{\"notebook\": \"Analysis\", \"source\": {:?}}}", "share.exportMap {}");
    s.run("notebook.editCell", &json!({"notebook": opened.as_str(), "cell": 0, "source": plant})).unwrap();
    let before = s.project.notebooks[0].cells.len();
    let r = s.run("notebook.run", &json!({"notebook": opened.as_str()})).unwrap();
    assert_eq!(r["error"], 0, "{r}");
    assert_eq!(s.project.notebooks[0].cells.len(), before);
    let last = s.project.notebooks.len() - 1;
    s.run("notebook.remove", &json!({"notebook": last})).unwrap();
    // A cell that removes an earlier notebook still writes its output to its own notebook.
    s.run("notebook.new", &json!({"name": "Runner"})).unwrap();
    s.run("notebook.addCell", &json!({"notebook": "Runner", "source": "notebook.remove {\"notebook\": 0}"})).unwrap();
    s.run("notebook.addCell", &json!({"notebook": "Runner", "source": "map.goToXY {\"lon\": 0, \"lat\": 0}"})).unwrap();
    let r = s.run("notebook.run", &json!({"notebook": "Runner"})).unwrap();
    assert!(r.get("error").is_none(), "{r}");
    let nb = s.project.notebooks.iter().find(|nb| nb.name == "Runner").unwrap();
    assert!(nb.cells[2].output.as_deref().is_some_and(|o| o.starts_with("notebook.remove")), "{:?}", nb.cells[2].output);
    assert!(nb.cells[3].output.is_some());
    assert_eq!(s.project.notebooks.len(), 3);
    // Notebooks and presentations from a project file get unique names, since they're found
    // (and notebooks trusted) by name.
    let mut v = serde_json::to_value(&s.project).unwrap();
    v["notebooks"][1]["name"] = json!("ANALYSIS 2");
    v["presentations"][1]["name"] = json!("tour");
    let p = Project::from_json(&v.to_string()).unwrap();
    assert_eq!(p.notebooks[1].name, "ANALYSIS 2 2");
    assert_eq!(p.presentations[1].name, "tour 2");
    // Many notebooks of one name: capped, and renamed quickly.
    v["notebooks"] = Value::Array((0..1500).map(|_| json!({"name": "A", "cells": []})).collect());
    let t0 = std::time::Instant::now();
    let p = Project::from_json(&v.to_string()).unwrap();
    assert_eq!(p.notebooks.len(), crate::project::MAX_ITEMS);
    assert_eq!(p.notebooks.last().unwrap().name, format!("A {}", crate::project::MAX_ITEMS));
    assert!(t0.elapsed().as_secs() < 5);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A three-band raster: water-like pixels on the left half, field-like on the right.
fn two_class_b64() -> String {
    use base64::Engine;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let gt = GeoTransform { origin_x: 10.0, origin_y: 45.04, pixel_width: 0.001, pixel_height: -0.001 };
    let band = |water: f32, field: f32| -> Vec<f32> {
        (0..1600).map(|i| if i % 40 < 20 { water + (i % 7) as f32 } else { field + (i % 5) as f32 }).collect()
    };
    let r = Raster::new(
        "scene",
        40,
        40,
        vec![band(20.0, 120.0), band(80.0, 30.0), band(140.0, 60.0)],
        gt,
        Crs::LonLat,
        None,
        ColorHint::Rgb { alpha: false },
        "UInt8",
    )
    .unwrap();
    base64::engine::general_purpose::STANDARD.encode(geotiff::write(&r, 0).unwrap())
}

#[test]
fn deep_learning_hosted_imagery_and_sharing() {
    let dir = std::env::temp_dir().join(format!("rustgis-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = |f: &str| dir.join(f).to_str().unwrap().to_string();
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "scene.tif", "data": two_class_b64()})).unwrap();
    let training = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"class": "water"}, "geometry": {"type": "Polygon", "coordinates": [[[10.002, 45.002], [10.015, 45.002], [10.015, 45.035], [10.002, 45.035], [10.002, 45.002]]]}},
        {"type": "Feature", "properties": {"class": "field"}, "geometry": {"type": "Polygon", "coordinates": [[[10.025, 45.002], [10.038, 45.002], [10.038, 45.035], [10.025, 45.035], [10.025, 45.002]]]}}]});
    s.run("layer.addDataText", &json!({"fileName": "training.geojson", "text": training.to_string()})).unwrap();
    assert!(s.run("dl.package", &json!({"path": path("x.rgisdlpk")})).is_err(), "no model yet");
    // A class whose polygon misses the raster is named in the error.
    let lost = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"class": "water"}, "geometry": {"type": "Polygon", "coordinates": [[[10.002, 45.002], [10.015, 45.002], [10.015, 45.035], [10.002, 45.035], [10.002, 45.002]]]}},
        {"type": "Feature", "properties": {"class": "lost"}, "geometry": {"type": "Polygon", "coordinates": [[[-60.0, -10.0], [-59.0, -10.0], [-59.0, -9.0], [-60.0, -9.0], [-60.0, -10.0]]]}}]});
    s.run("layer.addDataText", &json!({"fileName": "lost.geojson", "text": lost.to_string()})).unwrap();
    let err = s.run("dl.train", &json!({"layer": "scene", "training": "lost", "classField": "class"})).unwrap_err();
    assert!(err.contains("'lost'"), "{err}");
    let r = s
        .run(
            "dl.classify",
            &json!({"layer": "scene", "training": "training", "classField": "class", "bands": [1, 2], "epochs": 30, "model": "landcover"}),
        )
        .unwrap();
    assert!(r["accuracy"].as_f64().unwrap() > 0.95, "{r}");
    let classes = r["classes"].as_array().unwrap();
    assert!(classes.iter().all(|c| c["pixels"].as_u64().unwrap() > 700), "both halves classified: {r}");
    // Package, remove, import, apply again.
    s.run("dl.package", &json!({"path": path("landcover.rgisdlpk")})).unwrap();
    s.run("dl.remove", &json!({"model": "landcover"})).unwrap();
    assert!(s.project.dl_models.is_empty());
    s.run("dl.import", &json!({"path": path("landcover.rgisdlpk")})).unwrap();
    s.run("dl.classify", &json!({"layer": "scene", "model": "landcover", "name": "again"})).unwrap();
    assert!(s.run("dl.import", &json!({"data": "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA=="})).is_err(), "an empty zip");
    // Importing again doesn't replace the model; retraining it by name does, and says so.
    let r = s.run("dl.import", &json!({"path": path("landcover.rgisdlpk")})).unwrap();
    assert_eq!(r["model"], "landcover 2");
    let r = s
        .run(
            "dl.train",
            &json!({"layer": "scene", "training": "training", "classField": "class", "bands": [1, 2], "epochs": 5, "model": "landcover"}),
        )
        .unwrap();
    assert_eq!(r["replaced"], true);
    assert_eq!(s.project.dl_models.len(), 2);
    // Hosted imagery: tiles in a zip, transparent outside the raster.
    let r = s.run("imagery.hostedImagery", &json!({"layer": "scene", "path": path("tiles.zip"), "minZoom": 10, "maxZoom": 12})).unwrap();
    assert!(r["tiles"].as_u64().unwrap() >= 3);
    let entries = rustgis_data::zip::read(&std::fs::read(path("tiles.zip")).unwrap()).unwrap();
    assert!(entries.iter().any(|e| e.0 == "tiles.json") && entries.iter().any(|e| e.0.starts_with("tiles/12/")));

    // Sharing to a data store.
    assert!(s.run("share.webMap", &Value::Null).is_err(), "no path and no data store");
    s.run("share.dataStore", &json!({"name": "out", "path": dir.to_str().unwrap()})).unwrap();
    assert!(s.run("share.dataStore", &json!({"path": path("missing")})).is_err());
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "graduated", "field": "pop_est"})).unwrap();
    s.run("share.webMap", &Value::Null).unwrap();
    let html = std::fs::read_to_string(path("World.html")).unwrap();
    assert!(
        html.contains("<canvas") && html.contains("\"Countries\"") && html.contains("data:image/png;base64,"),
        "vector and raster layers"
    );
    s.run("share.webLayer", &json!({"layer": "Countries"})).unwrap();
    let z = rustgis_data::zip::read(&std::fs::read(path("Countries.zip")).unwrap()).unwrap();
    for f in ["collections.json", "conformance.json", "style.json", "index.html", "collections/countries/items.json"] {
        assert!(z.iter().any(|e| e.0 == f), "{f}");
    }
    s.run("share.webStyle", &json!({"layer": "Countries"})).unwrap();
    let style: Value = serde_json::from_slice(&std::fs::read(path("Countries.json")).unwrap()).unwrap();
    assert_eq!(style["version"], 8);
    assert_eq!(style["layers"][0]["paint"]["fill-color"][0], "step", "graduated colours as a step expression");
    // What a style leaves out of the labels is the last notice, after the one saying it's shared.
    s.run("labels.enable", &json!({"layer": "Cities", "on": true})).unwrap();
    s.run("labels.class.add", &json!({"layer": "Cities", "name": "Big", "where": "pop_max > 5000000"})).unwrap();
    let last_two = |s: &Session| s.notices.iter().rev().take(2).map(|n| n.text.clone()).collect::<Vec<_>>();
    s.run("share.webStyle", &json!({"layer": "Cities"})).unwrap();
    let said = last_two(&s);
    assert!(said[0].contains("labels every feature with its Default class") && said[1].starts_with("Shared Cities"), "{said:?}");
    s.run("labels.class.update", &json!({"layer": "Cities", "visible": false})).unwrap();
    s.run("share.webStyle", &json!({"layer": "Cities"})).unwrap();
    let said = last_two(&s);
    assert!(said[0].contains("has no labels") && said[1].starts_with("Shared Cities"), "{said:?}");
    // A hidden Default alone is said too: the labels are on, and the style has none.
    s.run("labels.class.clear", &json!({"layer": "Cities"})).unwrap();
    s.run("share.webStyle", &json!({"layer": "Cities"})).unwrap();
    let said = last_two(&s);
    assert!(said[0].ends_with("its Default class is hidden.") && said[1].starts_with("Shared Cities"), "{said:?}");
    s.run("share.locator", &json!({"layer": "Cities", "fields": ["name"]})).unwrap();
    let r = s.run("locator.find", &json!({"text": "paris", "zoom": true})).unwrap();
    assert_eq!(r["matches"][0]["text"], "Paris");
    assert!((s.map().center_lonlat().x - 2.35).abs() < 0.1);
    s.run("share.geodataService", &Value::Null).unwrap();
    s.run("model.new", &json!({"name": "Buffer", "steps": [{"tool": "buffer", "params": {"input": "Cities", "distance": 10}}]})).unwrap();
    s.run("share.webTool", &Value::Null).unwrap();
    s.run("model.import", &json!({"path": path("Buffer.rgistool")})).unwrap();
    assert_eq!(s.project.models.len(), 2);
    s.run("layer.extrusion", &json!({"layer": "Countries", "type": "height", "height": 100000})).unwrap();
    s.run("share.webScene", &Value::Null).unwrap();
    let g: Value = serde_json::from_slice(&std::fs::read(path("World.gltf")).unwrap()).unwrap();
    assert_eq!(g["asset"]["version"], "2.0");
    assert!(g["meshes"].as_array().unwrap().len() >= 2);
    assert!(s.run("share.linkChart", &Value::Null).is_err(), "no relationships yet");
    s.run("data.relate", &json!({"name": "Country of city", "origin": "Countries", "originKey": "name", "destination": "Cities", "destinationKey": "adm0name"})).unwrap();
    s.run("share.linkChart", &Value::Null).unwrap();
    assert!(std::fs::read_to_string(path("Relationships.html")).unwrap().contains("<svg"));
    // Manage: update in place; jobs list them.
    let before = s.project.shared.len();
    s.run("share.saveWebMap", &Value::Null).unwrap();
    s.run("share.replaceWebLayer", &json!({"layer": "Lakes"})).unwrap();
    s.run("share.saveWebScene", &Value::Null).unwrap();
    s.run("share.saveWebLinkChart", &Value::Null).unwrap();
    assert_eq!(s.project.shared.len(), before, "updated, not added");
    let z = rustgis_data::zip::read(&std::fs::read(path("Countries.zip")).unwrap()).unwrap();
    assert!(z.iter().any(|e| e.0 == "collections/lakes/items.json"), "the web layer now holds the lakes");
    s.run("share.upload", &json!({"path": path("landcover.rgisdlpk"), "store": "out"})).unwrap_err();
    let up = dir.join("upload_src");
    std::fs::create_dir_all(&up).unwrap();
    std::fs::write(up.join("notes.txt"), "hello").unwrap();
    s.run("share.upload", &json!({"path": up.join("notes.txt").to_str().unwrap()})).unwrap();
    assert!(dir.join("notes.txt").exists());
    let jobs = s.run("share.jobs", &Value::Null).unwrap();
    assert_eq!(jobs["items"].as_array().unwrap().len(), before + 1);
    // A second file of that name isn't written over the first.
    assert!(s.run("share.upload", &json!({"path": up.join("notes.txt").to_str().unwrap()})).is_err());
    // Shares and data stores listed by an opened project aren't written to (their paths come
    // from the file) until they're shared or registered again.
    s.project = Project::from_json(&serde_json::to_string(&s.project).unwrap()).unwrap();
    let err = s.run("share.saveWebMap", &Value::Null).unwrap_err();
    assert!(err.contains("came with the opened project"), "{err}");
    let err = s.run("share.webMap", &Value::Null).unwrap_err();
    assert!(err.contains("came with the opened project"), "{err}");
    s.run("share.dataStore", &json!({"name": "out", "path": dir.to_str().unwrap()})).unwrap();
    s.run("share.webMap", &Value::Null).unwrap();
    s.run("share.dataStore", &json!({"remove": "out"})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A FlatGeobuf of a grid of points over the world (`n` × `n`), numbered.
fn grid_fgb(name: &str, n: usize) -> std::path::PathBuf {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
    let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("i", FieldType::Integer), Field::new("x", FieldType::Double)]);
    for i in 0..n {
        for j in 0..n {
            let c = Coord::new(-179.0 + 358.0 * i as f64 / (n - 1) as f64, -80.0 + 160.0 * j as f64 / (n - 1) as f64);
            ds.push(Some(Geometry::Point(c)), vec![V::Int((i * n + j) as i64), V::Float(c.x)]);
        }
    }
    let p = std::env::temp_dir().join(format!("rustgis-engine-{}-{name}.fgb", std::process::id()));
    rustgis_data::io::write_path(&ds, &p).unwrap();
    p
}

/// A view of a lon/lat box, 800 × 600 points.
fn view_of(s: &mut Session, e: rustgis_geom::Extent) -> crate::render::View {
    s.view_size = (800.0, 600.0);
    s.zoom_to(&e);
    crate::render::View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false }
}

#[test]
fn a_refused_edit_of_a_streamed_layer_says_why() {
    use rustgis_data::{Coord, Dataset, Geometry, GeometryType};
    let path = grid_fgb("refused", 20);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    let streamed = r["layer"].as_u64().unwrap() as u32;
    // Its window read, as drawing does.
    let v = view_of(&mut s, rustgis_geom::Extent::new(-180.0, -80.0, 180.0, 80.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let count = |s: &Session| s.map().layer(streamed).unwrap().dataset.features.len();
    let n = count(&s);
    assert!(n > 2, "{n}");
    let mut d = Dataset::new("other", GeometryType::Point, vec![]);
    d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let other = s.map_mut().add_dataset(d);
    // Refused up front (the streamed layer active), or undone after (another one active): either
    // way nothing changes, and the reason is said.
    for active in [streamed, other] {
        for (id, p) in [("edit.delete", json!(null)), ("edit.cut", json!(null)), ("edit.move", json!({"dx": 1000, "dy": 0}))] {
            s.active_layer = Some(active);
            s.map_mut().layer_mut(streamed).unwrap().selection = vec![0, 1];
            let mark = s.notice_mark();
            let e = s.run(id, &p).unwrap_err();
            assert!(e.contains("read in place"), "{id}: {e}");
            assert_eq!(count(&s), n, "{id}");
            let said = s.notices_since(mark);
            assert!(said.iter().any(|x| x.error && x.text == e), "{id}: no notice");
            // Nothing that says it was done ("Deleted 2 features"): it was undone.
            assert!(said.iter().all(|x| x.error), "{id}: {said:?}");
        }
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn large_sources_stream_a_view_at_a_time() {
    let path = grid_fgb("big", 150);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    assert_eq!(r["streamed"], true, "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    // A small view limit, to see sampling without a huge file.
    s.map_mut().layer_mut(id).unwrap().source.as_mut().unwrap().limit = 5_000;
    let layer = |s: &Session| s.map().layer(id).unwrap().clone();
    assert!(layer(&s).is_streamed());
    // The whole extent, not the part read.
    assert!(layer(&s).extent().width() > 350.0);

    // The world: a spread sample, capped.
    let v = view_of(&mut s, rustgis_geom::Extent::new(-180.0, -80.0, 180.0, 80.0));
    let png = crate::render::render_png(s.map_mut(), &v).unwrap();
    assert!(!png.is_empty());
    let l = layer(&s);
    let n = l.dataset.features.len();
    assert!(n > 2_000 && n <= 5_000, "{n}");
    assert!(l.source.as_ref().unwrap().sampled().is_some());
    assert!(l.dataset.features.iter().any(|f| f.geometry.as_ref().unwrap().extent().min_x > 150.0));

    // Zoomed in: exactly the features in view (padded), all of them.
    let small = rustgis_geom::Extent::new(10.0, 10.0, 14.0, 13.0);
    let v = view_of(&mut s, small);
    let lonlat = crate::render::view_lonlat(s.map(), &v);
    let scale = s.map().scale();
    // Read in the background, as the screen does, then wait for it.
    let mut pending = s.map_mut().sync_sources(&lonlat, scale, false);
    let start = std::time::Instant::now();
    while pending && start.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(5));
        pending = s.map_mut().sync_sources(&lonlat, scale, false);
    }
    assert!(!pending);
    let l = layer(&s);
    assert!(l.source.as_ref().unwrap().sampled().is_none());
    let w = l.source.as_ref().unwrap().window().unwrap().extent;
    let inside =
        |e: &rustgis_geom::Extent| l.dataset.features.iter().filter(|f| e.contains(f.geometry.as_ref().unwrap().extent().center())).count();
    assert_eq!(inside(&w), l.dataset.features.len());
    assert!(inside(&lonlat) > 0);

    // A selection survives the next read (by feature id).
    let ids: Vec<u64> = l.dataset.features.iter().take(3).map(|f| f.id).collect();
    s.map_mut().layer_mut(id).unwrap().selection = vec![0, 1, 2];
    let v = view_of(&mut s, rustgis_geom::Extent::new(-40.0, -40.0, 60.0, 60.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let l = layer(&s);
    let kept: Vec<u64> = l.selection.iter().map(|r| l.dataset.features[*r].id).collect();
    assert!(kept.iter().all(|k| ids.contains(k)) && !kept.is_empty(), "{kept:?}");

    // Edits are refused; Load All Features brings them into memory.
    s.run("edit.enable", &json!({"on": true})).unwrap();
    s.active_layer = Some(id);
    let e = s.run("edit.delete", &json!({"layer": id})).unwrap_err();
    assert!(e.contains("read in place"), "{e}");
    let r = s.run("layer.loadAll", &json!({"layer": id})).unwrap();
    assert_eq!(r["features"], 22_500);
    let l = layer(&s);
    assert!(!l.is_streamed() && !l.dataset.windowed);
    s.map_mut().layer_mut(id).unwrap().selection = vec![0];
    s.run("edit.delete", &json!({"layer": id})).unwrap();
    assert_eq!(layer(&s).dataset.features.len(), 22_499);
    let _ = std::fs::remove_file(path);
}

#[test]
fn streamed_layers_save_their_source_not_their_features() {
    let path = grid_fgb("saved", 60);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    let v = view_of(&mut s, rustgis_geom::Extent::new(-180.0, -80.0, 180.0, 80.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    assert_eq!(s.map().layers[0].dataset.features.len(), 3600);
    let text = serde_json::to_string(&s.project).unwrap();
    // The window isn't in the project (3,600 features would be far more than this).
    assert!(text.len() < 20_000, "{}", text.len());
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let l = &t.map().layers[0];
    assert!(l.is_streamed() && l.source.as_ref().unwrap().is_open());
    assert!(l.dataset.features.is_empty());
    let v = view_of(&mut t, rustgis_geom::Extent::new(-180.0, -80.0, 180.0, 80.0));
    crate::render::render_png(t.map_mut(), &v).unwrap();
    assert_eq!(t.map().layers[0].dataset.features.len(), 3600);
    // A missing file is reported, not a crash.
    // (The path as the project's JSON spells it: escaped backslashes on Windows.)
    let quoted = serde_json::to_string(&std::path::absolute(&path).unwrap().display().to_string()).unwrap();
    assert!(text.contains(quoted.trim_matches('"')), "{text}");
    let gone = text.replace(quoted.trim_matches('"'), "/nonexistent/gone.fgb");
    let mut u = Session::new(Project::default());
    u.run("project.openText", &json!({"text": gone})).unwrap();
    assert!(u.map().layers[0].source.as_ref().unwrap().error.is_some());
    crate::render::render_png(u.map_mut(), &v).unwrap();
    let _ = std::fs::remove_file(path);
}

#[test]
fn tools_read_streamed_layers_whole() {
    let path = grid_fgb("tools", 50);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    // Only a corner is in view.
    let v = view_of(&mut s, rustgis_geom::Extent::new(0.0, 0.0, 20.0, 20.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let windowed = s.map().layers[0].dataset.features.len();
    assert!(windowed < 2500, "{windowed}");
    let r = s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "tools", "distance": 1}})).unwrap();
    let out = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
    assert_eq!(out.dataset.features.len(), 2500);
    // A tool updating the layer from a selection (made in the window) would replace the whole
    // layer with that part: refused. Without a selection it updates the whole source.
    let id = s.map().layer_by_name("tools").unwrap().id;
    s.map_mut().layer_mut(id).unwrap().selection = vec![0, 1];
    let e = s.run("gp.run", &json!({"tool": "calculateGeometry", "params": {"input": "tools"}})).unwrap_err();
    assert!(e.contains("Load All Features"), "{e}");
    assert!(s.map().layer(id).unwrap().is_streamed());
    s.map_mut().layer_mut(id).unwrap().selection.clear();
    s.run("gp.run", &json!({"tool": "calculateGeometry", "params": {"input": "tools"}})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 2500);
    let _ = std::fs::remove_file(path);
}

#[test]
fn web_style_carries_the_layers_scale_limits_as_zooms() {
    let mut s = world();
    let id = s.map().layer_by_name("Cities").unwrap().id;
    let zooms = |s: &Session| {
        let style = crate::cmd::sharing::web_style(s.map().layer(id).unwrap());
        style["layers"].as_array().unwrap().iter().map(|l| (l.get("minzoom").cloned(), l.get("maxzoom").cloned())).collect::<Vec<_>>()
    };
    // No limits, no keys.
    assert!(zooms(&s).iter().all(|(a, b)| a.is_none() && b.is_none()));
    let c = s.map().layer(id).unwrap().extent().center();
    // Ground metres per pixel at the layer's latitude, and the MapLibre zoom (512-pixel world).
    let zoom_of = |scale: f64| (2.0 * std::f64::consts::PI * 6_378_137.0 * c.y.to_radians().cos() / (512.0 * scale * 0.00028)).log2();
    let l = s.map_mut().layer_mut(id).unwrap();
    l.min_scale = Some(5_000_000.0);
    l.max_scale = Some(100_000.0);
    l.labels.enabled = true;
    l.labels.default.field = "name".into();
    let z = zooms(&s);
    // The label layer is limited like the symbols.
    assert!(z.len() >= 2, "{z:?}");
    for (min, max) in z {
        assert!((min.as_ref().unwrap().as_f64().unwrap() - zoom_of(5_000_000.0)).abs() < 1e-3, "{min:?}");
        assert!((max.as_ref().unwrap().as_f64().unwrap() - zoom_of(100_000.0)).abs() < 1e-3, "{max:?}");
    }
    // One limit gives one key; nonsense gives none.
    let l = s.map_mut().layer_mut(id).unwrap();
    l.max_scale = Some(f64::NAN);
    l.min_scale = Some(-1.0);
    assert!(zooms(&s).iter().all(|(a, b)| a.is_none() && b.is_none()));
    s.map_mut().layer_mut(id).unwrap().max_scale = Some(100_000.0);
    assert!(zooms(&s).iter().all(|(a, b)| a.is_none() && b.is_some()));
    // A hidden Default class labels nothing on the map, nor in the style.
    let labelled = |s: &Session| {
        let style = crate::cmd::sharing::web_style(s.map().layer(id).unwrap());
        style["layers"].as_array().unwrap().iter().any(|l| l["type"] == "symbol")
    };
    assert!(labelled(&s));
    s.map_mut().layer_mut(id).unwrap().labels.default.visible = false;
    assert!(!labelled(&s));
}

#[test]
fn web_shares_carry_every_feature_of_a_streamed_layer() {
    let path = grid_fgb("webshare", 50);
    let dir = std::env::temp_dir().join(format!("rustgis-webshare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    // Only a corner is in view.
    let v = view_of(&mut s, rustgis_geom::Extent::new(0.0, 0.0, 20.0, 20.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    assert!(s.map().layers[0].dataset.features.len() < 2500);
    let count = |zip: &std::path::Path, name: &str| -> usize {
        let z = rustgis_data::zip::read(&std::fs::read(zip).unwrap()).unwrap();
        let items = z.iter().find(|e| e.0 == name).unwrap();
        let v: Value = serde_json::from_slice(&items.1).unwrap();
        v["features"].as_array().unwrap().len()
    };
    let layer_zip = dir.join("layer.zip");
    s.run("share.webLayer", &json!({"layer": "webshare", "path": layer_zip.display().to_string()})).unwrap();
    assert_eq!(count(&layer_zip, "collections/webshare/items.json"), 2500);
    let service_zip = dir.join("service.zip");
    s.run("share.geodataService", &json!({"path": service_zip.display().to_string()})).unwrap();
    assert_eq!(count(&service_zip, "collections/webshare/items.json"), 2500);
    // A scene from the corner view matches one made with every feature in hand.
    let scene = dir.join("scene.gltf");
    s.run("share.webScene", &json!({"path": scene.display().to_string()})).unwrap();
    let windowed = std::fs::read(&scene).unwrap();
    let all = s.map().layers[0].load_all().unwrap();
    s.map_mut().layers[0].dataset = all;
    s.run("share.webScene", &json!({"path": scene.display().to_string()})).unwrap();
    assert!(windowed == std::fs::read(&scene).unwrap(), "the scene depends on the view");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(path);
}

/// MapLibre map links: read with their bearing and pitch (or without), written back the same,
/// and junk refused without moving the map.
#[test]
fn map_links() {
    let mut s = world();
    let r = s.run("map.goToLink", &json!({"url": "https://example.com/map/#15.5/40.7484/-73.9857/30/60"})).unwrap();
    assert_eq!((r["bearing"].as_f64(), r["pitch"].as_f64()), (Some(30.0), Some(60.0)));
    let c = s.map().center_lonlat();
    assert!((c.x + 73.9857).abs() < 1e-6 && (c.y - 40.7484).abs() < 1e-6, "{c:?}");
    assert_eq!(s.run("map.link", &Value::Null).unwrap()["hash"], "#15.5/40.7484/-73.9857/30/60");
    // MapLibre's zoom 0 shows the world's 40,075 km across 512 pixels.
    s.run("map.goToLink", &json!({"url": "0/0/0"})).unwrap();
    let ground = s.map().camera.res * s.map().flat().ground_per_unit(s.map().camera.center);
    assert!((ground - 2.0 * std::f64::consts::PI * 6_378_137.0 / 512.0).abs() < 1.0, "{ground}");
    assert_eq!((s.map().camera.bearing, s.map().camera.pitch), (0.0, 0.0), "left out, they are zero");
    // A named hash among other parameters, and bearing without pitch.
    s.run("map.goToLink", &json!({"url": "https://example.com/#layers=a&map=4/10/20/-45"})).unwrap();
    assert_eq!(s.map().camera.bearing, -45.0);
    assert_eq!(s.run("map.link", &Value::Null).unwrap()["hash"], "#4/10/20/-45");
    // MapLibre's own `map=` before other parameters that look like positions.
    s.run("map.goToLink", &json!({"url": "#other=1/2/3&map=4/10/20"})).unwrap();
    assert_eq!(s.run("map.link", &Value::Null).unwrap()["hash"], "#4/10/20");
    // A view wider than zoom 0 gives a link that reads back; no "-0".
    s.map_mut().camera.res = rustgis_map::camera::MAX_RES;
    s.map_mut().camera.center = rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(-1e-9, 0.0));
    let hash = s.run("map.link", &Value::Null).unwrap()["hash"].as_str().unwrap().to_string();
    assert_eq!(hash, "#0/0/0");
    s.run("map.goToLink", &json!({"url": hash})).unwrap();
    assert!(crate::cmd::navigate::is_map_link("#map=4/10/20") && !crate::cmd::navigate::is_map_link("#about"));
    let before = s.map().camera;
    for bad in [
        json!({"url": "https://example.com/#about"}),
        json!({"url": "https://example.com/?a=1/2/3"}),
        json!({"url": "map=1/2/3"}),
        json!({"url": "#4/95/0"}),
        json!({"url": "#4/0/200"}),
        json!({"url": "#99/0/0"}),
        json!({"url": "#4/0"}),
        json!({"url": "#4/0/0/1/2/3"}),
        json!({"url": "#NaN/0/0"}),
        json!({"url": "#inf/0/0"}),
        json!({"url": "#4/0/0/1e400"}),
        json!({"url": "/".repeat(100_000)}),
        json!({"url": format!("#4/0/0{}", "é".repeat(100_000))}),
        json!({"url": format!("{}#4/0/0", "€".repeat(2000))}),
        json!({"url": 3}),
        json!({}),
    ] {
        assert!(s.run("map.goToLink", &bad).is_err(), "{bad}");
    }
    assert_eq!(s.map().camera, before);
}

/// A MapLibre style added with `"view": true` takes the map to its root center, zoom, bearing
/// and pitch once it has loaded; without it the map stays where it is.
#[test]
fn style_root_view_moves_the_map() {
    if std::env::var_os("RUSTGIS_TEST_STYLE_VIEW").is_none() {
        // Parallel reload tests clear the process-wide tile cache. Isolate these downloads:
        // refetching after a reload can exhaust the fixture server's request allowance.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::style_root_view_moves_the_map", "--nocapture"])
            .env("RUSTGIS_TEST_STYLE_VIEW", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed; 0 failed"), "the child must run the style-view test: {stdout}");
        return;
    }
    const STYLE: &[u8] = br#"{"version": 8, "center": [2.2945, 48.8584], "zoom": 16, "bearing": -20, "pitch": 55, "sources": {}, "layers": [{"id": "bg", "type": "background"}]}"#;
    let base = serve(STYLE, 5);
    let mut s = world();
    let before = s.map().camera;
    s.run("map.addTiles", &json!({"url": format!("{base}/plain/style.json")})).unwrap();
    assert!(s.style_view.is_none());
    assert_eq!(s.map().camera, before);
    s.run("map.addTiles", &json!({"url": format!("{base}/view/style.json"), "view": true})).unwrap();
    // Loaded in the background; followed once it is.
    let start = std::time::Instant::now();
    while s.style_view.is_some() && start.elapsed() < std::time::Duration::from_secs(20) {
        crate::cmd::tiles::follow_style_view(&mut s, web_time::Duration::from_secs(5));
    }
    assert!(s.style_view.is_none());
    let m = s.map();
    let c = m.center_lonlat();
    assert!((c.x - 2.2945).abs() < 1e-6 && (c.y - 48.8584).abs() < 1e-6, "{c:?}; notices: {:?}", s.notices);
    assert_eq!((m.camera.bearing, m.camera.pitch), (-20.0, 55.0));
    assert_eq!(s.run("map.link", &Value::Null).unwrap()["hash"], "#16/48.8584/2.2945/-20/55");
    // Only a style has a view of its own.
    let e = s.run("map.addTiles", &json!({"url": "https://tiles.example.com/{z}/{x}/{y}.png", "view": true})).unwrap_err();
    assert!(e.contains("style"), "{e}");
    // A style that can't load is said, and forgotten.
    s.run("map.addTiles", &json!({"url": format!("{base}/missing/style.json"), "view": true})).unwrap();
    let start = std::time::Instant::now();
    while s.style_view.is_some() && start.elapsed() < std::time::Duration::from_secs(20) {
        crate::cmd::tiles::follow_style_view(&mut s, web_time::Duration::from_secs(5));
    }
    assert!(s.style_view.is_none());
    assert!(s.notices.iter().any(|n| n.error && n.text.contains("missing")), "{:?}", s.notices);
    // Moved while the style loads: the map stays where it was put.
    s.run("map.addTiles", &json!({"url": format!("{base}/moved/style.json"), "view": true})).unwrap();
    assert!(s.style_view.is_some());
    let panned = rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(10.0, 20.0));
    s.map_mut().camera.center = panned;
    crate::cmd::tiles::follow_style_view(&mut s, web_time::Duration::from_secs(5));
    assert!(s.style_view.is_none());
    assert_eq!(s.map().camera.center, panned);
    // A style still loading when another project opens doesn't move that one.
    s.run("map.addTiles", &json!({"url": "https://tiles.example.invalid/style.json", "view": true})).unwrap();
    assert!(s.style_view.is_some());
    s.run("project.new", &Value::Null).unwrap();
    assert!(s.style_view.is_none());
}

/// A web scene carries a camera at the map's viewpoint: straight down when the map is flat, and
/// from its bearing and pitch when it is turned and tilted.
#[test]
fn web_scene_camera() {
    let dir = std::env::temp_dir().join(format!("rustgis_scene_camera_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scene.gltf").display().to_string();
    let mut s = world();
    s.view_size = (800.0, 600.0);
    let scene = |s: &mut Session| -> Value {
        s.run("share.webScene", &json!({"path": path})).unwrap();
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
    };
    // Where a node's camera looks (-z turned by its rotation) and its up (+y).
    let axes = |n: &Value| {
        let q: Vec<f64> = n["rotation"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
        let turn = |v: [f64; 3]| {
            let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
            [v[0] + w * t[0] + (y * t[2] - z * t[1]), v[1] + w * t[1] + (z * t[0] - x * t[2]), v[2] + w * t[2] + (x * t[1] - y * t[0])]
        };
        (turn([0.0, 0.0, -1.0]), turn([0.0, 1.0, 0.0]))
    };
    let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-9);
    let g = scene(&mut s);
    let cam = g["nodes"].as_array().unwrap().iter().find(|n| n.get("camera").is_some()).unwrap().clone();
    assert_eq!(g["cameras"][0]["type"], "perspective");
    assert!((g["cameras"][0]["perspective"]["aspectRatio"].as_f64().unwrap() - 800.0 / 600.0).abs() < 1e-9);
    let (look, up) = axes(&cam);
    assert!(near(look, [0.0, -1.0, 0.0]) && near(up, [0.0, 0.0, -1.0]), "down, north up: {look:?} {up:?}");
    let t: Vec<f64> = cam["translation"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    // As far up as the map's scale needs: 300 points of ground under half the field of view.
    let ground = s.map().camera.res * s.map().flat().ground_per_unit(s.map().camera.center);
    assert!(t[0].abs() < 1e-6 && t[2].abs() < 1e-6 && (t[1] - 300.0 * 3.0 * ground).abs() < 1e-3 * t[1], "{t:?}");
    // Facing east, tilted 60°: behind the centre to the west, looking east and down.
    s.run("map.setBearing", &json!({"bearing": 90})).unwrap();
    s.run("map.setPitch", &json!({"pitch": 60})).unwrap();
    let g = scene(&mut s);
    assert_eq!((g["extras"]["bearing"].as_f64(), g["extras"]["pitch"].as_f64()), (Some(90.0), Some(60.0)));
    let cam = g["nodes"].as_array().unwrap().iter().find(|n| n.get("camera").is_some()).unwrap().clone();
    let (look, _) = axes(&cam);
    let (s60, c60) = (60f64.to_radians().sin(), 60f64.to_radians().cos());
    assert!(near(look, [s60, -c60, 0.0]), "{look:?}");
    let t: Vec<f64> = cam["translation"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(t[0] < 0.0 && t[1] > 0.0 && t[2].abs() < 1e-6 * t[1], "{t:?}");
    // The camera looks at the centre (the origin).
    let d = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
    assert!(near([-t[0] / d, -t[1] / d, -t[2] / d], look), "{t:?} {look:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reloading_web_layers_clears_the_tile_cache() {
    use crate::render::rustgis_tiles::cache;
    let mut s = world();
    let before = cache::generation();
    let r = s.run("map.reloadWebLayers", &Value::Null).unwrap();
    // Other tests share the cache, so the generation may have moved further.
    assert!(r["generation"].as_u64().unwrap() > before);
    // Parameters are ignored.
    s.run("map.reloadWebLayers", &json!({"junk": [1, null, "x"]})).unwrap();
}

#[test]
fn a_streamed_layer_whose_file_goes_away_shows_an_error_not_stale_features() {
    let path = grid_fgb("vanishing", 40);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    let v = view_of(&mut s, rustgis_geom::Extent::new(0.0, 0.0, 30.0, 30.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    assert!(!s.map().layers[0].dataset.features.is_empty());
    std::fs::remove_file(&path).unwrap();
    let v = view_of(&mut s, rustgis_geom::Extent::new(-150.0, -60.0, -120.0, -30.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let l = &s.map().layers[0];
    assert!(l.dataset.features.is_empty());
    assert!(l.source.as_ref().unwrap().error.is_some());
}

/// `layout.publication` over a streamed layer: the frame's view is read for its legend, which lists
/// the layer; undone, the layout goes and the layer is streaming from its source as before,
/// reading a view again when one is drawn.
#[test]
fn a_publication_layout_over_a_streamed_layer_is_undone() {
    let path = grid_fgb("publication", 30);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    let r = s.run("layout.publication", &json!({"extent": [0, 0, 20, 20], "author": ""})).unwrap();
    assert!(s.project.layouts[0].elements.iter().any(|e| e.name == "Legend"), "the read layer is listed: {r}");
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.project.layouts.is_empty());
    let l = &s.map().layers[0];
    assert!(l.source.as_ref().is_some_and(|src| src.error.is_none()), "still streaming from its source");
    let v = view_of(&mut s, rustgis_geom::Extent::new(0.0, 0.0, 20.0, 20.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    assert!(!s.map().layers[0].dataset.features.is_empty(), "it reads the view again");
    let _ = std::fs::remove_file(path);
}

#[test]
fn any_command_editing_a_streamed_layer_is_undone() {
    let path = grid_fgb("guarded", 30);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string(), "stream": true})).unwrap();
    // A sample of the world, so a closer view reads again.
    s.map_mut().layers[0].source.as_mut().unwrap().limit = 300;
    let v = view_of(&mut s, rustgis_geom::Extent::new(-180.0, -80.0, 180.0, 80.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let before = s.map().layers[0].dataset.features.clone();
    // Whatever the command, an edit of the window is undone and refused.
    let r = s.guarded("Some Edit", true, |s| {
        if let Some(f) = s.map_mut().layers[0].dataset.features.get_mut(0) {
            f.attrs[0] = rustgis_data::Value::Int(-1);
        }
        Ok(Value::Null)
    });
    assert!(r.unwrap_err().contains("Load All Features"));
    assert_eq!(s.map().layers[0].dataset.features, before);
    // A command whose render reads the source again is fine.
    let r = s.guarded("Render", true, |s| {
        let v = view_of(s, rustgis_geom::Extent::new(0.0, 0.0, 20.0, 20.0));
        crate::render::render_png(s.map_mut(), &v).map(|_| Value::Null)
    });
    assert!(r.is_ok(), "{r:?}");
    assert_ne!(s.map().layers[0].dataset.features.len(), before.len());
    let _ = std::fs::remove_file(path);
}

/// A GDAL-only format (GeoJSON sequence), when GDAL is installed (skipped otherwise).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn gdal_formats_open_when_gdal_is_installed() {
    if !rustgis_data::source::gdal::available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rustgis-engine-gdal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let seq = dir.join("walk.geojsonl");
    std::fs::write(
        &seq,
        "{\"type\":\"Feature\",\"properties\":{\"name\":\"start\"},\"geometry\":{\"type\":\"Point\",\"coordinates\":[7,45]}}\n{\"type\":\"Feature\",\"properties\":{\"name\":\"end\"},\"geometry\":{\"type\":\"Point\",\"coordinates\":[7.1,45.1]}}\n",
    )
    .unwrap();
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": seq.display().to_string()})).unwrap();
    let l = s.map().layers.iter().find(|l| l.dataset.features.len() == 2).unwrap_or_else(|| panic!("{r}"));
    assert_eq!(l.dataset.source.as_deref(), Some(std::path::absolute(&seq).unwrap().display().to_string().as_str()));
    // Streamed, the layer remembers the original file.
    let r = s.run("layer.addData", &json!({"path": seq.display().to_string(), "stream": true})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().gdal.is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// KML (with its styles), KMZ and GPX through the built-in readers: files on disk and uploads.
#[test]
fn kml_and_gpx_add_as_layers() {
    let dir = std::env::temp_dir().join(format!("rustgis-engine-kml-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let kml = r##"<kml xmlns="http://www.opengis.net/kml/2.2"><Document>
<Style id="red"><LineStyle><color>ff0000ff</color><width>4</width></LineStyle></Style>
<Style id="blue"><LineStyle><color>ffff0000</color></LineStyle></Style>
<Placemark><name>a</name><styleUrl>#red</styleUrl><LineString><coordinates>7,45 8,46</coordinates></LineString></Placemark>
<Placemark><name>b</name><styleUrl>#blue</styleUrl><LineString><coordinates>7,46 8,45</coordinates></LineString></Placemark>
<Placemark><name>p</name><Point><coordinates>7.5,45.5</coordinates></Point></Placemark>
</Document></kml>"##;
    let path = dir.join("roads.kml");
    std::fs::write(&path, kml).unwrap();
    let mut s = Session::new(Project::default());
    let listed = s.run("info.fileLayers", &json!({"path": path.display().to_string()})).unwrap();
    assert_eq!(listed["layers"], json!(["roads points", "roads lines"]));
    let r = s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(2), "{r}");
    let lines = s.map().layer_by_name("roads lines").unwrap();
    match &lines.renderer {
        rustgis_map::Renderer::Unique { field, classes, .. } => {
            assert_eq!(field, "style");
            let red = classes.iter().find(|(k, _)| k == "red").unwrap().1;
            assert_eq!((red.stroke, red.stroke_width), (rustgis_map::Rgba::new(255, 0, 0, 255), 4.0));
        }
        other => panic!("{other:?}"),
    }
    // One layer by name.
    let one = s.run("layer.addData", &json!({"path": path.display().to_string(), "layer": "lines"})).unwrap();
    assert_eq!(one["layers"].as_array().map(Vec::len), Some(1));
    assert!(s.run("layer.addData", &json!({"path": path.display().to_string(), "layer": "nope"})).is_err());
    // A KMZ upload (the web app).
    {
        use base64::Engine;
        let kmz = rustgis_data::zip::write(&[("doc.kml".to_string(), kml.as_bytes().to_vec())]).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(kmz);
        let r = s.run("layer.addDataBytes", &json!({"fileName": "roads.kmz", "data": data})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(2), "{r}");
    }
    // GPX: waypoints, tracks and track points.
    let gpx = dir.join("walk.gpx");
    std::fs::write(
        &gpx,
        r#"<?xml version="1.0"?><gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
<wpt lat="45.0" lon="7.0"><name>start</name></wpt><wpt lat="45.1" lon="7.1"><name>end</name></wpt>
<trk><name>walk</name><trkseg><trkpt lat="45.0" lon="7.0"/><trkpt lat="45.05" lon="7.05"/><trkpt lat="45.1" lon="7.1"/></trkseg></trk></gpx>"#,
    )
    .unwrap();
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": gpx.display().to_string()})).unwrap();
    let names: Vec<String> = s.map().layers.iter().map(|l| l.name.clone()).collect();
    for n in ["walk waypoints", "walk tracks", "walk track points"] {
        assert!(names.contains(&n.to_string()), "{names:?}");
    }
    assert_eq!(s.map().layer_by_name("walk waypoints").unwrap().dataset.features.len(), 2);
    let project = dir.join("p.rgis").display().to_string();
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    s.run("project.open", &json!({"path": project})).unwrap();
    assert_eq!(s.map().layers.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

/// SQL queries and DuckDB databases through DuckDB, when it is installed; without it, both say
/// how to get it.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn duckdb_queries_and_databases() {
    let mut s = Session::new(Project::default());
    let query = "SELECT 'a' AS name, ST_Point(7, 45) AS geom UNION ALL SELECT 'b', ST_GeomFromText('LINESTRING(7 45, 8 46)')";
    if !rustgis_data::source::duckdb::available() {
        let e = s.run("layer.addSql", &json!({"sql": query})).unwrap_err();
        assert!(e.contains("need DuckDB"), "{e}");
        let e = s.run("layer.addData", &json!({"path": "/nowhere/x.duckdb"})).unwrap_err();
        assert!(e.contains("need DuckDB"), "{e}");
        return;
    }
    // A result mixing geometry types: a layer per type.
    let r = s.run("layer.addSql", &json!({"sql": query, "name": "Trip"})).unwrap();
    let names: Vec<String> = s.map().layers.iter().map(|l| l.name.clone()).collect();
    assert_eq!(names, vec!["Trip (points)", "Trip (lines)"], "{r}");
    // Streamed, a layer remembers its query; a project never runs it again when the result is gone
    // (a project file is hostile input), and says how to.
    let r = s
        .run(
            "layer.addSql",
            &json!({"sql": "SELECT i AS id, ST_Point(i, i / 2) AS geom FROM range(50) t(i)", "name": "Pts", "stream": true}),
        )
        .unwrap();
    assert_eq!(r["features"], 50);
    let id = r["layer"].as_u64().unwrap() as u32;
    let src = s.map().layer(id).unwrap().source.clone().unwrap();
    let d = src.duckdb.clone().unwrap();
    assert!(d.sql.as_deref().is_some_and(|q| q.contains("range(50)")) && d.family == rustgis_data::source::Family::Points);
    let cached = src.path.clone().unwrap();
    let text = serde_json::to_string(&s.project).unwrap();
    std::fs::remove_file(&cached).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let l = t.map().layer_by_name("Pts").unwrap();
    let again = l.source.as_ref().unwrap();
    assert!(!again.is_open());
    assert!(again.error.as_deref().is_some_and(|e| e.contains("run the query again")), "{:?}", again.error);
    // Errors are DuckDB's, and nothing is added.
    let before = s.map().layers.len();
    assert!(s.run("layer.addSql", &json!({"sql": "SELECT 1 AS a"})).unwrap_err().contains("no geometry column"));
    assert!(s.run("layer.addSql", &json!({"sql": "SELECT * FROM nowhere"})).unwrap_err().contains("nowhere"));
    assert_eq!(s.map().layers.len(), before);

    // A database file: its geometry tables become layers, or the one named.
    let dir = std::env::temp_dir().join(format!("rustgis-engine-duckdb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("places.duckdb");
    let ok = std::process::Command::new(rustgis_data::source::duckdb::program().unwrap())
        .arg(&db)
        .arg("-c")
        .arg("LOAD spatial; CREATE TABLE towns AS SELECT 'x' AS name, ST_Point(2.35, 48.85) AS geom; CREATE TABLE lakes AS SELECT ST_GeomFromText('POLYGON((6 46, 7 46, 7 47, 6 46))') AS geom; CREATE TABLE notes(t TEXT);")
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": db.display().to_string()})).unwrap();
    let mut names: Vec<String> = s.map().layers.iter().map(|l| l.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["lakes", "towns"], "{r}");
    assert_eq!(
        s.map().layer_by_name("towns").unwrap().dataset.source.as_deref(),
        Some(std::path::absolute(&db).unwrap().display().to_string().as_str())
    );
    let r = s.run("layer.addData", &json!({"path": db.display().to_string(), "layer": "towns", "stream": true, "name": "T"})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    let src = s.map().layer(id).unwrap().source.clone().unwrap();
    assert_eq!(src.duckdb.as_ref().unwrap().table.as_deref(), Some("towns"));
    // A table is converted again when its result is gone.
    let text = serde_json::to_string(&s.project).unwrap();
    std::fs::remove_file(src.path.as_deref().unwrap()).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("T").unwrap().source.as_ref().unwrap();
    assert!(again.is_open(), "{:?}", again.error);
    assert_eq!(again.info().and_then(|i| i.count), Some(1));
    assert!(s.run("layer.addData", &json!({"path": db.display().to_string(), "layer": "notes"})).is_err());
    // Its layers are listed without adding them.
    let listed = s.run("info.fileLayers", &json!({"path": db.display().to_string()})).unwrap();
    assert_eq!(listed, json!({"count": 2, "layers": ["lakes", "towns"]}));
    // The database commands take a DuckDB file too.
    let listed = s.run("info.databaseTables", &json!({"database": db.display().to_string()})).unwrap();
    assert_eq!(listed, json!({"count": 2, "layers": ["lakes", "towns"]}));
    let r = s.run("layer.addDatabase", &json!({"database": db.display().to_string(), "layer": "lakes", "name": "L"})).unwrap();
    assert_eq!(r["features"], 1);
    // A query over the database's tables.
    let r = s.run("layer.addSql", &json!({"sql": "SELECT * FROM towns", "database": db.display().to_string(), "name": "Q"})).unwrap();
    assert_eq!(r["features"], 1);
    // Projected coordinates that don't say their system are pointed out; "crs" converts them.
    let projected = "SELECT ST_Point(500000, 4200000) AS geom";
    s.run("layer.addSql", &json!({"sql": projected, "name": "Utm"})).unwrap();
    assert!(s.notices.iter().any(|n| n.error && n.text.starts_with("Utm: the data doesn't say its coordinate system")));
    let r = s.run("layer.addSql", &json!({"sql": projected, "name": "Utm2", "crs": "EPSG:32633"})).unwrap();
    let e = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().extent();
    assert!((e.min_x - 15.0).abs() < 0.01 && (e.min_y - 37.95).abs() < 0.05, "{e:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// PostgreSQL/PostGIS through DuckDB, when RUSTGIS_TEST_POSTGRES names a database with a
/// password (`postgres:host=… dbname=… user=… password=…`) holding a PostGIS table.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn postgres_databases() {
    let Ok(conn) = std::env::var("RUSTGIS_TEST_POSTGRES") else { return };
    if !rustgis_data::source::duckdb::available() {
        return;
    }
    let mut s = Session::new(Project::default());
    let listed = s.run("info.databaseTables", &json!({"database": conn})).unwrap();
    let first = listed["layers"][0].as_str().unwrap().to_string();
    let r = s.run("layer.addDatabase", &json!({"database": conn, "layer": first, "name": "P", "stream": true})).unwrap();
    assert!(r["features"].as_u64().unwrap() > 0, "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    let src = s.map().layer(id).unwrap().source.clone().unwrap();
    // The project names the connection without its password, and converts the table again.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("postgres:") && !text.contains("password"), "{text}");
    std::fs::remove_file(src.path.as_deref().unwrap()).unwrap();
    // A project never connects to its server on its own (it would hand over the user's
    // credentials to whatever host the file names): the layer says to add the table again.
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("P").unwrap().source.as_ref().unwrap();
    assert!(!again.is_open() && again.error.as_deref().is_some_and(|e| e.contains("add it again")), "{:?}", again.error);
    // A query over the database's tables.
    let r = s.run("layer.addSql", &json!({"sql": format!("SELECT * FROM {first} LIMIT 3"), "database": conn, "name": "Q"})).unwrap();
    assert!(r["features"].as_u64().unwrap() <= 3, "{r}");
}

#[test]
fn pmtiles_archives_become_tile_layers() {
    let mut s = Session::new(Project::default());
    // A file added as data is a tile layer of kind pmtiles (read when the map draws).
    let path = std::env::temp_dir().join("rustgis-test-archive.pmtiles");
    s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
    let t = &s.map().tiles[0];
    assert_eq!(t.kind, rustgis_map::TileKind::PmTiles);
    assert_eq!(t.name, "rustgis-test-archive");
    // A web address is guessed from its extension.
    s.run("map.addTiles", &json!({"url": "https://example.com/data/world.pmtiles"})).unwrap();
    assert!(s.map().tiles.iter().any(|t| t.kind == rustgis_map::TileKind::PmTiles && t.url.starts_with("https://")));
    // A relative path or another kind of file is refused.
    assert!(s.run("map.addTiles", &json!({"url": "world.pmtiles", "kind": "pmtiles"})).is_err());
    assert!(s.run("map.addTiles", &json!({"url": "/tmp/x.txt", "kind": "pmtiles"})).is_err());
    // A project keeps the layer.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(t.map().tiles.iter().filter(|x| x.kind == rustgis_map::TileKind::PmTiles).count(), 2);
}

/// Identify reports the features of a vector PMTiles archive under the point (topmost and
/// visible modes), with their tile and source layers; hidden layers and the selectable mode don't.
#[test]
fn identify_reports_vector_tile_features() {
    use crate::render::rustgis_tiles::{mvt, pmtiles};
    let park = mvt::Feature {
        id: Some(3),
        kind: mvt::GeomKind::Polygon,
        geometry: vec![vec![[1024.0, 1024.0], [3072.0, 1024.0], [3072.0, 3072.0], [1024.0, 3072.0], [1024.0, 1024.0]]],
        props: vec![("name".into(), mvt::Value::String("Central".into())), ("visitors".into(), mvt::Value::Number(1200.0))],
    };
    let tile = mvt::encode(&mvt::Tile { layers: vec![mvt::Layer { name: "parks".into(), extent: 4096, features: vec![park] }] });
    let new = pmtiles::NewArchive {
        tile_type: pmtiles::TileType::Mvt,
        gzipped_tiles: false,
        bounds: [-180.0, -85.0, 180.0, 85.0],
        metadata: json!({"vector_layers": [{"id": "parks"}]}),
    };
    let dir = std::env::temp_dir().join(format!("rustgis-identify-tiles-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("parks.pmtiles");
    std::fs::write(&path, pmtiles::write(vec![(0, 0, 0, tile)], &new).unwrap()).unwrap();

    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
    // Zoom 0, the park in the middle of the world.
    s.map_mut().camera.center = rustgis_geom::Coord::new(0.0, 0.0);
    s.map_mut().camera.res = 2.0 * 20_037_508.342_789_244 / 256.0;
    let identify = |s: &mut Session, p: Value| {
        // The archive and its tile arrive in the background.
        for _ in 0..500 {
            let r = s.run("identify", &p).unwrap();
            if !r.is_null() {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Value::Null
    };
    let r = identify(&mut s, json!({"lon": 1.0, "lat": 1.0}));
    assert_eq!(r["tileLayer"], "parks", "{r}");
    assert_eq!(r["sourceLayer"], "parks");
    assert_eq!(r["layer"], "parks › parks");
    assert_eq!(r["attributes"]["name"], "Central");
    assert_eq!(r["attributes"]["visitors"], "1200");
    assert!(r.get("layerId").is_none(), "{r}");
    assert_eq!(r["results"].as_array().map(Vec::len), Some(1));
    // Outside the park, in the selectable mode, or hidden: nothing.
    assert!(s.run("identify", &json!({"lon": 150.0, "lat": -70.0})).unwrap().is_null());
    assert!(s.run("identify", &json!({"lon": 1.0, "lat": 1.0, "mode": "selectable"})).unwrap().is_null());
    s.run("map.setTiles", &json!({"index": 0, "visible": false})).unwrap();
    assert!(s.run("identify", &json!({"lon": 1.0, "lat": 1.0})).unwrap().is_null());
    let _ = std::fs::remove_dir_all(dir);
}

/// A tile-less PMTiles archive (an empty root directory) whose header gives these bounds
/// (west, south, east, north).
fn empty_pmtiles(bounds: [f64; 4]) -> Vec<u8> {
    let mut h = b"PMTiles".to_vec();
    h.push(3);
    // Root directory (one byte: no entries), no metadata, leaves or tiles.
    for v in [127u64, 1, 128, 0, 128, 0, 128, 0, 0, 0, 0] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    h.extend_from_slice(&[1, 1, 1, 1, 0, 6]); // clustered, no compression, vector tiles, zooms 0–6
    for v in bounds {
        h.extend_from_slice(&((v * 1e7) as i32).to_le_bytes());
    }
    h.extend_from_slice(&[0u8; 9]);
    h.push(0);
    h
}

/// An archive added frames the map on its bounds once its header is read, unless it covers the
/// whole world; Zoom To Tile Layer goes there again, and a layer that can't say is refused.
#[test]
fn pmtiles_archives_frame_the_map_on_their_bounds() {
    let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-frame-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let iceland = dir.join("iceland.pmtiles");
    std::fs::write(&iceland, empty_pmtiles([-24.5, 63.3, -13.5, 66.6])).unwrap();
    let world = dir.join("world.pmtiles");
    std::fs::write(&world, empty_pmtiles([-180.0, -85.0, 180.0, 85.0])).unwrap();
    let center = |s: &Session| rustgis_geom::merc_to_lonlat(s.map().camera.center);
    let wait = |s: &mut Session| {
        let start = std::time::Instant::now();
        while s.style_view.is_some() && start.elapsed() < std::time::Duration::from_secs(20) {
            crate::cmd::tiles::follow_style_view(s, web_time::Duration::from_secs(5));
        }
    };

    let mut s = Session::new(Project::default());
    // A file's header is read as it is added: the next command sees the map moved.
    s.run("layer.addData", &json!({"path": iceland.display().to_string()})).unwrap();
    assert!(s.style_view.is_none());
    let c = center(&s);
    assert!((c.x + 19.0).abs() < 0.5 && (c.y - 65.0).abs() < 1.5, "{c:?}");

    // A world archive leaves the map where it is.
    s.run("map.zoomToExtent", &json!({"extent": [0.0, 0.0, 10.0, 10.0]})).unwrap();
    let before = s.map().camera.center;
    s.run("map.addTiles", &json!({"url": world.display().to_string()})).unwrap();
    wait(&mut s);
    assert_eq!(s.map().camera.center, before);

    // Zoom To Tile Layer goes back to the first; a layer with no bounds is refused.
    let r = s.run("map.zoomToTiles", &json!({"name": "iceland"})).unwrap();
    assert_eq!(r["bounds"][0].as_f64().map(|w| (w + 24.5).abs() < 1e-6), Some(true), "{r}");
    let c = center(&s);
    assert!((c.x + 19.0).abs() < 0.5, "{c:?}");
    s.run("map.addTiles", &json!({"url": "https://tile.example.com/{z}/{x}/{y}.png", "name": "xyz"})).unwrap();
    assert!(s.run("map.zoomToTiles", &json!({"name": "xyz"})).is_err());
    assert!(s.run("map.zoomToTiles", &json!({})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn explicit_tile_bounds_are_rejected_without_replacing_the_layer() {
    let mut s = Session::new(Project::default());
    let url = "https://tiles.example.com/{z}/{x}/{y}.png";
    s.run("map.addTiles", &json!({"url": url, "name": "Coverage", "bounds": [5, 45, 15, 55]})).unwrap();
    let before = serde_json::to_value(&s.project).unwrap();
    for bounds in [
        json!([20, 0, 10, 10]),
        json!([0, 20, 10, 10]),
        json!([10, 0, 10, 10]),
        json!([0, 10, 10, 10]),
        json!([-181, 0, 10, 10]),
        json!([0, 0, 181, 10]),
        json!([0, -91, 10, 10]),
        json!([0, 0, 10, 91]),
        json!([0, 0, 1e300, 10]),
        json!([0, 0, 10]),
        json!([0, "south", 10, 10]),
        json!("not bounds"),
    ] {
        let r = s.run("map.addTiles", &json!({"url": url, "name": "Coverage", "bounds": bounds, "replace": true}));
        assert!(r.is_err(), "{bounds}: {r:?}");
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "{bounds}");
    }
    let r = s.run("map.zoomToTiles", &json!({"name": "Coverage"})).unwrap();
    assert_eq!(r["bounds"], json!([5.0, 45.0, 15.0, 55.0]));
}

/// Bad optional coverage must not prevent loading the tiles or the rest of the project.
#[test]
fn malformed_saved_tile_bounds_do_not_prevent_opening_the_project() {
    let mut s = world();
    let url = "https://tiles.example.com/{z}/{x}/{y}.png";
    s.run("map.addTiles", &json!({"url": url, "name": "Coverage", "bounds": [5, 45, 15, 55], "opacity": 0.5})).unwrap();
    s.run("map.addTiles", &json!({"url": "https://other.example.com/{z}/{x}/{y}.png", "name": "Valid", "bounds": [1, 2, 3, 4]})).unwrap();
    let mut project = serde_json::to_value(&s.project).unwrap();
    let coverage = project["maps"][0]["tiles"].as_array().unwrap().iter().position(|t| t["name"] == "Coverage").unwrap();
    for bounds in [
        json!([0, 0, 10]),
        json!([0, 0, 10, 10, 20]),
        json!([0, "south", 10, 10]),
        json!([0, {"nested": [1, 2]}, 10, 10]),
        json!([[0], 0, 10, 10]),
        json!([]),
        json!({"nested": [1, 2]}),
        json!("x"),
        json!(true),
        json!(42),
        Value::Null,
        json!([20, 0, 10, 10]),
    ] {
        project["maps"][0]["tiles"][coverage]["bounds"] = bounds.clone();
        let loaded = Project::from_json(&project.to_string()).unwrap_or_else(|e| panic!("{bounds}: {e}"));
        let tile = loaded.map().tiles.iter().find(|t| t.name == "Coverage").unwrap();
        assert_eq!(tile.bounds, None, "{bounds}");
        assert_eq!((tile.url.as_str(), tile.opacity, tile.visible), (url, 0.5, true), "{bounds}");
        assert_eq!(loaded.map().tiles.iter().find(|t| t.name == "Valid").unwrap().bounds, Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(
            loaded.map().layer_by_name("Countries").unwrap().dataset.features.len(),
            s.map().layer_by_name("Countries").unwrap().dataset.features.len()
        );
    }
    project["maps"][0]["tiles"][coverage].as_object_mut().unwrap().remove("bounds");
    let loaded = Project::from_json(&project.to_string()).unwrap();
    assert_eq!(loaded.map().tiles.iter().find(|t| t.name == "Coverage").unwrap().bounds, None);
}

/// An MBTiles file of one red picture tile: added by path or as an upload, it is a tile layer that
/// draws, survives a project round trip (by path), and a file that isn't an archive is refused.
#[test]
fn mbtiles_archives_become_tile_layers() {
    use crate::render::View;
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-mbtiles-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("red.mbtiles");
    let _ = std::fs::remove_file(&path);
    let mut red = tiny_skia::Pixmap::new(256, 256).unwrap();
    red.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE metadata (name text, value text); CREATE TABLE tiles (zoom_level integer, tile_column integer, tile_row integer, tile_data blob);
         INSERT INTO metadata VALUES ('name', 'Red'), ('format', 'png'), ('minzoom', '0'), ('maxzoom', '0'), ('attribution', '© Red');",
    )
    .unwrap();
    c.execute("INSERT INTO tiles VALUES (0, 0, 0, ?1)", [red.encode_png().unwrap()]).unwrap();
    drop(c);
    let p = path.display().to_string();

    let draws_red = |s: &mut Session| {
        s.run("map.goToXY", &json!({"lon": 10.0, "lat": 20.0, "scale": 50_000_000})).unwrap();
        let v = View { width: 300.0, height: 200.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
        let mut px = None;
        for _ in 0..50 {
            let r = crate::render::render(s.map_mut(), &v).unwrap();
            px = r.pixmap.pixel(150, 100);
            if px.is_some_and(|p| p.red() > 200 && p.green() < 60) {
                return;
            }
            crate::render::rustgis_tiles::cache::wait(std::time::Duration::from_millis(100));
        }
        panic!("the tile layer didn't draw: {px:?}");
    };

    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": p})).unwrap();
    let t = &s.map().tiles[0];
    assert_eq!((t.kind, t.name.as_str()), (rustgis_map::TileKind::MbTiles, "red"));
    draws_red(&mut s);
    // A project keeps the layer by its path.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(t.map().tiles.first().map(|x| x.kind), Some(rustgis_map::TileKind::MbTiles));
    draws_red(&mut t);

    // An upload is held in memory.
    let b64 = base64::engine::general_purpose::STANDARD.encode(std::fs::read(&path).unwrap());
    let mut u = Session::new(Project::default());
    u.run("layer.addDataBytes", &json!({"fileName": "uploaded.mbtiles", "data": b64})).unwrap();
    assert_eq!(u.map().tiles[0].url, "upload:uploaded.mbtiles");
    draws_red(&mut u);

    // Not an archive, a relative path, a network share, an upload never made.
    let junk = dir.join("junk.mbtiles");
    std::fs::write(&junk, b"not sqlite").unwrap();
    assert!(s.run("layer.addData", &json!({"path": junk.display().to_string()})).is_err());
    let junk64 = base64::engine::general_purpose::STANDARD.encode(b"not sqlite");
    assert!(s.run("layer.addDataBytes", &json!({"fileName": "junk.mbtiles", "data": junk64})).is_err());
    for bad in ["red.mbtiles", r"\\attacker\share\x.mbtiles", "/tmp/x.pmtiles", "upload:"] {
        assert!(s.run("map.addTiles", &json!({"url": bad, "kind": "mbtiles"})).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Delimited text in a projected system, by path, text and upload; WKT columns; bad options.
#[test]
fn delimited_text_with_a_coordinate_system() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-csv-crs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("parks.tsv");
    std::fs::write(&path, "name\tX\tY\nCentral Park\t992000\t220000\n").unwrap();
    let p = path.display().to_string();
    let near_nyc = |s: &Session, id: &Value| {
        let e = s.map().layer(id.as_u64().unwrap() as u32).unwrap().extent();
        assert!((e.min_x + 73.96).abs() < 0.05 && (e.min_y - 40.78).abs() < 0.05, "{e:?}");
    };
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": p, "crs": "EPSG:2263"})).unwrap();
    near_nyc(&s, &r["layer"]);
    let text = "id;east;north\n1;992000;220000\n";
    let r = s.run("layer.addDataText", &json!({"fileName": "a.csv", "text": text, "x": "east", "y": "north", "crs": "2263"})).unwrap();
    near_nyc(&s, &r["layer"]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let r = s
        .run("layer.addDataBytes", &json!({"fileName": "b.csv", "data": b64, "x": "east", "y": "north", "crs": "EPSG:2263", "name": "Up"}))
        .unwrap();
    near_nyc(&s, &r["layer"]);
    assert!(s.map().layer_by_name("Up").is_some());
    // Without options a WKT column is found; with a SRID it is placed.
    let r = s.run("layer.addDataText", &json!({"fileName": "w.csv", "text": "wkt,n\n\"SRID=2263;POINT (992000 220000)\",1\n"})).unwrap();
    near_nyc(&s, &r["layer"]);
    for (bad, why) in [
        (json!({"crs": "nowhere"}), "unknown system"),
        (json!({"x": "missing", "y": "north"}), "missing column"),
        (json!({"x": "east"}), "x without y"),
        (json!({"delimiter": ";;"}), "two-character delimiter"),
    ] {
        let mut q = json!({"fileName": "a.csv", "text": text});
        for (k, v) in bad.as_object().unwrap() {
            q[k] = v.clone();
        }
        assert!(s.run("layer.addDataText", &q).is_err(), "{why}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Serves `body` to each of `n` requests on a local port; returns the base address.
pub(crate) fn serve(body: &'static [u8], n: usize) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(n) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let read = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]);
            let status = if req.contains("/missing") { "404 Not Found" } else { "200 OK" };
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(body);
        }
    });
    format!("http://{addr}")
}

/// Data added by its web address: the format from the address or, without one, the contents; a
/// page that isn't data, a missing file and a forbidden address are refused.
#[test]
fn data_from_a_web_address() {
    const GEOJSON: &[u8] = br#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"n": 1}, "geometry": {"type": "Point", "coordinates": [10, 20]}}]}"#;
    let base = serve(GEOJSON, 3);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/places.geojson")})).unwrap();
    assert_eq!(r["features"], 1);
    assert!(s.map().layer_by_name("places").is_some());
    // An API address without an extension: the contents say GeoJSON.
    s.run("layer.addUrl", &json!({"url": format!("{base}/api/items?f=json"), "name": "Api"})).unwrap();
    assert!(s.map().layer_by_name("Api").is_some());
    let e = s.run("layer.addUrl", &json!({"url": format!("{base}/missing.geojson")})).unwrap_err();
    assert!(e.contains("404"), "{e}");
    let page = serve(b"<!doctype html><html>not data</html>", 1);
    let e = s.run("layer.addUrl", &json!({"url": format!("{page}/index")})).unwrap_err();
    assert!(e.contains("not a data format"), "{e}");
    for bad in ["file:///etc/passwd", "/tmp/x.geojson", "http://169.254.169.254/latest.geojson", ""] {
        assert!(s.run("layer.addUrl", &json!({"url": bad})).is_err(), "{bad}");
    }
    assert_eq!(crate::cmd::layer::url_file_name("https://x.org/a/b/roads.parquet?sig=1", b"PAR1").unwrap(), "roads.parquet");
    assert_eq!(crate::cmd::layer::url_file_name("https://x.org/a/b/", b"PAR1").unwrap(), "b.parquet");
    assert_eq!(crate::cmd::layer::url_file_name("https://x.org", b"lon,lat\n1,2").unwrap(), "data.csv");
    assert_eq!(crate::cmd::layer::url_file_name("https://x.org/q.json", b"[]").unwrap(), "q.geojson");
}

/// JPEG 2000 from a web address or uploaded bytes is read through GDAL on the desktop, as a file
/// on disk is; contents that aren't JPEG 2000 never reach GDAL.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn jpeg2000_from_a_web_address() {
    use base64::Engine;
    let mut s = Session::new(Project::default());
    // A VRT (which would have GDAL read other files on this computer) named like JPEG 2000.
    let vrt = serve(br#"<VRTDataset rasterXSize="1" rasterYSize="1"></VRTDataset>"#, 1);
    let e = s.run("layer.addUrl", &json!({"url": format!("{vrt}/TCI.jp2")})).unwrap_err();
    assert!(e.contains("not a JPEG 2000 file"), "{e}");
    // Without a file name in the address, the contents alone say JPEG 2000 (a JP2 box or a bare
    // codestream); the same bytes are refused where GDAL can't run. A VRT stays an unknown format.
    let jp2_box = b"\0\0\0\x0cjP  \r\n\x87\n";
    let codestream = [0xff, 0x4f, 0xff, 0x51, 0x00];
    let named = |url: &str, bytes: &[u8]| crate::cmd::layer::url_file_name(url, bytes);
    assert_eq!(named("https://x.org/api/tile?f=jp2", jp2_box).unwrap(), "tile.jp2");
    assert_eq!(named("https://x.org/", &codestream).unwrap(), "data.j2k");
    assert!(named("https://x.org/api/tile", br#"<VRTDataset></VRTDataset>"#).unwrap_err().contains("not a data format"));
    if !rustgis_data::raster::gdal::available() {
        let jp2 = serve(jp2_box, 2);
        for path in ["TCI.jp2", "api/tile"] {
            let e = s.run("layer.addUrl", &json!({"url": format!("{jp2}/{path}")})).unwrap_err();
            assert!(e.contains("needs GDAL"), "{path}: {e}");
        }
        return;
    }
    let executable = if cfg!(windows) { "gdal_translate.exe" } else { "gdal_translate" };
    let translate = std::env::var_os("RUSTGIS_GDAL_BIN")
        .map(|d| std::path::PathBuf::from(d).join(executable))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| executable.into());
    let formats = std::process::Command::new(&translate).arg("--formats").output().expect("list GDAL drivers");
    assert!(formats.status.success(), "GDAL driver discovery failed: {}", String::from_utf8_lossy(&formats.stderr));
    if !String::from_utf8_lossy(&formats.stdout).lines().any(|line| line.split_whitespace().next() == Some("JP2OpenJPEG")) {
        eprintln!("skipping JPEG 2000 conversion: GDAL has no optional JP2OpenJPEG driver");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rustgis-engine-jp2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asc = dir.join("g.asc");
    std::fs::write(&asc, "ncols 4\nnrows 3\nxllcorner 10\nyllcorner 40\ncellsize 0.5\n1 2 3 4\n5 6 7 8\n9 10 11 12\n").unwrap();
    let jp2 = dir.join("g.jp2");
    let made = std::process::Command::new(&translate)
        .args(["-q", "-of", "JP2OpenJPEG", "-ot", "Byte", "-a_srs", "EPSG:4326"])
        .arg(&asc)
        .arg(&jp2)
        .output()
        .expect("generate JPEG 2000 fixture");
    assert!(made.status.success(), "JPEG 2000 fixture conversion failed ({}): {}", made.status, String::from_utf8_lossy(&made.stderr));
    // `serve` requires a 'static slice; intentionally leak this small test-only fixture so it
    // stays valid for the server thread's lifetime.
    let bytes = Vec::leak(std::fs::read(&jp2).unwrap());
    let uploaded = base64::engine::general_purpose::STANDARD.encode(&*bytes);
    let base = serve(bytes, 2);
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/tiles/33/T/XN/TCI.jp2")})).unwrap();
    let l = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
    assert_eq!(l.name, "TCI");
    let rl = l.raster.as_ref().unwrap();
    let data = rl.data.as_ref().unwrap();
    assert_eq!((data.width, data.height, data.georeferenced), (4, 3, true));
    let e = data.extent();
    assert!((e.min_x - 10.0).abs() < 1e-6 && (e.max_y - 41.5).abs() < 1e-6, "{e:?}");
    // The project keeps the converted GeoTIFF, so it opens again without GDAL.
    assert_eq!(rl.file_name.as_deref(), Some("TCI.tif"));
    assert!(rl.embedded.is_some() && rl.path.is_none());
    // The same file from an endpoint with no extension: named by its contents, converted alike.
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/api/tile")})).unwrap();
    let rl = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap();
    let raster = rl.data.as_ref().unwrap();
    assert_eq!((raster.width, raster.height, raster.georeferenced), (4, 3, true));
    assert_eq!(rl.file_name.as_deref(), Some("tile.tif"));
    assert!(rl.embedded.is_some() && rl.path.is_none());
    for file_name in ["uploaded.jp2", "uploaded.J2K"] {
        let mut upload = Session::new(Project::default());
        let r = upload.run("layer.addDataBytes", &json!({"fileName": file_name, "data": uploaded})).unwrap();
        let rl = upload.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap();
        let raster = rl.data.as_ref().unwrap();
        assert_eq!((raster.width, raster.height, raster.georeferenced), (4, 3, true));
        let e = raster.extent();
        assert!((e.min_x - 10.0).abs() < 1e-6 && (e.max_y - 41.5).abs() < 1e-6, "{e:?}");
        assert_eq!(rl.file_name.as_deref(), Some("uploaded.tif"));
        assert!(rl.embedded.is_some() && rl.path.is_none());
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Where a command may not download itself (the browser), `layer.addUrl` leaves its request for
/// the front end: once per address, a few at a time (the next is refused, never one silently
/// dropped after it was promised), and taken once.
#[test]
fn url_requests_wait_for_the_front_end() {
    let mut s = Session::new(Project::default());
    let p = json!({"url": "https://x.org/a.geojson", "name": "A", "zoom": true});
    assert_eq!(s.request_url(&p), Ok(json!({"pending": true, "url": "https://x.org/a.geojson"})));
    assert!(s.request_url(&p).is_ok());
    assert!(s.request_url(&json!({"url": "https://x.org/b.geojson"})).is_ok());
    let taken = s.take_url_requests();
    assert_eq!(taken.len(), 2, "the same address once");
    assert_eq!((taken[0]["name"].as_str(), taken[0]["zoom"].as_bool()), (Some("A"), Some(true)));
    assert_eq!(taken[1]["url"], "https://x.org/b.geojson");
    assert!(s.take_url_requests().is_empty());
    for i in 0..16 {
        assert!(s.request_url(&json!({"url": format!("https://x.org/{i}")})).is_ok(), "{i}");
    }
    let refused = s.request_url(&json!({"url": "https://x.org/16"})).unwrap_err();
    assert!(refused.contains("16 downloads are already waiting"), "{refused}");
    assert!(s.request_url(&json!({"url": "https://x.org/3"})).is_ok(), "one already waiting is fine");
    let taken = s.take_url_requests();
    assert_eq!(taken.len(), 16, "a few at a time");
    assert_eq!((taken[0]["url"].as_str(), taken[15]["url"].as_str()), (Some("https://x.org/0"), Some("https://x.org/15")));
    assert!(s.request_url(&json!({"url": "https://x.org/16"})).is_ok(), "room again once taken");
}

/// A mutating command that only left a request for the front end (the browser's `layer.addUrl`)
/// leaves no undo step and nothing to save; one that changed the project still does, and so
/// does one that merely answers "pending" (a plugin's command can't claim it changed nothing).
#[test]
fn a_pending_answer_is_no_undo_step() {
    let mut s = Session::new(Project::default());
    let undo = s.undo.len();
    let p = json!({"url": "https://x.org/a.geojson"});
    let r = s.guarded("Add Data From URL", true, |s| s.request_url(&p));
    assert_eq!(r.unwrap()["pending"], true);
    assert_eq!(s.undo.len(), undo);
    assert!(!s.dirty);
    assert_eq!(s.take_url_requests().len(), 1);
    let r = s.guarded("Plugin Command", true, |_| Ok(json!({"pending": true})));
    assert!(r.is_ok());
    assert_eq!(s.undo.len(), undo + 1, "an answer alone doesn't skip the step");
    assert!(s.dirty);
    assert!(s.run("map.new", &json!({"name": "B"})).is_ok());
    assert_eq!(s.undo.len(), undo + 2);
    // A command that changed the project and then, through a command of its own, left a
    // request (a plugin's) gets its step: the inner run takes the flag, the outer never sees it.
    let r = s.guarded("Plugin Command", true, |s| {
        s.run("map.new", &json!({"name": "C"}))?;
        s.guarded("Add Data From URL", true, |s| s.request_url(&json!({"url": "https://x.org/c.geojson"})))
    });
    assert_eq!(r.unwrap()["pending"], true);
    assert_eq!(s.undo.len(), undo + 4, "the map's step and the plugin's");
    assert_eq!(s.take_url_requests().len(), 1);
}

/// A command that left a request for the front end and then failed takes the request back with
/// its other changes: nothing is downloaded for a command that reported failure.
#[test]
fn a_failed_command_takes_its_request_back() {
    let mut s = Session::new(Project::default());
    assert!(s.request_url(&json!({"url": "https://x.org/kept.geojson"})).is_ok());
    let r = s.guarded("Plugin Command", true, |s| {
        s.guarded("Add Data From URL", true, |s| s.request_url(&json!({"url": "https://x.org/dropped.geojson"})))?;
        Err("the plugin failed afterwards".to_string())
    });
    assert!(r.is_err());
    let left = s.take_url_requests();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["url"], "https://x.org/kept.geojson");
}

/// The app's downloads land in the project they were started for, as bytes (no base64); one
/// whose project was closed meanwhile is dropped.
#[test]
fn downloads_land_in_their_own_project() {
    let mut s = Session::new(Project::default());
    let id = s.download_started();
    assert!(s.download_landed(id, br#"{"type": "Point", "coordinates": [1, 2]}"#.to_vec()));
    let r = s.run("layer.addDownload", &json!({"id": id, "url": "https://x.org/api?f=json", "name": "Api"})).unwrap();
    assert_eq!(r["features"], 1);
    assert!(s.run("layer.addDownload", &json!({"id": id, "url": "https://x.org/a.geojson"})).is_err(), "used once");
    let late = s.download_started();
    s.run("project.new", &json!({})).unwrap();
    assert!(!s.download_landed(late, b"{}".to_vec()), "another project is open now");
    let failed = s.download_started();
    s.download_failed(failed);
    assert!(!s.download_landed(failed, b"{}".to_vec()));
    // A link's first file zooms the map to it, whatever the map held.
    let mut w = world();
    let id = w.download_started();
    assert!(w.download_landed(id, br#"{"type": "Point", "coordinates": [151.2, -33.9]}"#.to_vec()));
    w.run("layer.addDownload", &json!({"id": id, "url": "https://x.org/sydney.geojson", "zoom": true})).unwrap();
    let c = rustgis_geom::merc_to_lonlat(w.map().camera.center);
    assert!((c.x - 151.2).abs() < 0.5 && (c.y + 33.9).abs() < 0.5, "{c:?}");
    // A file of several layers (a GPX's waypoints and track): the map frames them all.
    let gpx = br#"<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1"><wpt lat="10" lon="10"/><trk><trkseg><trkpt lat="20" lon="30"/><trkpt lat="21" lon="31"/></trkseg></trk></gpx>"#;
    let id = w.download_started();
    assert!(w.download_landed(id, gpx.to_vec()));
    let r = w.run("layer.addDownload", &json!({"id": id, "url": "https://x.org/trip.gpx", "zoom": true})).unwrap();
    assert_eq!(r["zoomed"], true, "{r}");
    let c = rustgis_geom::merc_to_lonlat(w.map().camera.center);
    assert!(c.x > 15.0 && c.x < 26.0 && c.y > 10.0 && c.y < 21.0, "{c:?}");
    // Without "zoom" the map stays where it is.
    let id = w.download_started();
    assert!(w.download_landed(id, br#"{"type": "Point", "coordinates": [-70, -40]}"#.to_vec()));
    let r = w.run("layer.addDownload", &json!({"id": id, "url": "https://x.org/far.geojson"})).unwrap();
    assert!(r.get("zoomed").is_none());
    assert_eq!(rustgis_geom::merc_to_lonlat(w.map().camera.center), c);
}

/// Spreadsheets, GeoRSS, LandXML and encoded polylines through the commands.
#[test]
fn spreadsheets_feeds_landxml_and_polylines() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-more-formats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::default());
    // A workbook: the first sheet by default, another by "layer", with a coordinate system.
    let book = rustgis_data::sheets::xlsx(&[
        ("Places", &[&["name", "longitude", "latitude"], &["A", "10", "20"]]),
        ("Survey", &[&["id", "E", "N"], &["1", "992000", "220000"]]),
    ]);
    let path = dir.join("book.xlsx");
    std::fs::write(&path, &book).unwrap();
    let p = path.display().to_string();
    assert_eq!(s.run("info.fileLayers", &json!({"path": p})).unwrap()["layers"], json!(["Places", "Survey"]));
    let r = s.run("layer.addData", &json!({"path": p})).unwrap();
    assert_eq!(r["features"], 1);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&book);
    let r = s
        .run(
            "layer.addDataBytes",
            &json!({"fileName": "book.xlsx", "data": b64, "layer": "survey", "x": "E", "y": "N", "crs": "EPSG:2263"}),
        )
        .unwrap();
    let e = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().extent();
    assert!((e.min_x + 73.96).abs() < 0.05, "{e:?}");
    assert!(s.run("layer.addData", &json!({"path": p, "layer": "nope"})).is_err());
    // Its columns, and a delimited file's, for Add Delimited Text.
    let c = s.run("info.fileColumns", &json!({"path": p, "layer": "Survey"})).unwrap();
    assert_eq!(c, json!({"columns": ["id", "E", "N"], "x": null, "y": null, "wkt": null, "delimiter": null}));
    let csv = dir.join("stops.csv");
    std::fs::write(&csv, "stop;Lon;Lat\nA;1;2\n").unwrap();
    let csv = csv.display().to_string();
    let c = s.run("info.fileColumns", &json!({"path": csv})).unwrap();
    assert_eq!(c, json!({"columns": ["stop", "Lon", "Lat"], "x": "Lon", "y": "Lat", "wkt": null, "delimiter": ";"}));
    let c = s.run("info.fileColumns", &json!({"path": csv, "delimiter": ","})).unwrap();
    assert_eq!(c["columns"], json!(["stop;Lon;Lat"]));
    assert!(s.run("info.fileColumns", &json!({"path": csv, "delimiter": ";;"})).is_err());
    assert!(s.run("info.fileColumns", &json!({"path": dir.join("missing.csv").display().to_string()})).is_err());
    // Only the start of delimited text is read: a character cut at its end isn't an error, bytes
    // that aren't UTF-8 are.
    let cut = crate::cmd::layer::columns_of("t.csv", b"lon,lat,name\n1,2,\xc3", &json!({})).unwrap();
    assert_eq!(cut["x"], "lon");
    assert!(crate::cmd::layer::columns_of("t.csv", b"lon,lat\n\xff,2\n", &json!({})).is_err());
    // A GeoRSS feed as plain .xml, by its contents.
    let feed = dir.join("quakes.xml");
    std::fs::write(&feed, r#"<rss xmlns:georss="http://www.georss.org/georss"><channel><item><title>Q</title><georss:point>45 -110</georss:point></item></channel></rss>"#).unwrap();
    s.run("layer.addData", &json!({"path": feed.display().to_string()})).unwrap();
    assert!(s.map().layer_by_name("quakes points").is_some());
    // LandXML without a coordinate system, given one.
    let lx = dir.join("site.landxml");
    std::fs::write(&lx, r#"<LandXML><CgPoints><CgPoint name="1">4649776 500000</CgPoint></CgPoints></LandXML>"#).unwrap();
    s.run("layer.addData", &json!({"path": lx.display().to_string(), "crs": "EPSG:32633"})).unwrap();
    let e = s.map().layer_by_name("site points").unwrap().extent();
    assert!((e.min_x - 15.0).abs() < 1e-6, "{e:?}");
    // Encoded polylines.
    let r = s
        .run("layer.addEncodedPolyline", &json!({"polyline": ["_p~iF~ps|U_ulLnnqC", "_p~iF~ps|U_ulLnnqC_mqNvxq`@"], "name": "Routes"}))
        .unwrap();
    assert_eq!(r["features"], 2);
    for bad in [json!({}), json!({"polyline": "_p~iF"}), json!({"polyline": "_p~iF~ps|U"}), json!({"polyline": "x", "precision": 99})] {
        assert!(s.run("layer.addEncodedPolyline", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A raster GDAL reads, added with a coordinate system: the project names the file and the
/// choice, and opens it again after the conversion cache is cleared. (Runs where GDAL is.)
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn gdal_rasters_reopen_with_their_choices() {
    // Found as the app finds it (the PATH, or RUSTGIS_GDAL_BIN).
    let Some(translate) = rustgis_data::source::gdal::program("gdal_translate") else { return };
    let dir = std::env::temp_dir().join(format!("rustgis-gdal-reopen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let asc = dir.join("g.asc");
    // UTM 33N coordinates and no .prj: the file doesn't say its system.
    std::fs::write(&asc, "ncols 2\nnrows 2\nxllcorner 500000\nyllcorner 4649776\ncellsize 100\n1 2\n3 4\n").unwrap();
    let img = dir.join("g.img");
    if !std::process::Command::new(&translate).args(["-q", "-of", "HFA"]).arg(&asc).arg(&img).status().is_ok_and(|s| s.success()) {
        return;
    }
    let mut s = Session::new(Project::default());
    let p = img.display().to_string();
    assert!(s.run("layer.addData", &json!({"path": p})).is_err(), "no coordinate system, not lon/lat");
    s.run("layer.addData", &json!({"path": p, "crs": "EPSG:32633"})).unwrap();
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("\"crs\":\"EPSG:32633\""), "the choice is saved");
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let l = t.map().layers.iter().find(|l| l.is_raster()).unwrap();
    assert!(l.raster.as_ref().is_some_and(|r| r.data.is_some()), "read again from the file");
    let _ = std::fs::remove_dir_all(dir);
}

/// GML, MapInfo MIF/MID and TopoJSON through the commands, by path, text and zip.
#[test]
fn gml_mapinfo_and_topojson() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::default());
    let gml = r#"<ogr:FeatureCollection xmlns:ogr="http://ogr.maptools.org/" xmlns:gml="http://www.opengis.net/gml">
<gml:featureMember><ogr:towns><ogr:geometryProperty><gml:Point srsName="EPSG:4326"><gml:coordinates>10,50</gml:coordinates></gml:Point></ogr:geometryProperty><ogr:name>A</ogr:name></ogr:towns></gml:featureMember>
</ogr:FeatureCollection>"#;
    s.run("layer.addDataText", &json!({"fileName": "towns.gml", "text": gml})).unwrap();
    assert_eq!(s.map().layer_by_name("towns").map(|l| l.dataset.features.len()), Some(1));
    // A MIF beside its MID, by path.
    let mif = "Version 300\nDelimiter \",\"\nCoordSys Earth Projection 1, 104\nColumns 1\n  name Char(10)\nData\nPoint 1 2\nPoint 3 4\n";
    std::fs::write(dir.join("sites.mif"), mif).unwrap();
    std::fs::write(dir.join("sites.mid"), "\"X\"\n\"Y\"\n").unwrap();
    s.run("layer.addData", &json!({"path": dir.join("sites.mif").display().to_string()})).unwrap();
    let l = s.map().layer_by_name("sites").unwrap();
    assert_eq!(l.dataset.features[1].attrs[0], rustgis_data::Value::Text("Y".into()));
    // Zipped together, uploaded.
    let zip =
        rustgis_data::zip::write(&[("a/sites2.mif".into(), mif.as_bytes().to_vec()), ("a/sites2.mid".into(), b"\"P\"\n\"Q\"\n".to_vec())])
            .unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(zip);
    s.run("layer.addDataBytes", &json!({"fileName": "mif.zip", "data": b64})).unwrap();
    assert_eq!(s.map().layer_by_name("sites2").unwrap().dataset.features[0].attrs[0], rustgis_data::Value::Text("P".into()));
    // TopoJSON, as .topojson and as .json.
    let topo = r#"{"type":"Topology","objects":{"pts":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[5,6],"properties":{"n":1}}]}},"arcs":[]}"#;
    s.run("layer.addDataText", &json!({"fileName": "a.topojson", "text": topo})).unwrap();
    s.run("layer.addDataText", &json!({"fileName": "b.json", "text": topo})).unwrap();
    assert!(s.map().layer_by_name("a").is_some() && s.map().layer_by_name("b").is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// Fields of the World's fields and density are two looks of one archive and stay side by side;
/// the fields again at another score threshold replace the earlier ones.
#[test]
fn fields_of_the_world_looks() {
    let mut s = Session::new(Project::default());
    s.run("fieldsOfTheWorld.add", &json!({"year": 2024, "draw": "fields"})).unwrap();
    s.run("fieldsOfTheWorld.add", &json!({"year": 2024, "draw": "density"})).unwrap();
    s.run("fieldsOfTheWorld.add", &json!({"year": 2024, "draw": "fields", "minScore": 50})).unwrap();
    let names: Vec<&str> = s.map().tiles.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Fields of the World 2024 fields", "Fields of the World 2024 density"]);
    assert!(s.map().tiles[0].style_layers.contains("50"));
    // Both replaces the parts (not another overlay of the archive); a part then replaces both.
    let url = crate::cmd::webdata::fields_url(2024);
    s.run("map.addTiles", &json!({"url": url, "kind": "pmtiles", "name": "My fields", "overlay": true})).unwrap();
    s.run("fieldsOfTheWorld.add", &json!({"year": 2024})).unwrap();
    assert_eq!(s.map().tiles.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Fields of the World 2024", "My fields"]);
    s.run("map.removeTiles", &json!({"name": "My fields"})).unwrap();
    s.run("fieldsOfTheWorld.add", &json!({"year": 2024, "draw": "density"})).unwrap();
    assert_eq!(s.map().tiles.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Fields of the World 2024 density"]);
}

/// A time-enabled WMTS layer (NASA GIBS's) is added at the date asked for, or its default; an
/// ArcGIS services directory lists its folders and services to open, which aren't added.
#[test]
fn service_times_and_directories() {
    const WMTS: &str = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>
<Layer><ows:Title>True Color</ows:Title><ows:Identifier>TC</ows:Identifier>
 <Dimension><ows:Identifier>Time</ows:Identifier><Default>2024-06-02</Default><Value>2000-02-24/2024-06-02/P1D</Value></Dimension>
 <TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet></TileMatrixSetLink>
 <ResourceURL resourceType="tile" format="image/jpeg" template="https://gibs.example.org/TC/default/{Time}/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.jpeg"/></Layer>
<TileMatrixSet><ows:Identifier>gm</ows:Identifier><ows:SupportedCRS>EPSG:3857</ows:SupportedCRS>
 <TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
 <TileMatrix><ows:Identifier>1</ows:Identifier><ScaleDenominator>279541132.0143589</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
</TileMatrixSet></Contents></Capabilities>"#;
    let mut s = Session::new(Project::default());
    let url = "https://gibs.example.org/wmts/1.0.0/WMTSCapabilities.xml";
    let v = s.run("service.layers", &json!({"url": url, "text": WMTS})).unwrap();
    assert_eq!(v["layers"][0]["time"]["default"], "2024-06-02");
    s.run("service.add", &json!({"url": url, "layer": "TC", "text": WMTS})).unwrap();
    s.run("service.add", &json!({"url": url, "layer": "TC", "text": WMTS, "time": "2020-01-05"})).unwrap();
    let urls: Vec<(&str, &str)> = s.map().tiles.iter().map(|t| (t.name.as_str(), t.url.as_str())).collect();
    assert_eq!(
        urls,
        [
            ("True Color 2020-01-05", "https://gibs.example.org/TC/default/2020-01-05/gm/{z}/{y}/{x}.jpeg"),
            ("True Color", "https://gibs.example.org/TC/default/2024-06-02/gm/{z}/{y}/{x}.jpeg")
        ]
    );
    assert!(s.run("service.add", &json!({"url": url, "layer": "TC", "text": WMTS, "time": "../../etc"})).is_err());
    // A directory: its folders and services, each opened by its own address.
    let root = "https://x.example.org/arcgis/rest/services";
    let dir = r#"{"currentVersion": 11, "folders": ["Hydro"], "services": [{"name": "Topo", "type": "MapServer"}]}"#;
    let v = s.run("service.layers", &json!({"url": root, "text": dir})).unwrap();
    assert_eq!(v["kind"], "ArcGIS Services Directory");
    assert_eq!(v["layers"][1]["offer"], json!({"type": "service", "url": "https://x.example.org/arcgis/rest/services/Topo/MapServer"}));
    let e = s.run("service.add", &json!({"url": root, "text": dir, "layer": "Topo/MapServer"})).unwrap_err();
    assert!(e.contains("service.layers"), "{e}");
}

/// Services from their capabilities: a WMS layer becomes a tile layer, an OGC API collection's
/// features are downloaded (from a local server); unknown layers and documents are refused.
#[test]
fn services_list_and_add_layers() {
    const WMS: &str = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Test WMS</Title></Service>
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.com/wms?"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><Title>Root</Title><CRS>EPSG:3857</CRS>
<Layer queryable="1"><Name>roads</Name><Title>Roads</Title><EX_GeographicBoundingBox><westBoundLongitude>5</westBoundLongitude><eastBoundLongitude>15</eastBoundLongitude><southBoundLatitude>45</southBoundLatitude><northBoundLatitude>55</northBoundLatitude></EX_GeographicBoundingBox></Layer>
</Layer></Capability></WMS_Capabilities>"#;
    let mut s = Session::new(Project::default());
    let url = "https://maps.example.com/wms";
    let v = s.run("service.layers", &json!({"url": url, "text": WMS})).unwrap();
    assert_eq!((v["kind"].as_str(), v["count"].as_u64()), (Some("WMS"), Some(1)));
    assert_eq!(v["layers"][0]["offer"]["type"], "tiles");
    s.run("service.add", &json!({"url": url, "layer": "Roads", "text": WMS})).unwrap();
    let t = s.map().tiles.first().unwrap();
    assert!(t.url.contains("LAYERS=roads") && t.url.contains("{bbox-epsg-3857}") && t.overlay, "{}", t.url);
    // Returning to a service layer's extent still works after saving and reopening the project.
    let project = serde_json::to_string(&s.project).unwrap();
    let mut reopened = Session::new(serde_json::from_str(&project).unwrap());
    reopened.run("map.goToXY", &json!({"lon": -100, "lat": 0, "scale": 1000000})).unwrap();
    let r = reopened.run("map.zoomToTiles", &json!({"name": "Roads"})).unwrap();
    assert_eq!(r["bounds"], json!([5.0, 45.0, 15.0, 55.0]));
    let center = reopened.map().center_lonlat();
    assert!((center.x - 10.0).abs() < 0.1 && (center.y - 50.0).abs() < 0.5, "{center:?}");
    assert!(s.run("service.add", &json!({"url": url, "layer": "nope", "text": WMS})).is_err());
    assert!(s.run("service.layers", &json!({"url": url, "text": "<html>no</html>"})).is_err());
    assert!(s.run("service.layers", &json!({"url": "file:///etc/x", "text": WMS})).is_err());
    // An OGC API collection's features, downloaded from a local server.
    const GEOJSON: &[u8] = br#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"name": "Lake"}, "geometry": {"type": "Point", "coordinates": [10, 50]}}]}"#;
    let base = serve(GEOJSON, 1);
    let api = format!("{base}/collections");
    let collections = format!(
        r#"{{"collections": [{{"id": "lakes", "title": "Lakes", "links": [{{"rel": "items", "type": "application/geo+json", "href": "{base}/collections/lakes/items"}}]}}]}}"#
    );
    let r = s.run("service.add", &json!({"url": api, "layer": "lakes", "text": collections})).unwrap();
    assert_eq!(r["features"], 1);
    assert!(s.map().layer_by_name("Lakes").is_some());
    // A document on the web can't send RustGIS into the local network.
    let inward = WMS.replace("https://maps.example.com/wms?", "http://127.0.0.1:8080/wms?");
    let v = s.run("service.layers", &json!({"url": url, "text": inward})).unwrap();
    assert_eq!(v["count"], 0);
    assert!(v["notes"].as_array().unwrap().iter().any(|n| n.as_str().is_some_and(|n| n.contains("won't fetch"))), "{v}");
    assert!(s.run("service.add", &json!({"url": url, "layer": "roads", "text": inward})).is_err());
    // A service's answer with latitude first (its crs says so) is turned round, and a page of a
    // larger collection is pointed out.
    const LAT_FIRST: &[u8] = br#"{"type": "FeatureCollection", "numberMatched": 9, "numberReturned": 1, "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [50, 10]}}], "crs": {"type": "name", "properties": {"name": "urn:ogc:def:crs:EPSG::4326"}}}"#;
    let base = serve(LAT_FIRST, 1);
    let collections = format!(
        r#"{{"collections": [{{"id": "wells", "title": "Wells", "links": [{{"rel": "items", "href": "{base}/collections/wells/items"}}]}}]}}"#
    );
    s.run("service.add", &json!({"url": format!("{base}/collections"), "layer": "wells", "text": collections})).unwrap();
    let e = s.map().layer_by_name("Wells").unwrap().extent();
    assert!((e.min_x - 10.0).abs() < 1e-9 && (e.min_y - 50.0).abs() < 1e-9, "{e:?}");
    assert!(s.notices.iter().any(|n| n.text.contains("1 of its 9 features")));
    // An OGC API layer added with its tileset's advertised template (the app reads it first); a
    // given template that isn't one, or points inward, is refused.
    let tiled = r#"{"collections": [{"id": "relief", "links": [{"rel": "tilesets-map", "href": "https://x.org/relief/map/tiles"}]}]}"#;
    let api = "https://x.org/collections";
    let r = s
        .run(
            "service.add",
            &json!({"url": api, "layer": "relief (map tiles)", "text": tiled, "tiles": "https://x.org/relief/map/tiles/smerc/{z}/{y}/{x}.png"}),
        )
        .unwrap();
    assert!(s.map().tiles.iter().any(|t| t.url == "https://x.org/relief/map/tiles/smerc/{z}/{y}/{x}.png"), "{r}");
    for bad in ["https://x.org/relief.png", "http://169.254.169.254/{z}/{x}/{y}", "http://127.0.0.1/{z}/{x}/{y}"] {
        assert!(s.run("service.add", &json!({"url": api, "layer": "relief (map tiles)", "text": tiled, "tiles": bad})).is_err(), "{bad}");
    }
}

/// An OGC API collection that comes in pages is read page by page, and a `next` link is followed
/// only where the page may point.
#[test]
fn ogc_api_features_follow_next_pages() {
    let page = |ids: &[u32], next: Option<&str>| {
        let features: Vec<Value> = ids
            .iter()
            .map(|&i| json!({"type": "Feature", "properties": {"n": i}, "geometry": {"type": "Point", "coordinates": [i, 1]}}))
            .collect();
        let mut p = json!({"type": "FeatureCollection", "numberMatched": 5, "numberReturned": ids.len(), "features": features});
        if let Some(h) = next {
            p["links"] = json!([{"rel": "next", "type": "application/geo+json", "href": h}]);
        }
        p.to_string()
    };
    let base = serve_routes(
        vec![
            ("/collections/wells/items?page=2", page(&[3, 4], Some("{base}/collections/wells/items?page=3"))),
            ("/collections/wells/items?page=3", page(&[5], None)),
            ("/collections/wells/items", page(&[1, 2], Some("items?page=2"))),
            ("/collections/inward/items", page(&[1, 2], Some("http://169.254.169.254/latest/meta-data/"))),
        ],
        4,
    );
    let collections = format!(
        r#"{{"collections": [{{"id": "wells", "title": "Wells", "links": [{{"rel": "items", "href": "{base}/collections/wells/items"}}]}},
        {{"id": "inward", "title": "Inward", "links": [{{"rel": "items", "href": "{base}/collections/inward/items"}}]}}]}}"#
    );
    let mut s = Session::new(Project::default());
    let api = format!("{base}/collections");
    let r = s.run("service.add", &json!({"url": api, "layer": "wells", "text": collections})).unwrap();
    assert_eq!(r["features"], 5);
    assert!(!s.notices.iter().any(|n| n.text.contains("aren't read")), "{:?}", s.notices.iter().map(|n| &n.text).collect::<Vec<_>>());
    let r = s.run("service.add", &json!({"url": api, "layer": "inward", "text": collections})).unwrap();
    assert_eq!(r["features"], 2);
    assert!(s.notices.iter().any(|n| n.text.starts_with("Inward: ") && n.text.contains("never fetched")));
}

/// A WCS coverage (from a local server) becomes a raster of the view, asked for in EPSG:4326 within
/// its bounds; an error answer is refused. A catalogue's records come with the links RustGIS can
/// follow, those pointing inward left out.
#[test]
fn wcs_coverages_and_catalogue_records() {
    let r = rustgis_data::raster::Raster::new(
        "soc",
        8,
        8,
        vec![vec![3.0; 64]],
        rustgis_data::raster::GeoTransform { origin_x: 5.0, origin_y: 50.0, pixel_width: 0.1, pixel_height: -0.1 },
        rustgis_data::raster::Crs::LonLat,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let (base, _) = serve_file(rustgis_data::raster::geotiff::write(&r, 0).unwrap(), false);
    let caps = String::from(
        r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
        <ows:ServiceIdentification><ows:Title>Soils</ows:Title></ows:ServiceIdentification>
        <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Soil carbon</ows:Title>
          <ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
        </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#,
    );
    let describe = r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
        <lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
    let url = format!("{base}/wcs");
    let mut s = Session::new(Project::default());
    let v = s.run("service.layers", &json!({"url": url, "text": caps})).unwrap();
    assert_eq!((v["kind"].as_str(), v["layers"][0]["offer"]["type"].as_str()), (Some("WCS"), Some("coverage")), "{v}");
    let p = json!({"url": url, "layer": "soc", "text": caps, "describe": describe, "bbox": [0, 40, 5.4, 60]});
    let mut selected = p.clone();
    selected["source_url"] = v["layers"][0]["source_url"].clone();
    assert!(s.run("service.coverage", &selected).is_ok());
    selected["source_url"] = json!("https://example.org/another-source");
    assert!(s.run("service.coverage", &selected).is_err());
    let c = s.run("service.coverage", &p).unwrap();
    let get = c["url"].as_str().unwrap();
    assert!(get.contains("SUBSET=Long(5,5.4)&SUBSET=Lat(49.2,50)&SCALESIZE=Long(1024),Lat(2048)"), "{get}");
    assert_eq!(c["bbox"], json!([5.0, 49.2, 5.4, 50.0]));
    let r = s.run("service.add", &p).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).is_some_and(|l| l.is_raster() && l.name == "Soil carbon"), "{r}");
    assert!(
        s.run("service.coverage", &json!({"url": url, "layer": "soc", "text": caps, "describe": describe, "bbox": [10, 10, 11, 11]}))
            .is_err()
    );
    let full = s.run("service.coverage", &json!({"url": url, "layer": "soc", "text": caps, "describe": describe, "bbox": "full"})).unwrap();
    assert_eq!(full["bbox"], json!([5.0, 49.2, 5.8, 50.0]));
    // The same coverage browsed from a saved connection: the add it lists runs as it is.
    s.run("connection.new", &json!({"kind": "wcs", "address": url, "name": "Soils"})).unwrap();
    let browsed = s.run("connection.browse", &json!({"id": "Soils", "answer": caps})).unwrap();
    let child = &browsed["children"][0];
    assert_eq!((child["what"].as_str(), child["add"]["command"].as_str()), (Some("coverage"), Some("service.add")), "{browsed}");
    let mut listed = child["add"]["params"].clone();
    listed["text"] = json!(caps);
    listed["describe"] = json!(describe);
    listed["bbox"] = json!("full");
    let layers = s.map().layers.len();
    s.run("service.add", &listed).unwrap();
    assert_eq!(s.map().layers.len(), layers + 1);
    let (err, _) = serve_file(b"<ows:ExceptionReport>no</ows:ExceptionReport>".to_vec(), false);
    let p = json!({"url": format!("{err}/wcs"), "layer": "soc", "text": caps, "describe": describe, "bbox": "full"});
    assert!(s.run("service.add", &p).unwrap_err().contains("error"));

    let records = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/">
        <csw:SearchResults numberOfRecordsMatched="3" nextRecord="2"><csw:Record><dc:title>Forests</dc:title>
          <dc:URI protocol="OGC:WMS">https://geo.example.org/wms</dc:URI><dc:URI protocol="OGC:WFS">http://192.168.1.2/wfs</dc:URI>
        </csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#;
    let v = s.run("catalog.search", &json!({"url": "https://cat.example.org/csw", "query": "forest", "text": records})).unwrap();
    assert_eq!((v["matched"].as_u64(), v["count"].as_u64(), v["next"].as_u64()), (Some(3), Some(1), Some(2)), "{v}");
    assert_eq!(
        v["records"][0]["links"],
        json!([{"url": "https://geo.example.org/wms", "kind": "wms", "name": null, "protocol": "OGC:WMS"}])
    );
    assert!(v["notes"][0].as_str().is_some_and(|n| n.contains("1 links")), "{v}");
    assert!(v["request"].as_str().is_some_and(|r| r.contains("AnyText%20LIKE%20%27%25forest%25%27")), "{v}");
    assert!(s.run("catalog.search", &json!({"url": "https://cat.example.org/csw", "text": "<html/>"})).is_err());
}

/// A KMZ's ground overlays become raster layers kept in the project; a KML whose picture is a web
/// address or outside its folder says so.
#[test]
fn kml_ground_overlays_become_rasters() {
    use base64::Engine;
    let mut red = tiny_skia::Pixmap::new(8, 8).unwrap();
    red.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
    let doc = r#"<kml xmlns="http://www.opengis.net/kml/2.2"><Document>
<Placemark><name>P</name><Point><coordinates>10.5,40.5</coordinates></Point></Placemark>
<GroundOverlay><name>Red</name><Icon><href>files/red.png</href></Icon><LatLonBox><north>41</north><south>40</south><east>11</east><west>10</west><rotation>10</rotation></LatLonBox></GroundOverlay>
<GroundOverlay><name>Quad</name><Icon><href>files/red.png</href></Icon></GroundOverlay>
</Document></kml>"#;
    let kmz = rustgis_data::zip::write(&[("doc.kml".into(), doc.as_bytes().to_vec()), ("files/red.png".into(), red.encode_png().unwrap())])
        .unwrap();
    let mut s = Session::new(Project::default());
    let b64 = base64::engine::general_purpose::STANDARD.encode(&kmz);
    let r = s.run("layer.addDataBytes", &json!({"fileName": "site.kmz", "data": b64})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(2), "{r}");
    let overlay = s.map().layer_by_name("Red").unwrap();
    assert!(overlay.raster.as_ref().is_some_and(|r| r.data.is_some()));
    assert!(s.notices.iter().any(|n| n.text.contains("1 ground overlays without a picture or a box")));
    // The project keeps the picture.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert!(t.map().layer_by_name("Red").and_then(|l| l.raster.as_ref()).is_some_and(|r| r.data.is_some()));
    // A KML of overlays only, whose pictures aren't readable from where it is.
    let dir = std::env::temp_dir().join(format!("rustgis-overlay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let only = r#"<kml><GroundOverlay><name>Far</name><Icon><href>../secret.png</href></Icon><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay>
<GroundOverlay><name>Web</name><Icon><href>https://example.com/x.png</href></Icon><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay>
<GroundOverlay><name>Here</name><Icon><href>red.png</href></Icon><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay></kml>"#;
    std::fs::write(dir.join("o.kml"), only).unwrap();
    std::fs::write(dir.join("red.png"), red.encode_png().unwrap()).unwrap();
    let mut u = Session::new(Project::default());
    u.run("layer.addData", &json!({"path": dir.join("o.kml").display().to_string()})).unwrap();
    assert!(u.map().layer_by_name("Here").is_some());
    assert!(u.map().layer_by_name("Far").is_none() && u.map().layer_by_name("Web").is_none());
    assert!(u.notices.iter().any(|n| n.text.contains("Far (its picture ../secret.png isn't in the file or beside it)")));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pmtiles_from_network_shares_are_refused() {
    let mut s = Session::new(Project::default());
    for p in [r"\\attacker\share\x.pmtiles", "//attacker/share/x.pmtiles", "file://attacker/share/x.pmtiles"] {
        assert!(s.run("map.addTiles", &json!({"url": p, "kind": "pmtiles"})).is_err(), "{p}");
    }
    assert!(crate::render::rustgis_tiles::pmtiles::check_local(r"\\attacker\share\x.pmtiles").is_err());
    assert!(crate::render::rustgis_tiles::pmtiles::check_local("/data/x.pmtiles").is_ok());
}

/// A GeoPackage with more feature tables than are added at once says which were left out, in the
/// result and in a notice (issue #85); `info.fileLayers` lists them all.
#[test]
fn geopackage_table_cap_is_reported() {
    let dir = std::env::temp_dir().join(format!("rustgis-gpkg-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("many.gpkg");
    let _ = std::fs::remove_file(&path);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         INSERT INTO gpkg_spatial_ref_sys VALUES ('WGS 84', 4326, 'EPSG', 4326, '', '');
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);",
    )
    .unwrap();
    let mut blob = b"GP\x00\x01".to_vec();
    blob.extend_from_slice(&4326i32.to_le_bytes());
    blob.extend(rustgis_data::wkb::encode(&rustgis_data::Geometry::Point(rustgis_data::Coord::new(2.35, 48.85))));
    for i in 1..=34 {
        let t = format!("t_{i:02}");
        c.execute_batch(&format!(
            "INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('{t}', 'features', 4326);
             INSERT INTO gpkg_geometry_columns VALUES ('{t}', 'geom', 'POINT', 4326, 0, 0);
             CREATE TABLE {t} (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB);"
        ))
        .unwrap();
        c.execute(&format!("INSERT INTO {t} (geom) VALUES (?1)"), [&blob]).unwrap();
    }
    drop(c);
    let p = path.display().to_string();

    let mut s = Session::new(Project::default());
    let listed = s.run("info.fileLayers", &json!({"path": p})).unwrap();
    assert_eq!(listed["count"], 34);
    assert_eq!(listed["layers"][33], "t_34");

    let mark = s.notice_mark();
    let r = s.run("layer.addData", &json!({"path": p})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(32));
    assert_eq!(r["tables"], 34);
    assert_eq!(r["skipped"], json!(["t_33", "t_34"]));
    assert_eq!(r["skippedCount"], 2);
    let said = s.notices_since(mark);
    assert!(said.iter().any(|n| n.text.contains("first 32 of 34 tables")), "{said:?}");
    // A table past the cap is added by name.
    let one = s.run("layer.addData", &json!({"path": p, "layer": "t_34"})).unwrap();
    assert!(one.get("skipped").is_none());
    assert_eq!(s.map().layers.len(), 33);
    // The pure-Rust reader the web app lists an upload's tables with sees them all.
    let listed =
        rustgis_data::source::gpkg_mem::layers(rustgis_data::source::Shared(std::sync::Arc::new(std::fs::read(&path).unwrap()))).unwrap();
    assert_eq!(listed.len(), 34);
    assert!(s.run("info.fileLayers", &json!({"path": dir.join("none.geojson").display().to_string()})).is_err());
    let notes = dir.join("notes.rgis");
    std::fs::write(&notes, "{}").unwrap();
    assert!(s.run("info.fileLayers", &json!({"path": notes.display().to_string()})).is_err(), "not a data file");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn notices_since_survives_the_cap() {
    let mut s = Session::new(Project::default());
    for i in 0..250 {
        s.notify(format!("n{i}"), false);
    }
    let mark = s.notice_mark();
    assert!(s.notices_since(mark).is_empty());
    s.notify("last", false);
    assert_eq!(s.notices_since(mark).iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["last"]);
    assert_eq!(s.notices_since(0).len(), 200);
}

/// The web app's `?blank&projection=EPSG:3010` (discussion #81): an empty project starts in a
/// national grid, and that start is neither an edit to undo nor unsaved work.
#[test]
fn blank_project_starts_in_a_projected_crs() {
    isolate_plugins();
    let mut s = Session::new(Project::default());
    assert!(s.map().layers.is_empty());
    let r = s.run("map.projection", &json!({"type": "EPSG:3010"})).unwrap();
    assert_eq!(r["projection"], "EPSG:3010");
    assert!(s.dirty && s.can_undo());
    s.clear_history();
    assert!(!s.dirty && !s.can_undo());
    assert_eq!(s.map().projection.code(), "EPSG:3010");
    // It opens on the country, and reports that view back.
    s.run("map.zoomToExtent", &json!({"extent": [10.5, 55.0, 24.5, 69.5]})).unwrap();
    let e = s.run("map.extent", &Value::Null).unwrap()["extent"].clone();
    let n: Vec<f64> = e.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    assert!(n[0] < 11.0 && n[2] > 24.0 && n[1] < 55.5 && n[3] > 69.0 && n[3] - n[1] < 25.0, "{n:?}");
    for bad in [json!(null), json!([1, 2, 3]), json!([20, 50, 10, 60]), json!([0, 0, 200, 10]), json!(["a", 0, 1, 1])] {
        assert!(s.run("map.zoomToExtent", &json!({"extent": bad})).is_err(), "{bad}");
    }
    assert!(crate::cmd::share::map_png(&mut s, Some(400.0), Some(300.0), None).unwrap().len() > 1000);
}

#[test]
fn layers_export_to_geopackage_geoparquet_shapefile_and_kmz() {
    let dir = std::env::temp_dir().join(format!("rustgis-engine-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = world();
    let want = s.map().layers.iter().find(|l| l.name == "Countries").unwrap().dataset.features.len();
    for ext in ["gpkg", "parquet", "shp", "kmz", "zip"] {
        let path = dir.join(format!("countries.{ext}")).display().to_string();
        let r = s.run("share.exportLayer", &json!({"layer": "Countries", "path": path})).unwrap();
        assert_eq!((r["features"].as_u64(), r["leftOut"].as_u64()), (Some(want as u64), Some(0)), "{ext}");
        if matches!(ext, "kmz" | "zip") {
            assert!(std::fs::metadata(&path).unwrap().len() > 0);
            continue;
        }
        // Added back, drawn and saved in a project that reopens.
        let r = s.run("layer.addData", &json!({"path": path})).unwrap();
        let id = r["layer"].as_u64().unwrap() as u32;
        let l = s.map().layer(id).unwrap();
        let n = if l.is_streamed() { l.load_all().unwrap().features.len() } else { l.dataset.features.len() };
        assert_eq!(n, want, "{ext}");
    }
    let project = dir.join("p.rgis").display().to_string();
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    s.run("project.open", &json!({"path": project})).unwrap();
    s.run("share.exportMap", &json!({"path": dir.join("m.png").display().to_string()})).unwrap();
    // Not a format it writes.
    assert!(s.run("share.exportLayer", &json!({"layer": "Countries", "path": dir.join("x.dwg").display().to_string()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn layers_export_in_the_system_they_were_read_in() {
    // Issue #514: squares in ETRS89 / UTM zone 32N came out of every export in EPSG:4326.
    let dir = std::env::temp_dir().join(format!("rustgis-engine-export-crs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("squares.csv");
    std::fs::write(&csv, "side;wkt\n1 km;POLYGON ((500000 5700000, 501000 5700000, 501000 5701000, 500000 5701000, 500000 5700000))\n")
        .unwrap();
    let mut s = Session::new(Project::default());
    let add = json!({"path": csv.display().to_string(), "name": "squares", "wkt": "wkt", "crs": "EPSG:25832", "delimiter": ";"});
    s.run("layer.addData", &add).unwrap();
    let path = |name: &str| dir.join(name).display().to_string();
    for (file, crs, want) in [
        ("out.gpkg", None, "EPSG:25832"),
        ("out.shp", None, "EPSG:25832"),
        ("out.fgb", None, "EPSG:25832"),
        ("out4647.gpkg", Some("EPSG:4647"), "EPSG:4647"),
        ("out4326.shp", Some("EPSG:4326"), "EPSG:4326"),
        ("out.geojson", None, "EPSG:4326"),
    ] {
        let mut p = json!({"layer": "squares", "path": path(file)});
        if let Some(c) = crs {
            p["crs"] = json!(c);
        }
        let r = s.run("share.exportLayer", &p).unwrap();
        assert_eq!(r["crs"], want, "{file}");
        // Added back, it is in the same place and remembers its system.
        let r = s.run("layer.addData", &json!({"path": path(file)})).unwrap();
        let l = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
        let ds = if l.is_streamed() { l.load_all().unwrap() } else { l.dataset.clone() };
        assert_eq!(ds.crs.map(|c| c.label()).unwrap_or_else(|| "EPSG:4326".into()), want, "{file}");
        let e = ds.extent();
        assert!((e.min_x - 9.0).abs() < 1e-6 && (e.min_y - 51.451_18).abs() < 1e-5 && e.width() > 0.0139, "{file}: {e:?}");
    }
    // The .prj says the system, by its parameters and its code.
    let prj = std::fs::read_to_string(dir.join("out.prj")).unwrap();
    assert!(prj.contains("PARAMETER[\"central_meridian\",9]") && prj.contains("AUTHORITY[\"EPSG\",\"25832\"]"), "{prj}");
    // GeoJSON is longitude/latitude only; an unknown system is an error.
    assert!(s.run("share.exportLayer", &json!({"layer": "squares", "path": path("x.geojson"), "crs": "EPSG:25832"})).is_err());
    assert!(s.run("share.exportLayer", &json!({"layer": "squares", "path": path("x.gpkg"), "crs": "EPSG:1"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zipped_shapefiles_add_as_layers() {
    use base64::Engine;
    // The sample's countries and cities as Shapefiles in one zip, under a folder.
    let dir = std::env::temp_dir().join(format!("rustgis-engine-zip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut entries = Vec::new();
    for (name, text) in [("countries", crate::sample::countries()), ("cities", crate::sample::CITIES)] {
        let (ds, _) = rustgis_data::io::read_bytes(&format!("{name}.geojson"), text.as_bytes()).unwrap();
        let (zipped, _) = rustgis_data::io::write_bytes(&ds, "shp").unwrap();
        for (n, b) in rustgis_data::zip::read(&zipped).unwrap() {
            entries.push((format!("world/{n}"), b));
        }
    }
    let zip = rustgis_data::zip::write(&entries).unwrap();
    // Uploaded (the web app).
    let mut s = Session::new(Project::default());
    let data = base64::engine::general_purpose::STANDARD.encode(&zip);
    let r = s.run("layer.addDataBytes", &json!({"fileName": "world.zip", "data": data})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(2), "{r}");
    let names: Vec<&str> = s.map().layers.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"cities") && names.contains(&"countries"), "{names:?}");
    // From disk (the desktop app), saved and reopened with the project.
    let path = dir.join("world.zip");
    std::fs::write(&path, &zip).unwrap();
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": path.display().to_string(), "name": "Towns"})).unwrap();
    assert!(r["features"].as_u64().unwrap() > 10, "{r}");
    assert_eq!(s.map().layers.iter().filter(|l| l.name == "Towns").count(), 1);
    let project = dir.join("p.rgis").display().to_string();
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    s.run("project.open", &json!({"path": project})).unwrap();
    assert_eq!(s.map().layers.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Serves each request by the first route its path starts with (`{base}` in a body becomes the
/// server's address; other paths are 404); returns the base address.
fn serve_routes(routes: Vec<(&'static str, String)>, n: usize) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let b = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(n) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let read = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            match routes.iter().find(|(p, _)| path.starts_with(p)) {
                Some((_, body)) => {
                    let body = body.replace("{base}", &b);
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
                None => {
                    let _ = write!(s, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        }
    });
    base
}

/// A STAC API (a local one): its collections, a search adding the footprints, and an item's
/// asset added by its address; assets elsewhere than the web are refused by name.
#[test]
fn stac_search_and_assets() {
    let item = r#"{"type": "Feature", "id": "S2_1", "collection": "s2", "bbox": [7, 46, 8, 47],
        "geometry": {"type": "Polygon", "coordinates": [[[7, 46], [8, 46], [8, 47], [7, 47], [7, 46]]]},
        "properties": {"datetime": "2024-06-02T10:30:00Z", "eo:cloud_cover": 3.5},
        "links": [{"rel": "self", "href": "{base}/v1/collections/s2/items/S2_1"}],
        "assets": {"places": {"href": "{base}/data/places.geojson", "type": "application/geo+json", "roles": ["data"]},
                   "b04": {"href": "s3://bucket/B04.tif", "type": "image/tiff; application=geotiff", "roles": ["data"]}}}"#;
    let geojson = r#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"n": 1}, "geometry": {"type": "Point", "coordinates": [7.5, 46.5]}}]}"#;
    let base = serve_routes(
        vec![
            ("/v1/collections/s2/items/", item.to_string()),
            ("/v1/collections", r#"{"collections": [{"id": "s2", "title": "Sentinel-2"}]}"#.to_string()),
            (
                "/v1/search",
                format!(
                    r#"{{"type": "FeatureCollection", "features": [{item}], "links": [{{"rel": "next", "href": "{{base}}/v1/search?page=2"}}]}}"#
                ),
            ),
            ("/data/places.geojson", geojson.to_string()),
        ],
        6,
    );
    let api = format!("{base}/v1");
    let mut s = Session::new(Project::default());
    let r = s.run("stac.collections", &json!({"url": api})).unwrap();
    assert_eq!(r["collections"][0]["id"], "s2");
    let r = s.run("stac.search", &json!({"url": format!("{api}/search"), "collections": "s2", "bbox": [7, 46, 8, 47], "datetime": "2024-06-01/2024-06-30", "maxCloud": 20})).unwrap();
    assert_eq!(r["count"], 1);
    assert_eq!(r["next"], format!("{base}/v1/search?page=2"));
    let l = s.map().layer_by_name("STAC footprints").unwrap();
    assert_eq!(l.dataset.features.len(), 1);
    assert_eq!(l.dataset.geometry_type, rustgis_data::GeometryType::Polygon);
    // The item as the search gave it: its data asset (no visual one, no GeoTIFF on the web).
    let found = r["items"][0].clone();
    let e = s.run("stac.addAsset", &json!({"item": found, "asset": "b04"})).unwrap_err();
    assert!(e.contains("s3://"), "{e}");
    // Given whole, an item came from no address: its assets may only be public, and this one is
    // on the local network.
    let e = s.run("stac.addAsset", &json!({"item": found})).unwrap_err();
    assert!(e.contains("local network"), "{e}");
    // By the item's address (a server the user named): its local assets are fine.
    let r = s.run("stac.addAsset", &json!({"url": format!("{api}/collections/s2/items/S2_1"), "name": "Again"})).unwrap();
    assert_eq!(r["features"], 1, "{r}");
    assert!(s.map().layer_by_name("Again").is_some());
    for bad in [
        json!({"url": api, "bbox": [0, 50, 1, 40]}),
        json!({"url": api, "datetime": "last week"}),
        json!({"url": "file:///etc"}),
        json!({"item": 7}),
        json!({"item": {"type": "Feature", "id": "x", "assets": {}}}),
    ] {
        assert!(s.run("stac.search", &bad).is_err() || s.run("stac.addAsset", &bad).is_err(), "{bad}");
    }
    assert!(s.run("stac.addAsset", &json!({"item": {"type": "Feature", "id": "x", "assets": {}}})).is_err());
}

/// Directions through a routing server of one's own (a local Valhalla answer here): the route
/// and its steps as layers; the app's answer can be passed in instead, and nothing is fetched.
#[test]
fn directions_between_places() {
    const ANSWER: &str = r#"{"code":"Ok","routes":[{"duration":302.776,"distance":1921.553,"legs":[{"summary":"Pariser Platz","steps":[
        {"geometry":"g~idcBwjpoXl@InGo@","name":"Pariser Platz","distance":17.553,"duration":6.327,"maneuver":{"instruction":"Drive south on Pariser Platz.","type":"depart","location":[13.378236,52.51634]}},
        {"geometry":"itidcBqlpoXiD}mAYsKe@cS","name":"Pariser Platz","distance":1904.0,"duration":296.448,"maneuver":{"modifier":"left","instruction":"Turn left to stay on Pariser Platz.","type":"continue","location":[13.378265,52.516181]}},
        {"geometry":"ycqdcBurdqX??","name":"","distance":0.0,"duration":0.0,"maneuver":{"instruction":"You have arrived at your destination.","type":"arrive","location":[13.404987,52.520013]}}]}]}]}"#;
    let base = serve(ANSWER.as_bytes(), 1);
    let mut s = Session::new(Project::default());
    let r = s
        .run("map.route", &json!({"from": "52.5163, 13.3777", "to": [13.405, 52.52], "via": [{"lon": 13.39, "lat": 52.517}], "mode": "drive", "avoid": ["tolls"], "url": base}))
        .unwrap();
    assert_eq!(r["duration"], "5 min");
    assert_eq!(r["steps"].as_array().map(Vec::len), Some(3));
    assert_eq!(r["steps"][1]["modifier"], "left");
    let line = s.map().layer_by_name("Route (1.9 km)").unwrap();
    assert_eq!(line.dataset.geometry_type, rustgis_data::GeometryType::Polyline);
    let steps = s.map().layer_by_name("Route steps").unwrap();
    assert_eq!(steps.dataset.features.len(), 3);
    // The camera went to the route.
    let c = s.map().to_lonlat(s.map().camera.center).unwrap();
    assert!((c.x - 13.39).abs() < 0.05 && (c.y - 52.518).abs() < 0.05, "{c:?}");
    // The app's answer: nothing is fetched (the server above answered once only).
    let r = s.run("map.route", &json!({"answer": ANSWER, "mode": "walk", "zoom": false})).unwrap();
    assert!(r["layer"].is_u64());
    assert!(s.map().layer_by_name("Route (1.9 km) (2)").is_some());
    for bad in [
        json!({}),
        json!({"from": [13.0, 52.0]}),
        json!({"from": [200, 0], "to": [0, 0], "url": base}),
        json!({"from": [0, 0], "to": [1, 1], "mode": "teleport"}),
        json!({"from": [0, 0], "to": [1, 1], "avoid": ["mountains"]}),
        json!({"from": [0, 0], "to": [1, 1], "via": 7}),
        json!({"from": [0, 0], "to": [1, 1], "url": "file:///etc/passwd"}),
        json!({"answer": "<html>"}),
        json!({"answer": ANSWER, "route": 3}),
        json!({"answer": ANSWER, "route": u64::MAX}),
        // More names than one call looks up (refused before any is).
        json!({"from": "a", "to": "b", "via": ["c", "d", "e", "f"]}),
    ] {
        assert!(s.run("map.route", &bad).is_err(), "{bad}");
    }
}

/// Places found by name, and a table's addresses turned into points, through a geocoder of one's
/// own (a local Nominatim answer here); the app's answers can be passed in instead.
#[test]
fn geocoding_places_and_addresses() {
    use crate::render::rustgis_tiles::geocode::Geocoder;
    const ANSWER: &[u8] = br#"[{"lat": "48.8584", "lon": "2.2945", "display_name": "Tour Eiffel, Paris", "boundingbox": ["48.857", "48.859", "2.293", "2.296"]}]"#;
    let base = serve(ANSWER, 4);
    let mut s = Session::new(Project::default());
    let r = s.run("map.findPlace", &json!({"query": "Eiffel Tower", "url": base})).unwrap();
    assert_eq!(r["count"], 1);
    assert_eq!(r["places"][0]["name"], "Tour Eiffel, Paris");
    assert!(s.run("map.findPlace", &json!({"query": " "})).is_err());
    assert!(s.run("map.findPlace", &json!({"query": "x", "provider": "google"})).is_err());
    // Two rows share an address, asked once; a row without one has no point.
    let csv = "id,street,city\n1,Champ de Mars,Paris\n2,Champ de Mars,Paris\n3,5 Av. Anatole,Paris\n4,,\n";
    s.run("layer.addDataText", &json!({"fileName": "sites.csv", "text": csv})).unwrap();
    assert!(s.project.tables.iter().any(|t| t.name == "sites"), "a table of addresses");
    let r = s.run("layer.geocode", &json!({"table": "sites", "address": ["street", "city"], "url": base})).unwrap();
    assert_eq!((r["matched"].as_u64(), r["unmatched"].as_u64()), (Some(3), Some(1)), "{r}");
    let l = s.map().layer_by_name("sites (geocoded)").unwrap();
    assert_eq!(l.dataset.features.len(), 4);
    assert_eq!(l.dataset.fields.last().unwrap().name, "geocoded");
    assert!(l.dataset.features[3].geometry.is_none());
    // The app's own answers: one address found, the other not.
    let places = json!({"Champ de Mars, Paris": {"lon": 2.29, "lat": 48.85, "name": "Champ de Mars"}, "5 Av. Anatole, Paris": null});
    let r = s.run("layer.geocode", &json!({"layer": "sites", "address": "street, city", "places": places, "name": "G"})).unwrap();
    assert_eq!((r["matched"].as_u64(), r["unmatched"].as_u64()), (Some(2), Some(2)));
    for bad in
        [json!({"layer": "sites"}), json!({"layer": "sites", "address": "nope"}), json!({"layer": "sites", "address": "id", "places": 7})]
    {
        assert!(s.run("layer.geocode", &bad).is_err(), "{bad}");
    }
    // A server that refuses: the requests are failures, not "not found".
    let refusing = format!("{}/missing", serve(ANSWER, 2));
    let e = s.run("layer.geocode", &json!({"table": "sites", "address": ["street", "city"], "url": refusing})).unwrap_err();
    assert!(e.contains("none of the addresses"), "{e}");
    assert!(s.notices.iter().any(|n| n.error && n.text.contains("2 addresses couldn't be looked up")));
    // A public server takes a hundred addresses a run.
    let g = Geocoder::new(None, None).unwrap();
    assert!(crate::cmd::geocode::check_batch(&g, 101).unwrap_err().contains("100"));
}

/// The SQL workspace's commands: a query's first rows without a layer, and a database's columns
/// (through DuckDB when it is installed; without it, they say how to get it).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn sql_workspace_rows_and_schema() {
    let mut s = Session::new(Project::default());
    assert!(s.run("sql.query", &json!({})).is_err());
    if !rustgis_data::source::duckdb::available() {
        assert!(s.run("sql.query", &json!({"sql": "SELECT 1"})).unwrap_err().contains("need DuckDB"));
        return;
    }
    let before = s.map().layers.len();
    let r = s.run("sql.query", &json!({"sql": "SELECT i AS n, ST_Point(i, 0) AS geom FROM range(150) t(i)"})).unwrap();
    assert_eq!(r["rows"].as_array().map(Vec::len), Some(100), "the default limit");
    assert_eq!((r["geometry"].as_bool(), r["more"].as_bool()), (Some(true), Some(true)));
    assert_eq!(r["columns"][0]["name"], "n");
    assert_eq!(r["rows"][2][1], "POINT (2 0)");
    // A result the app ran is added only for the query it came from, and kept until it is.
    let q = "SELECT 'a' AS name, ST_Point(1, 2) AS geom";
    let out = rustgis_data::source::duckdb::run(&rustgis_data::source::duckdb::Query { sql: q, ..Default::default() }).unwrap();
    let id = s.sql_ran(q, None, out);
    assert!(s.run("layer.addSql", &json!({"sql": "SELECT 2 AS x", "ran": id})).unwrap_err().contains("another query"));
    let r = s.run("layer.addSql", &json!({"sql": q, "ran": id, "name": "Ran"})).unwrap();
    assert_eq!(r["features"], 1);
    assert!(s.run("layer.addSql", &json!({"sql": q, "ran": id})).unwrap_err().contains("no longer kept"));
    s.run("layer.remove", &json!({"layer": "Ran"})).unwrap();
    assert_eq!(s.map().layers.len(), before, "no layer is added");
    let r = s.run("sql.query", &json!({"sql": "SELECT 'a' AS x", "limit": 5000})).unwrap();
    assert_eq!((r["geometry"].as_bool(), r["more"].as_bool()), (Some(false), Some(false)));
    assert!(s.run("sql.query", &json!({"sql": "SELECT * FROM nowhere"})).unwrap_err().contains("nowhere"));
    // A database's tables and their columns.
    let dir = std::env::temp_dir().join(format!("rustgis-engine-sqlws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("towns.duckdb");
    let ok = std::process::Command::new(rustgis_data::source::duckdb::program().unwrap())
        .arg(&db)
        .arg("-c")
        .arg("CREATE TABLE towns AS SELECT 'x' AS name, 3 AS pop;")
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let r = s.run("sql.schema", &json!({"database": db.display().to_string()})).unwrap();
    assert_eq!(r["tables"]["towns"], json!(["name", "pop"]));
    let r = s.run("sql.query", &json!({"sql": "SELECT pop FROM towns", "database": db.display().to_string()})).unwrap();
    assert_eq!(r["rows"], json!([[3]]));
    let _ = std::fs::remove_dir_all(dir);
}

/// Serves `body` with byte ranges (`Range: bytes=a-b` answered 206) until the test ends; returns
/// the base address and the count of bytes sent.
fn serve_ranges(body: Vec<u8>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    serve_file(body, true)
}

/// [`serve_ranges`], or (`ranges` false) a server that ignores `Range` and answers every request
/// with the whole file.
fn serve_file(body: Vec<u8>, ranges: bool) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    serve_gated(body, ranges, None)
}

/// [`serve_file`] holding every answer until `gate` is set (a request still under way, for as long
/// as a test needs).
fn serve_gated(
    body: Vec<u8>,
    ranges: bool,
    gate: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let sent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = sent.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let read = s.read(&mut buf).unwrap_or(0);
            if let Some(g) = &gate {
                let start = std::time::Instant::now();
                while !g.load(std::sync::atomic::Ordering::Relaxed) && start.elapsed() < std::time::Duration::from_secs(60) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            let req = String::from_utf8_lossy(&buf[..read]).to_ascii_lowercase();
            let range = req.lines().filter(|_| ranges).find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| {
                let (a, b) = r.trim().split_once('-')?;
                // A suffix (`-n`): the last n bytes.
                if a.is_empty() {
                    let n = b.parse::<usize>().ok()?.min(body.len());
                    return Some((body.len() - n, body.len().saturating_sub(1)));
                }
                Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
            });
            let (status, part) = match range {
                Some((a, b)) if a < body.len() => ("206 Partial Content", &body[a..=b.min(body.len() - 1)]),
                Some(_) => ("416 Range Not Satisfiable", &body[..0]),
                None => ("200 OK", &body[..]),
            };
            count.fetch_add(part.len(), std::sync::atomic::Ordering::Relaxed);
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", part.len());
            let _ = s.write_all(part);
        }
    });
    (format!("http://{addr}"), sent)
}

/// A STAC collection's band as a data cube on the time slider (#559), from a local API: a tile
/// layer per grid tile, each a file per date; the slider on over their dates; a fixed range in the
/// band's own units, through its scale; a tile with no date in the window drawn as nothing.
#[test]
fn a_stac_data_cube_on_the_time_slider() {
    let (cog, _) = serve_ranges(rustgis_data::raster::cog::example(512, 256));
    let item = |id: &str, tile: &str, date: &str, band: bool| {
        let asset = if band {
            format!(
                r#""NDVI": {{"href": "{cog}/{id}.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "eo:bands": [{{"scale": 0.0001}}]}}"#
            )
        } else {
            format!(r#""EVI": {{"href": "{cog}/{id}.tif"}}"#)
        };
        format!(
            r#"{{"type": "Feature", "id": "{id}", "collection": "cube", "bbox": [10, 45, 11, 46], "geometry": null,
               "properties": {{"datetime": "{date}T00:00:00Z", "bdc:tiles": ["{tile}"]}}, "assets": {{{asset}}}}}"#
        )
    };
    let features = [
        item("a1", "T1", "2024-01-01", true),
        item("a2", "T1", "2024-01-17", true),
        item("b1", "T2", "2024-01-01", true),
        item("b2", "T2", "2024-01-01", true),
        item("c1", "T3", "2024-01-17", false),
    ]
    .join(",");
    let base = serve_routes(vec![("/v1/search", format!(r#"{{"type": "FeatureCollection", "features": [{features}]}}"#))], 4);
    let api = format!("{base}/v1");
    let mut s = Session::new(Project::default());
    let r = s
        .run(
            "stac.addCube",
            &json!({"url": api, "collection": "cube", "asset": "ndvi", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31", "range": [0, 0.9]}),
        )
        .unwrap();
    assert_eq!(
        (r["items"].as_u64(), r["tiles"].as_u64(), r["dates"].as_u64(), r["layers"].as_u64(), r["skipped"].as_u64()),
        (Some(5), Some(2), Some(2), Some(2), Some(1)),
        "{r}"
    );
    assert_eq!(r["duplicates"], 1, "the second item of T2 on 2024-01-01 is left out and counted");
    assert!(crate::cmd::tiles::NETWORK.contains(&"stac.addCube"), "a network command: a plugin needs the network permission to run it");
    // An end given as a year or a month takes all of it: "end": "2024-01" reaches 31 January.
    // Read as 1 January, it would be before this start and refused.
    let mut by_month = Session::new(Project::default());
    let r2 = by_month
        .run(
            "stac.addCube",
            &json!({"url": api, "collection": "cube", "asset": "ndvi", "bbox": [10, 45, 11, 46], "start": "2024-01-15", "end": "2024-01"}),
        )
        .unwrap();
    assert_eq!(r2["dates"], 2, "an end given as a year or a month reaches its last day: {r2}");
    // A slider already on takes the cube's dates in: one over other dates gains these two.
    let mut busy = Session::new(Project::default());
    busy.run("time.set", &json!({"dates": ["2023-06-01", "2023-07-01"]})).unwrap();
    busy.run(
        "stac.addCube",
        &json!({"url": api, "collection": "cube", "asset": "ndvi", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31"}),
    )
    .unwrap();
    assert_eq!(busy.run("time.info", &Value::Null).unwrap()["steps"], 4, "the slider takes the cube's dates in");
    let tiles = s.map().tiles.clone();
    assert_eq!(tiles.len(), 2);
    let t1 = tiles.iter().find(|t| t.name.ends_with("T1")).unwrap();
    let t2 = tiles.iter().find(|t| t.name.ends_with("T2")).unwrap();
    // A file per date (the first item of a tile and day), the range in stored values (0.9 / 0.0001).
    assert_eq!(t1.time_urls.len(), 2);
    assert_eq!(t2.time_urls.len(), 1);
    assert!(t1.time_urls.iter().all(|(_, u)| u.ends_with("#range=0,9000")), "{:?}", t1.time_urls);
    assert!(t2.time_urls[0].1.contains("/b1.tif"));
    // The slider: on, over the two dates; at the first, both tiles draw.
    assert_eq!(s.run("time.info", &Value::Null).unwrap()["steps"], 2);
    assert!(t1.draws() && t2.draws() && t1.url.contains("/a1.tif"));
    // The second date: T1's second file; T2 has none then and draws nothing.
    s.run("time.goTo", &json!({"step": 1})).unwrap();
    let t1 = s.map().tiles.iter().find(|t| t.name.ends_with("T1")).unwrap();
    let t2 = s.map().tiles.iter().find(|t| t.name.ends_with("T2")).unwrap();
    assert!(t1.url.contains("/a2.tif") && t1.draws(), "{}", t1.url);
    assert!(!t2.draws());
    // Saved and opened again, each tile's dates and the slider's step come back.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().tiles.iter().find(|x| x.name.ends_with("T1")).unwrap();
    assert_eq!(again.time_urls.len(), 2);
    assert!(again.url.contains("/a2.tif"), "{}", again.url);
    // T2, with no file at that step, draws nothing from the start: opening takes the window.
    assert!(!t.map().tiles.iter().find(|x| x.name.ends_with("T2")).unwrap().draws());
    for bad in [
        json!({"url": api, "asset": "ndvi", "start": "2024-01-01", "end": "2024-01-31"}),
        json!({"url": api, "collection": "cube", "start": "2024-01-01", "end": "2024-01-31"}),
        json!({"url": api, "collection": "cube", "asset": "ndvi", "start": "2024-02-01", "end": "2024-01-01"}),
        json!({"url": api, "collection": "cube", "asset": "ndvi", "start": "2024-01-01", "end": "2024-01-31", "range": [1, 0]}),
        json!({"url": api, "collection": "cube", "asset": "ndvi", "start": "June", "end": "2024-01-31"}),
        json!({"url": api, "collection": "cube", "asset": "ndvi", "start": "2024-01-01", "end": "2024-01-31", "range": ["a", 0, 1]}),
        json!({"url": api, "collection": "cube", "asset": "ndvi", "start": "2024-01-01", "end": "2024-01-31", "range": [0, "x", 1]}),
    ] {
        assert!(s.run("stac.addCube", &bad).is_err(), "{bad}");
    }
    // A file read whole (a GeoTIFF that isn't cloud-optimised) can't follow the slider: the cube
    // is refused, and what it added is taken back, the view its first tile framed included.
    let gt = rustgis_data::raster::GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.5, pixel_height: -0.5 };
    let plain = rustgis_data::raster::Raster::new(
        "p",
        2,
        2,
        vec![vec![1.0; 4]],
        gt,
        rustgis_data::raster::Crs::LonLat,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let (file, _) = serve_ranges(rustgis_data::raster::geotiff::write(&plain, 0).unwrap());
    let one = format!(
        r#"{{"type": "Feature", "id": "p1", "collection": "cube", "bbox": [10, 45, 11, 46], "geometry": null,
           "properties": {{"datetime": "2024-01-01T00:00:00Z", "bdc:tiles": ["P"]}}, "assets": {{"NDVI": {{"href": "{file}/p.tif"}}}}}}"#
    );
    let first = item("q1", "A", "2024-01-01", true);
    let plain_api = serve_routes(vec![("/v1/search", format!(r#"{{"type": "FeatureCollection", "features": [{first},{one}]}}"#))], 2);
    s.zoom_to(&rustgis_geom::Extent::new(-60.0, -30.0, -50.0, -20.0));
    let (tiles, layers) = (s.map().tiles.len(), s.map().layers.len());
    let (camera, time) = (s.map().camera, s.map().time.clone());
    let e = s
        .run(
            "stac.addCube",
            &json!({"url": format!("{plain_api}/v1"), "collection": "cube", "asset": "ndvi", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31"}),
        )
        .unwrap_err();
    assert!(e.contains("not a cloud-optimised"), "{e}");
    assert_eq!((s.map().tiles.len(), s.map().layers.len()), (tiles, layers));
    // The view tile A framed and the slider are put back too.
    assert_eq!(s.map().camera, camera);
    assert_eq!(s.map().time, time);
    // Files stored at another scale partway: one range can't fit them all, so it's refused.
    let rescaled = |id: &str, date: &str, scale: f64| {
        format!(
            r#"{{"type": "Feature", "id": "{id}", "collection": "cube", "bbox": [10, 45, 11, 46], "geometry": null,
               "properties": {{"datetime": "{date}T00:00:00Z", "bdc:tiles": ["T1"]}},
               "assets": {{"NDVI": {{"href": "{cog}/{id}.tif", "type": "image/tiff; profile=cloud-optimized", "eo:bands": [{{"scale": {scale}}}]}}}}}}"#
        )
    };
    let two = [rescaled("r1", "2024-01-01", 0.0001), rescaled("r2", "2024-01-17", 0.001)].join(",");
    let rescaled_api = serve_routes(vec![("/v1/search", format!(r#"{{"type": "FeatureCollection", "features": [{two}]}}"#))], 2);
    let e = s
        .run(
            "stac.addCube",
            &json!({"url": format!("{rescaled_api}/v1"), "collection": "cube", "asset": "ndvi", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31", "range": [0, 0.9]}),
        )
        .unwrap_err();
    assert!(e.contains("same scale and offset"), "{e}");
    // Files behind signed addresses (Planetary Computer's) are refused before anything is added.
    let signed = r#"{"type": "Feature", "id": "pc1", "collection": "cube", "bbox": [10, 45, 11, 46], "geometry": null,
        "properties": {"datetime": "2024-01-01T00:00:00Z", "bdc:tiles": ["S"]},
        "assets": {"NDVI": {"href": "https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/a.tif"}}}"#;
    let signed_api = serve_routes(vec![("/v1/search", format!(r#"{{"type": "FeatureCollection", "features": [{signed}]}}"#))], 2);
    let (tiles, layers) = (s.map().tiles.len(), s.map().layers.len());
    let e = s
        .run(
            "stac.addCube",
            &json!({"url": format!("{signed_api}/v1"), "collection": "cube", "asset": "ndvi", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31"}),
        )
        .unwrap_err();
    assert!(e.contains("signed addresses"), "{e}");
    assert_eq!((s.map().tiles.len(), s.map().layers.len()), (tiles, layers));
}

/// A search's `"bbox": "view"` is the map's view in any case or spacing, as the trend and the data
/// cube read it; any other text is refused, never read as no area at all.
#[test]
fn a_stac_search_takes_view_in_any_case() {
    let s = world();
    let search = |bbox: &str| crate::cmd::stac::search_params(&s, &json!({"bbox": bbox})).unwrap().bbox;
    assert!(search("view").is_some());
    assert_eq!(search(" VIEW "), search("view"));
    for bad in [json!("nowhere"), json!(7), json!({"w": 1}), json!([10, 45, 11, 46, "extra"]), json!([10, 45, 11, "46"])] {
        let e = crate::cmd::stac::search_params(&s, &json!({"bbox": bad})).unwrap_err();
        assert!(e.contains("bbox is"), "{bad}: {e}");
    }
    assert!(crate::cmd::stac::search_params(&s, &json!({})).unwrap().bbox.is_none(), "no bbox, no area");
}

/// A trend given `"bbox": " VIEW "` searches the map's view: its search request carries the box
/// the trend reads, not no area at all.
#[test]
fn a_stac_trend_searches_the_view_it_reads() {
    use std::io::{Read, Write};
    let mut s = world();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}/v1", listener.local_addr().unwrap());
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        for mut c in listener.incoming().take(4).flatten() {
            let mut buf = [0u8; 8192];
            let n = c.read(&mut buf).unwrap_or(0);
            let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
            seen.lock().unwrap().push(line);
            let body = r#"{"type": "FeatureCollection", "features": []}"#;
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let p = json!({"url": api, "collection": "mod", "asset": "NDVI", "bbox": " VIEW ", "start": "2024", "end": "2024"});
    let e = s.run("cube.trend", &p).unwrap_err();
    assert!(e.contains("no item"), "the search was made and found nothing: {e}");
    let lines = asked.lock().unwrap().clone();
    let search = lines.iter().find(|l| l.contains("/search")).unwrap_or_else(|| panic!("{lines:?}"));
    let query = search.split(['?', ' ']).nth(2).unwrap_or_default();
    let bbox = query.split('&').find_map(|kv| kv.strip_prefix("bbox=")).unwrap_or_else(|| panic!("no bbox in {search}"));
    // The commas as sent, or percent-encoded in either case.
    let asked: Vec<f64> = bbox.replace("%2C", ",").replace("%2c", ",").split(',').map(|v| v.parse().unwrap()).collect();
    let view = crate::cmd::stac::search_params(&s, &json!({"bbox": "view"})).unwrap().bbox.unwrap();
    assert_eq!(asked.len(), 4, "{search}");
    for (a, v) in asked.iter().zip(view) {
        assert!((a - v).abs() < 1e-6, "{search} against the view {view:?}");
    }
}

/// A trend over a STAC collection's band (#579), read in place from its files over an area: each
/// date's values scaled as its own metadata says (here date k's scale is k + 1 ten-thousandths,
/// so every cell grows in a straight line), Sen's slope as worked out by hand; the same outputs
/// read whole or in pieces of rows; a coarser cell size read from the files' overviews; and the
/// requests a trend can't take refused.
#[test]
fn a_trend_over_a_stac_collection() {
    use crate::cmd::stac::PIECE_VALUES;
    let (cog, _) = serve_ranges(rustgis_data::raster::cog::example(512, 256));
    let dates = ["2024-01-01", "2024-01-17", "2024-02-02", "2024-02-18", "2024-03-05", "2024-03-21"];
    let features: Vec<String> = dates
        .iter()
        .enumerate()
        .map(|(k, d)| {
            format!(
                r#"{{"type": "Feature", "id": "n{k}", "collection": "mod", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["h13v09"]}},
                   "assets": {{"NDVI": {{"href": "{cog}/n{k}.tif", "eo:bands": [{{"scale": {}, "offset": {}}}]}}}}}}"#,
                (k + 1) as f64 * 1e-4,
                k as f64 * 0.01
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    let base = serve_routes(vec![("/v1/search", search)], 16);
    let api = format!("{base}/v1");
    let params = json!({"url": api, "collection": "mod", "asset": "NDVI", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-03", "test": "original"});
    let mut s = Session::new(Project::default());
    let run = |s: &mut Session, p: &Value| {
        let r = s.run("cube.trend", p).unwrap();
        let ids: Vec<u32> = r["layers"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
        let raster = |id: u32| s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
        (r["dates"].as_u64(), ids.iter().map(|id| raster(*id)).collect::<Vec<_>>())
    };
    let (n, whole) = run(&mut s, &params);
    assert_eq!(n, Some(6));
    let slope = &whole[0];
    assert_eq!((slope.width, slope.height), (512, 512), "the area on the full images' grid");
    // Pixel (x, y) stores (x + y) % 256; with date k's scale (k + 1) ten-thousandths and offset
    // k hundredths, its value grows by that many ten-thousandths and a hundredth a date, 16 days
    // apart: each file's own scale and offset taken.
    let per_year = |x: usize, y: usize| (((x + y) % 256) as f64 * 1e-4 + 0.01) * 365.2425 / 16.0;
    for (x, y) in [(10, 20), (300, 100), (0, 0), (511, 511)] {
        let got = f64::from(slope.bands[0].data[y * 512 + x]);
        assert!((got - per_year(x, y)).abs() < 1e-4, "({x}, {y}): {got} vs {}", per_year(x, y));
    }
    // A straight rise over six dates is significant, (0, 0)'s too: its stored 0 rises by the offsets.
    assert!(whole[3].bands[0].data[20 * 512 + 10].is_finite() && whole[3].bands[0].data[0].is_finite());
    // An area wider than the file: half a degree more to the west, 256 columns no file covers
    // (NaN), and the file's own beside them, in their places.
    let wider = json!({"url": api, "collection": "mod", "asset": "NDVI", "bbox": [9.5, 45, 11, 46], "start": "2024-01-01", "end": "2024-03", "test": "original"});
    let (_, wide) = run(&mut s, &wider);
    let ws = &wide[0];
    assert_eq!((ws.width, ws.height), (768, 512));
    assert!(ws.bands[0].data[20 * 768 + 255].is_nan() && ws.bands[0].data[0].is_nan());
    for (x, y) in [(10, 20), (511, 511)] {
        let got = f64::from(ws.bands[0].data[y * 768 + 256 + x]);
        assert!((got - per_year(x, y)).abs() < 1e-4, "({x}, {y}): {got} vs {}", per_year(x, y));
    }
    // The parameters read without the network are refused before the search: an API on a
    // closed port, never asked.
    for (bad, said) in [
        (json!({"bbox": [11, 45, 10, 46]}), "bbox is"),
        (json!({"cellSize": 0}), "cellSize is"),
        (json!({"resampling": "bilinear"}), "resampling is"),
    ] {
        let mut q = json!({"url": "http://127.0.0.1:9/v1", "collection": "mod", "asset": "NDVI", "bbox": [10, 45, 11, 46], "start": "2024", "end": "2024"});
        for (k, v) in bad.as_object().unwrap() {
            q[k] = v.clone();
        }
        let e = s.run("cube.trend", &q).unwrap_err();
        assert!(e.contains(said), "{e}");
    }
    // An area no file covers (the items only near it) is refused, not four empty rasters.
    let away = json!({"url": api, "collection": "mod", "asset": "NDVI", "bbox": [20, 45, 21, 46], "start": "2024-01-01", "end": "2024-03"});
    let e = s.run("cube.trend", &away).unwrap_err();
    assert!(e.contains("covers the area"), "{e}");
    // Ten tiles, more than the cover probes reach, none covering it: refused once read, not four
    // empty rasters.
    let many: Vec<String> = (0..10)
        .flat_map(|t| {
            let cog = cog.clone();
            dates.iter().enumerate().map(move |(k, d)| {
                format!(
                    r#"{{"type": "Feature", "id": "t{t}n{k}", "collection": "mod", "bbox": [20, 45, 21, 46], "geometry": null,
                       "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t{t}"]}},
                       "assets": {{"NDVI": {{"href": "{cog}/t{t}n{k}.tif"}}}}}}"#
                )
            })
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, many.join(","));
    let tiles_api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 16));
    let e = s.run("cube.trend", &json!({"url": tiles_api, "collection": "mod", "asset": "NDVI", "bbox": [20, 45, 21, 46], "start": "2024-01-01", "end": "2024-03"})).unwrap_err();
    assert!(e.contains("covers the area"), "{e}");
    // Read in two pieces (a quarter of the rows' values each would make four; 256-row tiles make
    // it two), the outputs are the same, cell for cell.
    PIECE_VALUES.with(|v| v.set(Some(512 * 300 * 6)));
    let (_, pieces) = run(&mut s, &params);
    PIECE_VALUES.with(|v| v.set(None));
    for (a, b) in whole.iter().zip(&pieces) {
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert!(a.bands[0].data.iter().zip(&b.bands[0].data).all(|(x, y)| x == y || (x.is_nan() && y.is_nan())), "{}", a.name);
        assert!(crate::cmd::cube::same_grid(&a.transform, &b.transform));
    }
    // Cancelled, the readers stop early and the run says so, rather than work out a block only
    // partly read.
    let job = crate::cmd::cube::trend_start(&s, &params).unwrap();
    let e = job.run(&std::sync::atomic::AtomicBool::new(true), &std::sync::atomic::AtomicUsize::new(0)).unwrap_err();
    assert_eq!(e, "cancelled");
    // A row too wide for a piece is said.
    PIECE_VALUES.with(|v| v.set(Some(100)));
    let e = s.run("cube.trend", &params).unwrap_err();
    PIECE_VALUES.with(|v| v.set(None));
    assert!(e.contains("more than a run's block"), "{e}");
    // A coarser cell size reads the overview nearest it: 256 × 256 cells of 1/256°.
    let mut coarse = params.clone();
    coarse["cellSize"] = json!(0.004);
    let (_, half) = run(&mut s, &coarse);
    assert_eq!((half[0].width, half[0].height), (256, 256));
    // Averaged, each 1/256° cell is the mean of its 2 × 2 full pixels, (2x + 2y + 1) where the
    // overview's (nearest) pixel is 2x + 2y.
    coarse["resampling"] = json!("average");
    let (_, mean) = run(&mut s, &coarse);
    assert_eq!((mean[0].width, mean[0].height), (256, 256));
    for (x, y) in [(5, 7), (30, 40)] {
        let (near, avg) = (f64::from(half[0].bands[0].data[y * 256 + x]), f64::from(mean[0].bands[0].data[y * 256 + x]));
        let (unit, offsets) = (1e-4 * 365.2425 / 16.0, 0.01 * 365.2425 / 16.0);
        assert!((near - (2 * x + 2 * y) as f64 * unit - offsets).abs() < 1e-4, "nearest ({x}, {y}): {near}");
        assert!((avg - (2 * x + 2 * y + 1) as f64 * unit - offsets).abs() < 1e-4, "average ({x}, {y}): {avg}");
    }
    let before = s.map().layers.len();
    for bad in [json!("bilinear"), json!(3)] {
        coarse["resampling"] = bad;
        assert!(s.run("cube.trend", &coarse).unwrap_err().contains("resampling"));
    }
    assert_eq!(s.map().layers.len(), before, "refused, nothing added");
    coarse["resampling"] = Value::Null;
    // The level nearest the size asked: 0.0025 is nearer the full images' 1/512° than 1/256°.
    coarse["cellSize"] = json!(0.0025);
    assert_eq!(run(&mut s, &coarse).1[0].width, 512);
    // What can't be read is refused before anything is added.
    let layers = s.map().layers.len();
    for (bad, said) in [
        (json!({"cellSize": -1}), "cellSize"),
        (json!({"cellSize": "fine"}), "cellSize"),
        (json!({"bbox": [11, 45, 10, 46]}), "bbox"),
        (json!({"asset": null}), "asset"),
        (json!({"bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-20"}), "dates"),
    ] {
        let mut p = params.clone();
        for (k, v) in bad.as_object().unwrap() {
            p[k] = v.clone();
        }
        let e = s.run("cube.trend", &p).unwrap_err();
        assert!(e.contains(said), "{bad}: {e}");
    }
    assert_eq!(s.map().layers.len(), layers);
}

/// A cloud-optimised GeoTIFF by its address is read in place: its header, then only the tiles a
/// map tile needs, by range; a GeoTIFF in strips is downloaded whole as a raster layer.
#[test]
fn cloud_optimised_geotiffs_are_read_in_place() {
    use crate::render::rustgis_tiles::cache::{self, Payload, State, Want};
    let file = rustgis_data::raster::cog::example(2048, 256);
    let total = file.len();
    let (base, sent) = serve_ranges(file);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/scene.tif")})).unwrap();
    assert_eq!(r["inPlace"], true, "{r}");
    let t = s.map().tiles.last().unwrap().clone();
    assert_eq!((t.kind, t.name.as_str()), (rustgis_map::TileKind::Cog, "scene"));
    // Framed on the file: 10–11° E, 45–46° N.
    let b = r["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!((b[0] - 10.0).abs() < 1e-6 && (b[3] - 46.0).abs() < 1e-6, "{b:?}");
    // A map tile inside it (zoom 9) is drawn from a few of its tiles, not the whole file.
    let (z, lon, lat) = (9u32, 10.5f64, 45.5f64);
    let n = f64::from(1u32 << z);
    let x = ((lon + 180.0) / 360.0 * n) as u32;
    let y = ((1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * n) as u32;
    let tile = format!("cog:{z}/{x}/{y}@{base}/scene.tif");
    let mut got = None;
    for _ in 0..200 {
        match cache::get(&tile, Want::Image) {
            State::Ready(p) => {
                got = Some(p);
                break;
            }
            State::Failed(e) => panic!("{e}"),
            State::Pending => cache::wait(std::time::Duration::from_millis(50)),
        }
    }
    let Some(p) = got else { panic!("the tile never came") };
    let Payload::Image(pm) = &*p else { panic!("not a picture") };
    assert_eq!((pm.width(), pm.height()), (256, 256));
    assert!(pm.pixels().iter().any(|px| px.alpha() == 255));
    let read = sent.load(std::sync::atomic::Ordering::Relaxed);
    assert!(read < total / 4, "{read} of {total} bytes read");
    // A tile outside it is empty.
    assert!(matches!(cache::get(&format!("cog:9/0/0@{base}/scene.tif"), Want::Image), State::Pending | State::Ready(_)));
    // The project keeps the address; asked to, the file is downloaded whole instead.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("\"kind\":\"cog\"") && text.contains("scene.tif"));
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/scene.tif"), "whole": true, "name": "Whole"})).unwrap();
    assert!(r.get("inPlace").is_none(), "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).is_some_and(|l| l.is_raster()), "{r}");
    // Opened as a scan, it is downloaded whole and has no coordinates, never read in place.
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/scene.tif"), "as": "scan", "name": "Scan"})).unwrap();
    assert!(r.get("inPlace").is_none(), "{r}");
    assert_eq!(r["georeferenced"], false, "{r}");
    // A dated address is read in place with its coordinates, so it can't be a scan.
    let e = s.run("layer.addUrl", &json!({"url": format!("{base}/{{date:YYYY}}.tif"), "as": "scan"})).unwrap_err();
    assert!(e.contains("scan"), "{e}");
    // Anything else in "as" is refused, not ignored, and tiles can't be a scan.
    for (url, as_) in [("scene.tif", json!("pdf")), ("scene.tif", json!(3)), ("a.pmtiles", json!("scan")), ("a.fgb", json!("scan"))] {
        let e = s.run("layer.addUrl", &json!({"url": format!("{base}/{url}"), "as": as_})).unwrap_err();
        assert!(e.contains("\"as\"") || e.contains("scan"), "{url}: {e}");
    }
    // A GeoTIFF in strips isn't cloud-optimised: downloaded whole, a raster layer.
    let r = rustgis_data::raster::Raster::new(
        "strips",
        8,
        8,
        vec![vec![3.0; 64]],
        rustgis_data::raster::GeoTransform { origin_x: 5.0, origin_y: 50.0, pixel_width: 0.1, pixel_height: -0.1 },
        rustgis_data::raster::Crs::LonLat,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let (base, _) = serve_ranges(rustgis_data::raster::geotiff::write(&r, 0).unwrap());
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/strips.tif")})).unwrap();
    assert!(r.get("inPlace").is_none(), "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).is_some_and(|l| l.is_raster()), "{r}");
}

/// OpenStreetMap data for an area through an Overpass server (a local one): a layer per geometry
/// kind; geoBoundaries' GeoJSON given as text; bad requests refused.
#[test]
fn osm_and_boundaries() {
    const ANSWER: &[u8] = br#"{"elements": [
        {"type": "node", "id": 1, "lat": 46.945, "lon": 7.445, "tags": {"amenity": "cafe", "name": "Kaffee"}},
        {"type": "way", "id": 2, "tags": {"building": "yes"}, "geometry": [{"lat": 46.94, "lon": 7.44}, {"lat": 46.94, "lon": 7.441}, {"lat": 46.941, "lon": 7.441}, {"lat": 46.94, "lon": 7.44}]},
        {"type": "way", "id": 3, "tags": {"highway": "residential", "name": "Gasse"}, "geometry": [{"lat": 46.94, "lon": 7.44}, {"lat": 46.945, "lon": 7.445}]}
    ]}"#;
    let base = serve(ANSWER, 1);
    let mut s = Session::new(Project::default());
    let r = s.run("osm.download", &json!({"bbox": [7.44, 46.94, 7.45, 46.95], "tags": "amenity, building, highway", "url": base})).unwrap();
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(3), "{r}");
    let names: Vec<String> = s.map().layers.iter().map(|l| l.name.clone()).collect();
    assert!(names.iter().any(|n| n == "OSM amenity, building, highway (polygons)"), "{names:?}");
    // The app's answer, passed in.
    let r = s
        .run(
            "osm.download",
            &json!({"bbox": [7.44, 46.94, 7.45, 46.95], "preset": "pois", "name": "Cafés", "answer": std::str::from_utf8(ANSWER).unwrap()}),
        )
        .unwrap();
    assert_eq!(r["features"]["points"], 1);
    for bad in [
        json!({"bbox": [7.0, 46.0, 8.0, 47.0], "preset": "roads"}),
        json!({"bbox": [7.44, 46.94, 7.45, 46.95]}),
        json!({"bbox": [7.44, 46.94, 7.45, 46.95], "preset": "x"}),
        json!({"bbox": [7.44, 46.94, 7.45, 46.95], "preset": "roads", "answer": "<html>busy</html>"}),
        json!({"bbox": "here", "preset": "roads"}),
        // An entry that isn't a number isn't dropped: the box is refused.
        json!({"bbox": [7.44, "x", 46.94, 7.45, 46.95], "preset": "roads"}),
        json!({"preset": "roads", "view": false}),
    ] {
        assert!(s.run("osm.download", &bad).is_err(), "{bad}");
    }
    let gj = r#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"shapeName": "Bern"}, "geometry": {"type": "Polygon", "coordinates": [[[7, 46], [8, 46], [8, 47], [7, 46]]]}}]}"#;
    let r = s.run("boundaries.add", &json!({"country": "che", "level": 1, "text": gj})).unwrap();
    assert_eq!(r["features"], 1);
    assert!(s.map().layer_by_name("CHE ADM1").is_some());
    for bad in [
        json!({}),
        json!({"country": "CH", "text": gj}),
        json!({"country": "CHE", "level": "ADM9", "text": gj}),
        json!({"country": "CHE", "text": "{"}),
    ] {
        assert!(s.run("boundaries.add", &bad).is_err(), "{bad}");
    }
}

/// Search Open Data against a local CKAN site: datasets with their resources (what RustGIS can't
/// add dropped, the best format first), one added by its address; bad requests are refused.
#[test]
fn open_data_portal_search_and_add() {
    let ckan = r#"{"success": true, "result": {"count": 2, "results": [
        {"name": "parks", "title": "Parks", "notes": "<p>City <b>parks</b> &amp; gardens</p>", "organization": {"title": "City"},
         "resources": [{"format": "PDF", "url": "{base}/parks.pdf"}, {"format": "CSV", "url": "{base}/parks.csv"}, {"format": "GeoJSON", "url": "{base}/parks.geojson"}]},
        {"name": "report", "title": "A report", "resources": [{"format": "PDF", "url": "{base}/r.pdf"}]}
    ]}}"#;
    let gj = r#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"n": 1}, "geometry": {"type": "Point", "coordinates": [10, 20]}}]}"#;
    let base = serve_routes(vec![("/api/3/action/package_search", ckan.to_string()), ("/parks.geojson", gj.to_string())], 2);
    let mut s = Session::new(Project::default());
    let r = s.run("portal.search", &json!({"portal": "ckan", "url": base, "query": "parks"})).unwrap();
    assert_eq!(r["count"], 1, "{r}");
    let d = &r["datasets"][0];
    assert_eq!(d["title"], "Parks");
    assert_eq!(d["description"], "City parks & gardens");
    let formats: Vec<&str> = d["resources"].as_array().unwrap().iter().filter_map(|x| x["format"].as_str()).collect();
    assert_eq!(formats, ["GeoJSON", "CSV"]);
    let url = d["resources"][0]["url"].as_str().unwrap();
    let r = s.run("portal.add", &json!({"url": url, "name": "Parks", "portal": base})).unwrap();
    assert_eq!(r["features"], 1, "{r}");
    assert!(s.map().layer_by_name("Parks").is_some());
    for (id, bad) in [
        ("portal.search", json!({"portal": "ckan", "url": base})),
        ("portal.search", json!({"portal": "nope", "query": "parks"})),
        ("portal.search", json!({"portal": "ckan", "url": "file:///etc", "query": "parks"})),
        ("portal.add", json!({})),
        ("portal.add", json!({"url": "file:///etc/passwd"})),
        ("portal.add", json!({"url": "https://example.org/x.geojson", "use": "ftp"})),
        // A public portal's document can't point into the local network.
        ("portal.add", json!({"url": "http://127.0.0.1/x.geojson", "portal": "https://catalog.data.gov"})),
    ] {
        assert!(s.run(id, &bad).is_err(), "{id} {bad}");
    }
}

/// A LAS 1.2 file of point format 0 in longitude/latitude: (x, y, z, class) per point.
fn las_bytes(points: &[(f64, f64, f64, u8)]) -> Vec<u8> {
    let mut b = vec![0u8; 227];
    b[0..4].copy_from_slice(b"LASF");
    b[24] = 1;
    b[25] = 2;
    b[94..96].copy_from_slice(&227u16.to_le_bytes());
    b[96..100].copy_from_slice(&227u32.to_le_bytes());
    b[105..107].copy_from_slice(&20u16.to_le_bytes());
    b[107..111].copy_from_slice(&(points.len() as u32).to_le_bytes());
    for k in 0..3 {
        b[131 + k * 8..139 + k * 8].copy_from_slice(&1e-6f64.to_le_bytes());
    }
    // max x, min x, max y, min y, max z, min z.
    for (at, v) in [(179, 11.0f64), (187, 10.0), (195, 51.0), (203, 50.0), (211, 400.0), (219, 300.0)] {
        b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    for p in points {
        let mut r = [0u8; 20];
        for (k, v) in [p.0, p.1, p.2].iter().enumerate() {
            r[k * 4..k * 4 + 4].copy_from_slice(&((v / 1e-6).round() as i32).to_le_bytes());
        }
        r[14] = 0x09;
        r[15] = p.3;
        b.extend_from_slice(&r);
    }
    b
}

/// LiDAR point clouds: read in place (a project keeps the file's path, not its points), coloured
/// by ASPRS class, from a path and from an upload.
#[test]
fn lidar_point_clouds() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-lidar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pts: Vec<(f64, f64, f64, u8)> =
        (0..200).map(|i| (10.0 + f64::from(i) / 200.0, 50.5, 300.0 + f64::from(i % 50), if i % 2 == 0 { 2 } else { 6 })).collect();
    let path = dir.join("scan.las");
    std::fs::write(&path, las_bytes(&pts)).unwrap();
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
    assert_eq!(r["streamed"], true, "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    let l = s.map().layer(id).unwrap();
    match &l.renderer {
        rustgis_map::Renderer::Unique { field, classes, .. } => {
            assert_eq!(field, "classification");
            let ground = classes.iter().find(|(v, _)| v == "2").unwrap();
            assert_eq!(ground.1.fill, rustgis_map::Rgba::rgb(0xA6, 0x7C, 0x52));
        }
        other => panic!("{other:?}"),
    }
    // The project names the file; reopened, the points are read from it again.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("scan.las") && text.len() < 20_000, "{}", text.len());
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("scan").unwrap().source.as_ref().unwrap();
    assert!(again.is_open(), "{:?}", again.error);
    // An upload, and one class only: by elevation.
    let one: Vec<_> = pts.iter().map(|p| (p.0, p.1, p.2, 2)).collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(las_bytes(&one));
    let r = s.run("layer.addDataBytes", &json!({"fileName": "ground.las", "data": b64})).unwrap();
    let l = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
    assert!(matches!(&l.renderer, rustgis_map::Renderer::Graduated { field, .. } if field == "z"), "{:?}", l.renderer);
    // Not a point cloud.
    assert!(s.run("layer.addDataBytes", &json!({"fileName": "junk.laz", "data": "AAAA"})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// A Zarr store in a temporary folder: `t` over three monthly steps (days since 2001-01-01: 0,
/// 31, 59) on a 3 × 4 grid, step `k`'s values `100·k + i`.
#[cfg(not(target_arch = "wasm32"))]
fn zarr_store_over_time(tag: &str) -> (std::path::PathBuf, String) {
    zarr_store_at(tag, &[0, 31, 59])
}

/// A Zarr store of one variable, `t` (time × 3 × 4), with a step at each of `days` since 2001-01-01:
/// step `k`'s cells are `100 k + cell`.
#[cfg(not(target_arch = "wasm32"))]
fn zarr_store_at(tag: &str, days: &[i32]) -> (std::path::PathBuf, String) {
    let n = days.len();
    let dir = std::env::temp_dir().join(format!("rustgis-zarr-{tag}-{}", std::process::id()));
    let store = dir.join("air.zarr");
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["t", "lat", "lon", "time"] {
        std::fs::create_dir_all(store.join(d)).unwrap();
    }
    let arr = |shape: &str, chunks: &str, dtype: &str| {
        serde_json::from_str::<Value>(&format!(
            r#"{{"shape":{shape},"chunks":{chunks},"dtype":"{dtype}","compressor":null,"fill_value":null,"filters":null,"order":"C","zarr_format":2}}"#
        ))
        .unwrap()
    };
    let meta = json!({"metadata": {
        ".zgroup": {"zarr_format": 2},
        "t/.zarray": arr(&format!("[{n},3,4]"), "[1,3,4]", "<f4"),
        "t/.zattrs": {"_ARRAY_DIMENSIONS": ["time","lat","lon"], "units": "K"},
        "lat/.zarray": arr("[3]", "[3]", "<f4"),
        "lat/.zattrs": {"_ARRAY_DIMENSIONS": ["lat"], "units": "degrees_north"},
        "lon/.zarray": arr("[4]", "[4]", "<f4"),
        "lon/.zattrs": {"_ARRAY_DIMENSIONS": ["lon"], "units": "degrees_east"},
        "time/.zarray": arr(&format!("[{n}]"), &format!("[{n}]"), "<i4"),
        "time/.zattrs": {"_ARRAY_DIMENSIONS": ["time"], "units": "days since 2001-01-01"},
    }, "zarr_consolidated_format": 1});
    std::fs::write(store.join(".zmetadata"), meta.to_string()).unwrap();
    std::fs::write(store.join(".zgroup"), r#"{"zarr_format":2}"#).unwrap();
    let f32s = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    std::fs::write(store.join("lat/0"), f32s(&[10.0, 20.0, 30.0])).unwrap();
    std::fs::write(store.join("lon/0"), f32s(&[1.0, 2.0, 3.0, 4.0])).unwrap();
    std::fs::write(store.join("time/0"), days.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    for t in 0..n {
        let v: Vec<f32> = (0..12).map(|i| (t * 100 + i) as f32).collect();
        std::fs::write(store.join(format!("t/{t}.0.0")), f32s(&v)).unwrap();
    }
    let path = store.display().to_string();
    (dir, path)
}

/// A Zarr variable's series at a point (#555): the values along its time dimension at the cell
/// holding it, read from the chunks holding that cell, through a layer or the store itself.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_zarr_series_at_a_point() {
    let (dir, path) = zarr_store_over_time("series");
    let mut s = Session::new(Project::default());
    // At 1°E 30°N: the stored row 2 (latitudes 10, 20, 30), column 0, so 100·k + 8 at step k.
    let at = json!({"path": path, "variable": "t", "lon": 1.0, "lat": 30.0});
    let r = s.run("zarr.series", &at).unwrap();
    assert_eq!(r["values"], json!([8.0, 108.0, 208.0]), "{r}");
    assert_eq!(r["labels"], json!(["2001-01-01 00:00", "2001-02-01 00:00", "2001-03-01 00:00"]));
    assert_eq!(
        (r["dimension"].as_str(), r["units"].as_str(), r["row"].as_u64(), r["col"].as_u64()),
        (Some("time"), Some("K"), Some(2), Some(0))
    );
    // A point between centres takes the nearest (the axis starts half a cell before the first
    // centre): 24°N is the 20°N row, 26°N the 30°N row.
    let mut at =
        |lat: f64| s.run("zarr.series", &json!({"path": path, "variable": "t", "lon": 1.0, "lat": lat})).unwrap()["values"].clone();
    let (twenty, thirty) = (at(20.0), at(30.0));
    assert_eq!(at(24.0), twenty);
    assert_eq!(at(26.0), thirty);
    assert_ne!(twenty, thirty);
    // Longitudes either way round the globe; another cell.
    let r = s.run("zarr.series", &json!({"path": path, "variable": "t", "lon": 363.5, "lat": 11.0})).unwrap();
    assert_eq!(r["values"], json!([3.0, 103.0, 203.0]), "{r}");
    // A period.
    let r = s.run("zarr.series", &json!({"path": path, "variable": "t", "lon": 1.0, "lat": 30.0, "start": "2001-01-15"})).unwrap();
    assert_eq!(r["values"], json!([108.0, 208.0]));
    // Through a layer: one that follows the slider (its period), and a slice (the whole axis).
    let late = s.run("layer.addZarr", &json!({"path": path, "variable": "t", "time": true, "start": "2001-01-20"})).unwrap()["layer"]
        .as_u64()
        .unwrap();
    let r = s.run("zarr.series", &json!({"layer": late, "lon": 1.0, "lat": 30.0})).unwrap();
    assert_eq!(r["values"], json!([108.0, 208.0]));
    let slice = s.run("layer.addZarr", &json!({"path": path, "variable": "t", "index": {"time": 1}})).unwrap()["layer"].as_u64().unwrap();
    let r = s.run("zarr.series", &json!({"layer": slice, "lon": 1.0, "lat": 30.0})).unwrap();
    assert_eq!(r["values"], json!([8.0, 108.0, 208.0]));
    for bad in [
        json!({"path": path, "variable": "t", "lon": 50.0, "lat": 30.0}),
        json!({"path": path, "variable": "t", "lon": 1.0, "lat": 95.0}),
        json!({"path": path, "variable": "t", "lat": 30.0}),
        json!({"path": path, "variable": "t", "lon": 1.0, "lat": 30.0, "start": "2003-01-01"}),
        json!({"path": path, "variable": "lat", "lon": 1.0, "lat": 30.0}),
        json!({"path": "/nowhere/x.zarr", "lon": 1.0, "lat": 30.0}),
    ] {
        assert!(s.run("zarr.series", &bad).is_err(), "{bad}");
    }
    // A layer that isn't Zarr.
    let mut w = Session::new(sample::world_project());
    assert!(w.run("zarr.series", &json!({"layer": "Countries", "lon": 1.0, "lat": 30.0})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// A Zarr variable summarised over time (#556): each statistic, a season, a period and a layer's
/// period, into a raster layer kept in the project.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_zarr_variable_summarised_over_time() {
    let (dir, path) = zarr_store_over_time("reduce");
    let mut s = Session::new(Project::default());
    // The first pixel (north up) is the stored cell 8: 8, 108 and 208 at the three steps.
    let first = |s: &Session, r: &Value| {
        let l = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
        let d = l.raster.as_ref().unwrap().data.clone().unwrap();
        d.bands[0].data[0]
    };
    for (stat, want) in [("mean", 108.0), ("min", 8.0), ("max", 208.0), ("sum", 324.0), ("count", 3.0)] {
        let r = s.run("zarr.reduce", &json!({"path": path, "variable": "t", "stat": stat})).unwrap();
        assert_eq!(first(&s, &r), want, "{stat}");
        assert_eq!(r["steps"], 3);
    }
    let r = s.run("zarr.reduce", &json!({"path": path, "variable": "t", "stat": "std"})).unwrap();
    assert!((first(&s, &r) - 81.649_66).abs() < 1e-3, "{}", first(&s, &r));
    let r = s.run("zarr.reduce", &json!({"path": path, "variable": "t", "stat": "countAbove", "above": 150})).unwrap();
    assert_eq!(first(&s, &r), 1.0);
    // A season (February alone), and a period.
    let r = s.run("zarr.reduce", &json!({"path": path, "variable": "t", "stat": "mean", "months": [2]})).unwrap();
    assert_eq!(first(&s, &r), 108.0);
    let name = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().name.clone();
    assert!(name.starts_with("t mean 2001-02-01–2001-02-01 (months 2)"), "{name}");
    let r = s.run("zarr.reduce", &json!({"path": path, "variable": "t", "stat": "mean", "start": "2001-01-15"})).unwrap();
    assert_eq!(first(&s, &r), 158.0);
    // Through a layer that follows the slider: its period.
    let late = s.run("layer.addZarr", &json!({"path": path, "variable": "t", "time": true, "start": "2001-01-20"})).unwrap()["layer"]
        .as_u64()
        .unwrap();
    let r = s.run("zarr.reduce", &json!({"layer": late, "stat": "max", "name": "warmest"})).unwrap();
    assert_eq!(first(&s, &r), 208.0);
    assert!(s.map().layer_by_name("warmest").is_some());
    // Kept in the project as a GeoTIFF, read again.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("warmest").and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone()).unwrap();
    assert_eq!(again.bands[0].data[0], 208.0);
    // A step never written (its chunk absent) is Zarr's fill value: no value that step, so the
    // summary goes on with the others.
    let (gap_dir, gap) = zarr_store_over_time("reduce-gap");
    std::fs::remove_file(std::path::Path::new(&gap).join("t/1.0.0")).unwrap();
    let r = s.run("zarr.reduce", &json!({"path": gap, "variable": "t", "stat": "count"})).unwrap();
    assert_eq!(first(&s, &r), 2.0);
    let r = s.run("zarr.reduce", &json!({"path": gap, "variable": "t", "stat": "mean"})).unwrap();
    assert_eq!(first(&s, &r), 108.0);
    let _ = std::fs::remove_dir_all(gap_dir);
    for bad in [
        json!({"path": path, "variable": "t"}),
        json!({"path": path, "variable": "t", "stat": "median"}),
        json!({"path": path, "variable": "t", "stat": "countAbove"}),
        json!({"path": path, "variable": "t", "stat": "mean", "months": [13]}),
        json!({"path": path, "variable": "t", "stat": "mean", "months": "June"}),
        json!({"path": path, "variable": "t", "stat": "mean", "minValid": 1.5}),
        json!({"path": path, "variable": "t", "stat": "mean", "start": "2005-01-01"}),
        json!({"path": path, "variable": "lat", "stat": "mean"}),
    ] {
        assert!(s.run("zarr.reduce", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A trend over a data cube (#579), from the three kinds of cube: a stack of raster layers dated
/// by their names, a raster whose bands are dates, and a Zarr variable along its time dimension.
/// A cell with a clear trend gets Sen's slope and a significant slope; noise gets a slope but no
/// significant one; a cell with too few values gets nothing.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_trend_over_a_data_cube() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let mut s = Session::new(Project::default());
    let gt = GeoTransform { origin_x: -56.0, origin_y: -12.0, pixel_width: 0.01, pixel_height: -0.01 };
    // Twelve years; cell 0 rises 0.02 a year, cell 1 is noise, cell 2 has three values only.
    let years: Vec<i32> = (2013..2025).collect();
    let noise = [0.03, -0.02, 0.05, -0.04, 0.01, 0.0, -0.05, 0.04, -0.01, 0.02, -0.03, 0.03];
    let cells = |k: usize| -> Vec<f32> {
        vec![0.3 + 0.02 * k as f32 + 0.002 * noise[k] as f32, 0.5 + noise[k] as f32, if k < 3 { 0.4 } else { f32::NAN }, 0.6]
    };
    let mut names = Vec::new();
    for (k, y) in years.iter().enumerate() {
        let name = format!("ndvi_{y}-07-01");
        let r = Raster::new(&name, 2, 2, vec![cells(k)], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let bytes = geotiff::write(&r, 0).unwrap();
        crate::cmd::raster::add_raster(&mut s, &name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
        names.push(name);
    }
    let value = |s: &Session, id: &Value, cell: usize| {
        s.map().layer(id.as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap().bands[0].data[cell]
    };
    let r = s.run("cube.trend", &json!({"layers": names})).unwrap();
    let ids = r["layers"].as_array().unwrap().clone();
    assert_eq!((ids.len(), r["dates"].as_u64()), (4, Some(12)), "{r}");
    let (slope, p, significant) = (&ids[0], &ids[2], &ids[3]);
    assert!((value(&s, slope, 0) - 0.02).abs() < 1e-3, "{}", value(&s, slope, 0));
    assert!(value(&s, p, 0) < 0.01 && (value(&s, significant, 0) - 0.02).abs() < 1e-3);
    assert!(value(&s, slope, 1).is_finite() && value(&s, p, 1) > 0.05 && value(&s, significant, 1).is_nan());
    assert!(value(&s, slope, 2).is_nan() && value(&s, p, 2).is_nan());
    // A constant cell: no slope, and nothing significant.
    assert_eq!(value(&s, slope, 3), 0.0);
    assert!(value(&s, significant, 3).is_nan());
    let name = |id: &Value| s.map().layer(id.as_u64().unwrap() as u32).unwrap().name.clone();
    assert_eq!(name(slope), "ndvi 2013-07-01–2024-07-01 Sen slope per year");
    assert!(name(&ids[1]).ends_with("Z (Hamed-Rao)"), "{}", name(&ids[1]));
    // Per decade, another test, a period, and the dates given.
    let r = s.run("cube.trend", &json!({"layers": names, "per": "decade", "test": "yueWang", "start": "2016", "name": "late"})).unwrap();
    let late = r["layers"][0].clone();
    assert_eq!(r["dates"], 9);
    assert!((value(&s, &late, 0) - 0.2).abs() < 1e-2, "{}", value(&s, &late, 0));
    // An end given as a year takes all of it: 2013 to 2020's July, eight dates (read as 1 January
    // 2020, it would leave 2020's out).
    let r = s.run("cube.trend", &json!({"layers": names, "end": "2020", "name": "to2020"})).unwrap();
    assert_eq!(r["dates"], 8, "{r}");
    let r = s.run("cube.trend", &json!({"layers": names, "end": "2020-06", "name": "toJune"})).unwrap();
    assert_eq!(r["dates"], 7, "a month ends with it: 2020's July date is after June: {r}");
    // A year given as a number reads the same.
    let r = s.run("cube.trend", &json!({"layers": names, "end": 2020, "name": "to2020n"})).unwrap();
    assert_eq!(r["dates"], 8, "{r}");
    // A full date is that day, as before: 1 January 2020 leaves 2020's July out, its 1 July keeps it.
    let r = s.run("cube.trend", &json!({"layers": names, "end": "2020-01-01", "name": "toNewYear"})).unwrap();
    assert_eq!(r["dates"], 7, "{r}");
    let r = s.run("cube.trend", &json!({"layers": names, "end": "2020-07-01", "name": "toJuly"})).unwrap();
    assert_eq!(r["dates"], 8, "{r}");
    let backwards: Vec<String> = (0..12).map(|k| format!("{}-01-01", 2030 - k)).collect();
    let r = s.run("cube.trend", &json!({"layers": names, "dates": backwards, "test": "original"})).unwrap();
    assert!(value(&s, &r["layers"][0], 0) < 0.0, "the dates given are used, in the layers' order");
    // A raster whose bands are the dates, named by them.
    let bands: Vec<Vec<f32>> = (0..12).map(cells).collect();
    let mut cube = Raster::new("stack", 2, 2, bands, gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    for (b, y) in cube.bands.iter_mut().zip(&years) {
        b.name = format!("NDVI {y}");
    }
    let bytes = geotiff::write(&cube, 0).unwrap();
    let id =
        crate::cmd::raster::add_raster(&mut s, "stack", rustgis_map::RasterLayer::from_bytes(cube, "stack.tif", bytes)).unwrap()["layer"]
            .clone();
    let r = s.run("cube.trend", &json!({"layer": id})).unwrap();
    assert!((value(&s, &r["layers"][0], 0) - 0.02).abs() < 1e-3);
    // Two bands on one date are refused, as two layers are.
    let same: Vec<String> = (0..12).map(|k| format!("{}-06-01", 2000 + k.max(1))).collect();
    let e = s.run("cube.trend", &json!({"layer": id, "dates": same})).unwrap_err();
    assert!(e.contains("same date"), "{e}");
    // A Zarr variable: every cell rises 100 a step.
    let (dir, path) = zarr_store_at("trend", &[0, 31, 59, 90, 120, 151, 181, 212]);
    let r = s.run("cube.trend", &json!({"path": path, "variable": "t", "test": "seasonal", "seasons": 4})).unwrap();
    assert_eq!(r["dates"], 8);
    assert!(value(&s, &r["layers"][0], 0) > 1000.0, "{}", value(&s, &r["layers"][0], 0));
    let _ = std::fs::remove_dir_all(dir);
    // A step never written (its chunk absent) has no value; the trend goes on with the others.
    let (gap_dir, gap) = zarr_store_at("trend-gap", &[0, 31, 59, 90, 120, 151, 181, 212]);
    std::fs::remove_file(std::path::Path::new(&gap).join("t/3.0.0")).unwrap();
    let r = s.run("cube.trend", &json!({"path": gap, "variable": "t", "test": "original"})).unwrap();
    assert!(value(&s, &r["layers"][0], 0) > 1000.0, "{}", value(&s, &r["layers"][0], 0));
    let _ = std::fs::remove_dir_all(gap_dir);
    // Refused: too few layers, no dates, dates of another length, unknown options, another grid.
    let other = Raster::new("ndvi_2030-01-01", 3, 3, vec![vec![0.0; 9]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let bytes = geotiff::write(&other, 0).unwrap();
    crate::cmd::raster::add_raster(&mut s, "ndvi_2030-01-01", rustgis_map::RasterLayer::from_bytes(other, "o.tif", bytes)).unwrap();
    let mut mixed = names.clone();
    mixed.push("ndvi_2030-01-01".into());
    for bad in [
        json!({"layers": &names[..3]}),
        json!({"layers": names, "dates": ["2001", "2002"]}),
        json!({"layers": names, "test": "ols"}),
        json!({"layers": names, "alpha": 0.7}),
        json!({"layers": names, "per": "week"}),
        json!({"layers": names, "start": "2030"}),
        json!({"layers": mixed}),
        json!({"layer": names[0]}),
    ] {
        assert!(s.run("cube.trend", &bad).is_err(), "{bad}");
    }
    let undated = Raster::new("plain", 2, 2, vec![vec![0.0; 4]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let bytes = geotiff::write(&undated, 0).unwrap();
    crate::cmd::raster::add_raster(&mut s, "plain", rustgis_map::RasterLayer::from_bytes(undated, "plain.tif", bytes)).unwrap();
    let mut no_date = names[..4].to_vec();
    no_date[1] = "plain".into();
    let e = s.run("cube.trend", &json!({"layers": no_date})).unwrap_err();
    assert!(e.contains("no date in the name of plain"), "{e}");
    // A run the app no longer keeps is said, never worked out again here.
    let e = s.run("cube.trend", &json!({"ran": 987_654_321u64})).unwrap_err();
    assert!(e.contains("no longer kept"), "{e}");
    // A run kept by one session is that session's: another one (Python, tests side by side)
    // doesn't take it, and it is added once.
    let kept = Raster::new("kept", 2, 2, vec![vec![0.0; 4]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let ran = crate::cmd::cube::land(&mut s, vec![kept], 12);
    let mut other = Session::new(Project::default());
    assert!(other.run("cube.trend", &json!({"ran": ran})).unwrap_err().contains("no longer kept"));
    assert_eq!(s.run("cube.trend", &json!({"ran": ran})).unwrap()["dates"], 12);
    assert!(s.run("cube.trend", &json!({"ran": ran})).is_err());
    let e = s.run("cube.trend", &json!({"layers": names, "minValid": 99})).unwrap_err();
    assert!(e.contains("more than the"), "{e}");
    // Two layers on one day would count twice in the test: refused.
    let mut twice: Vec<String> = (0..names.len()).map(|k| format!("{}-06-01", 2010 + k)).collect();
    twice[1] = twice[0].clone();
    let e = s.run("cube.trend", &json!({"layers": names, "dates": twice})).unwrap_err();
    assert!(e.contains("same date"), "{e}");
    // Seasons from the calendar: the 1st of every month is its month in every year, leap years
    // included, and a 16-day composite on the same day each year keeps its season.
    use crate::cmd::cube::season_of;
    let day = |d: &str| rustgis_data::time::parse_text(d).unwrap();
    for y in 2000..2010 {
        for m in 1..=12 {
            assert_eq!(season_of(day(&format!("{y}-{m:02}-01")), 12), m - 1, "{y}-{m}");
        }
        assert_eq!(season_of(day(&format!("{y}-01-17")), 23), 1, "{y}");
        assert_eq!(season_of(day(&format!("{y}-12-19")), 23), 22, "{y}");
    }
    // A monthly cube dated on the 1st, with a yearly cycle and a rise: the seasonal test finds it
    // per year and per decade alike.
    let mut monthly = Vec::new();
    for y in 2015..2021 {
        for m in 1..=12 {
            let k = (y - 2015) * 12 + (m - 1);
            let v = 0.5 + 0.2 * (std::f32::consts::TAU * (m - 1) as f32 / 12.0).sin() + 0.01 * k as f32 / 12.0;
            let name = format!("evi_{y}-{m:02}-01");
            let r = Raster::new(&name, 1, 1, vec![vec![v]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
            let bytes = geotiff::write(&r, 0).unwrap();
            crate::cmd::raster::add_raster(&mut s, &name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
            monthly.push(name);
        }
    }
    for (per, want) in [("year", 0.01), ("decade", 0.1)] {
        let r = s.run("cube.trend", &json!({"layers": monthly, "test": "seasonal", "per": per})).unwrap();
        let slope = value(&s, &r["layers"][0], 0);
        assert!((slope - want).abs() < want * 0.05, "{per}: {slope}");
        assert!(value(&s, &r["layers"][2], 0) < 0.01, "{per}: significant");
    }
}

/// A Zarr variable follows the time slider (#554): the slice drawn is the step the slider is at,
/// the stretch is the same at every step, and the project keeps the layer by its store.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_zarr_variable_follows_the_time_slider() {
    let (dir, path) = zarr_store_over_time("follow");
    let mut s = Session::new(Project::default());
    let id = s.run("layer.addZarr", &json!({"path": path, "variable": "t", "time": true})).unwrap()["layer"].as_u64().unwrap() as u32;
    // The slider is on over the variable's three dates, at the first.
    let info = s.run("time.info", &Value::Null).unwrap();
    assert_eq!(info["steps"], 3, "{info}");
    let first = |s: &Session| s.map().layer(id).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.as_ref()).map(|d| d.bands[0].data[0]);
    // North up: the last latitude row first; step k's values are 100·k + i.
    assert_eq!(first(&s), Some(8.0));
    let rl = s.map().layer(id).unwrap().raster.clone().unwrap();
    assert!(matches!(rl.style.stretch, rustgis_map::raster::Stretch::Custom { .. }), "{:?}", rl.style.stretch);
    assert!(s.map().layer(id).unwrap().name.ends_with(" t"), "{}", s.map().layer(id).unwrap().name);
    let stretch = rl.style.stretch;
    // Moving the slider moves the slice; the stretch stays.
    s.run("time.goTo", &json!({"step": 2})).unwrap();
    assert_eq!(first(&s), Some(208.0));
    s.run("time.step", &json!({"by": -1})).unwrap();
    assert_eq!(first(&s), Some(108.0));
    assert_eq!(s.map().layer(id).unwrap().raster.as_ref().unwrap().style.stretch, stretch);
    // Saved and opened again: the store and the step, read again.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("\"time_zarr\"") && !text.contains("\"embedded\""), "kept by its store, not as a slice");
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t
        .map()
        .layers
        .iter()
        .find(|l| l.is_raster())
        .and_then(|l| l.raster.as_ref())
        .and_then(|r| r.data.as_ref())
        .map(|d| d.bands[0].data[0]);
    assert_eq!(again, Some(108.0));
    // A window with no step of the variable: nothing drawn.
    s.run("time.set", &json!({"dates": ["2000-06-01", "2001-01-01", "2001-02-01", "2001-03-01"], "date": "2000-06-01"})).unwrap();
    assert_eq!(first(&s), None);
    s.run("time.goTo", &json!({"step": 3})).unwrap();
    assert_eq!(first(&s), Some(208.0));
    // The animation reads each step's slice.
    let zip = std::env::temp_dir().join(format!("rustgis-zarr-frames-{}.zip", std::process::id()));
    let r = s.run("time.export", &json!({"path": zip.display().to_string(), "width": 64, "height": 48})).unwrap();
    assert_eq!(r["frames"], 4, "{r}");
    // After the export the slider is back at its step, and the layer shows it.
    assert_eq!(first(&s), Some(208.0), "the layer shows the step the slider is back at");
    let _ = std::fs::remove_file(zip);
    // A period: the steps from February on, the layer's first step the second of the axis.
    let id2 = s.run("layer.addZarr", &json!({"path": path, "variable": "t", "time": true, "start": "2001-01-20", "name": "late"})).unwrap()
        ["layer"]
        .as_u64()
        .unwrap() as u32;
    let zt = s.map().layer(id2).unwrap().raster.as_ref().unwrap().time_zarr.clone().unwrap();
    assert_eq!((zt.offset, zt.dates.len(), zt.dim.as_str()), (1, 2, "time"));
    for bad in [
        json!({"path": path, "variable": "t", "time": true, "start": "2002-01-01"}),
        json!({"path": path, "variable": "t", "time": true, "start": "June"}),
        json!({"path": path, "variable": "t", "time": true, "ran": 1}),
        json!({"path": path, "variable": "lat", "time": true}),
    ] {
        assert!(s.run("layer.addZarr", &bad).is_err(), "{bad}");
    }
    // An axis with a date twice, or going back: each date is a step, so it can't be followed.
    let (dir2, twice) = zarr_store_over_time("twice");
    std::fs::write(std::path::Path::new(&twice).join("time/0"), [0i32, 31, 31].iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
        .unwrap();
    let e = s.run("layer.addZarr", &json!({"path": twice, "variable": "t", "time": true})).unwrap_err();
    assert!(e.contains("increasing order"), "{e}");
    // With no variable named, a folder's first one follows the slider.
    let mut plain = Session::new(Project::default());
    let r = plain.run("layer.addZarr", &json!({"path": path, "time": true})).unwrap();
    assert!(plain.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().time_zarr.is_some());
    // A store whose first slice can't be read leaves the slider as it was.
    let (dir3, broken) = zarr_store_over_time("broken");
    std::fs::remove_file(std::path::Path::new(&broken).join("t/0.0.0")).unwrap();
    std::fs::write(std::path::Path::new(&broken).join("t/0.0.0"), b"short").unwrap();
    let mut fresh = Session::new(Project::default());
    assert!(fresh.run("layer.addZarr", &json!({"path": broken, "variable": "t", "time": true})).is_err());
    assert!(fresh.map().time.is_none(), "a store that can't be read leaves the slider as it was");
    let _ = std::fs::remove_dir_all(dir3);
    let _ = std::fs::remove_dir_all(dir2);
    let _ = std::fs::remove_dir_all(dir);
}

/// A Zarr store in a folder: its variables listed (with time labels), a slice added by command
/// and by Add Data, saved by folder and selection and read again when the project opens; a slice
/// the app read itself handed over by id.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn zarr_stores_in_folders_and_handed_over() {
    let dir = std::env::temp_dir().join(format!("rustgis-zarr-{}", std::process::id()));
    let store = dir.join("air.zarr");
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["t", "lat", "lon", "time"] {
        std::fs::create_dir_all(store.join(d)).unwrap();
    }
    let arr = |shape: &str, chunks: &str, dtype: &str| {
        serde_json::from_str::<Value>(&format!(
            r#"{{"shape":{shape},"chunks":{chunks},"dtype":"{dtype}","compressor":null,"fill_value":null,"filters":null,"order":"C","zarr_format":2}}"#
        ))
        .unwrap()
    };
    let meta = json!({"metadata": {
        ".zgroup": {"zarr_format": 2},
        "t/.zarray": arr("[2,3,4]", "[1,3,4]", "<f4"),
        "t/.zattrs": {"_ARRAY_DIMENSIONS": ["time","lat","lon"], "units": "K"},
        "lat/.zarray": arr("[3]", "[3]", "<f4"),
        "lat/.zattrs": {"_ARRAY_DIMENSIONS": ["lat"], "units": "degrees_north"},
        "lon/.zarray": arr("[4]", "[4]", "<f4"),
        "lon/.zattrs": {"_ARRAY_DIMENSIONS": ["lon"], "units": "degrees_east"},
        "time/.zarray": arr("[2]", "[2]", "<i4"),
        "time/.zattrs": {"_ARRAY_DIMENSIONS": ["time"], "units": "days since 2001-01-01"},
    }, "zarr_consolidated_format": 1});
    std::fs::write(store.join(".zmetadata"), meta.to_string()).unwrap();
    std::fs::write(store.join(".zgroup"), r#"{"zarr_format":2}"#).unwrap();
    let f32s = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    std::fs::write(store.join("lat/0"), f32s(&[10.0, 20.0, 30.0])).unwrap();
    std::fs::write(store.join("lon/0"), f32s(&[1.0, 2.0, 3.0, 4.0])).unwrap();
    std::fs::write(store.join("time/0"), [0i32, 31].iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    for t in 0..2 {
        let v: Vec<f32> = (0..12).map(|i| (t * 100 + i) as f32).collect();
        std::fs::write(store.join(format!("t/{t}.0.0")), f32s(&v)).unwrap();
    }
    let path = store.display().to_string();
    let mut s = Session::new(Project::default());
    let info = s.run("raster.zarrInfo", &json!({"path": path})).unwrap();
    assert_eq!(info["count"], 1);
    assert_eq!(info["variables"][0]["dims"][0]["labels"], json!(["2001-01-01", "2001-02-01"]));
    let r = s.run("layer.addZarr", &json!({"path": path, "variable": "t", "index": {"time": 1}})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(4), Some(3)), "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    let ras = s.map().layer(id).unwrap().raster.clone().unwrap();
    assert_eq!(ras.subdataset.as_deref(), Some("t[time=1]"));
    let data = ras.data.clone().unwrap();
    // North up: the last latitude row (30°N) first.
    assert_eq!(data.bands[0].data[..4], [108.0, 109.0, 110.0, 111.0]);
    // The project names the folder and the slice, and reads it again.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layers.iter().find(|l| l.is_raster()).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone()).unwrap();
    assert_eq!(again.bands[0].data[..4], [108.0, 109.0, 110.0, 111.0]);
    // Add Data on the folder: the first variable, first step.
    let r = s.run("layer.addData", &json!({"path": path})).unwrap();
    let first = s.map().layer(r["layer"].as_u64().unwrap() as u32).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone()).unwrap();
    assert_eq!(first.bands[0].data[0], 8.0);
    // A slice the app read: its GeoTIFF handed over by id, named by its store and selection.
    let tif = rustgis_data::raster::geotiff::write(&again, 0).unwrap();
    let id = s.download_started();
    assert!(s.download_landed(id, tif));
    let r = s
        .run("layer.addZarr", &json!({"url": "https://example.org/data/air.zarr", "variable": "t", "index": {"time": 1}, "ran": id}))
        .unwrap();
    let l = s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap();
    assert!(l.name.starts_with("air t[time=1]"), "{}", l.name);
    assert!(s.run("layer.addZarr", &json!({"url": "https://example.org/a.zarr", "ran": id})).is_err(), "used once");
    for bad in [
        json!({}),
        json!({"path": path, "variable": "nope"}),
        json!({"path": path, "variable": "t", "index": {"time": 9}}),
        json!({"path": "/nowhere/x.zarr"}),
    ] {
        assert!(s.run("layer.addZarr", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A stand-in S3 endpoint: answers a listing, an object and a redirect, and keeps each request's
/// head (path and headers) for the test to look at.
fn serve_s3() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 8192];
            let read = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock().unwrap().push(req);
            let (status, body): (&str, &[u8]) = if path.starts_with("/s3-test-bucket?list-type=2") {
                ("200 OK", br#"<ListBucketResult><Name>s3-test-bucket</Name><IsTruncated>false</IsTruncated><Contents><Key>data/places.geojson</Key><Size>120</Size><LastModified>2024-01-01T00:00:00.000Z</LastModified></Contents><CommonPrefixes><Prefix>data/more/</Prefix></CommonPrefixes></ListBucketResult>"#)
            } else if path == "/" {
                (
                    "200 OK",
                    br#"<ListAllMyBucketsResult><Buckets><Bucket><Name>s3-test-bucket</Name></Bucket></Buckets></ListAllMyBucketsResult>"#,
                )
            } else if path.starts_with("/s3-test-bucket/data/places.geojson") {
                ("200 OK", br#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"n": 1}, "geometry": {"type": "Point", "coordinates": [10, 20]}}]}"#)
            } else if path.starts_with("/s3-test-bucket/moved.geojson") {
                ("301 Moved Permanently\r\nLocation: http://127.0.0.1:9/elsewhere.geojson", b"")
            } else {
                ("404 Not Found", b"<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>")
            };
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(body);
        }
    });
    (base, seen)
}

/// S3 through a connection with credentials: listings and objects by s3:// address, signed for
/// the connection's endpoint only; the secret never in a result; a signed request isn't
/// redirected.
#[test]
fn s3_connections_listings_and_objects() {
    let (base, seen) = serve_s3();
    let mut s = Session::new(Project::default());
    let r = s
        .run(
            "s3.connect",
            &json!({"name": "s3-test", "endpoint": base, "buckets": ["s3-test-bucket"], "accessKeyId": "AKIDTEST", "secretAccessKey": "very-secret-key", "sessionToken": "token-1"}),
        )
        .unwrap();
    assert_eq!((r["signed"].as_bool(), r["credentials"].as_str()), (Some(true), Some("keys")));
    assert!(!r.to_string().contains("very-secret-key"));
    let listed = s.run("s3.connections", &Value::Null).unwrap();
    assert!(listed.to_string().contains("s3-test") && !listed.to_string().contains("very-secret-key"));
    // A blank form is anonymous: never the environment's or a profile's keys unless asked for.
    let r = s.run("s3.connect", &json!({"name": "s3-blank", "region": ""})).unwrap();
    assert_eq!(
        (r["signed"].as_bool(), r["credentials"].as_str(), r["region"].as_str()),
        (Some(false), Some("anonymous"), Some("us-east-1"))
    );
    s.run("s3.disconnect", &json!({"name": "s3-blank"})).unwrap();
    // A level of the bucket, and the connection's buckets; listing isn't an edit of the project.
    assert!(!s.dirty && !s.can_undo());
    let r = s.run("s3.list", &json!({"url": "s3://s3-test-bucket/data/"})).unwrap();
    assert!(!s.dirty && !s.can_undo(), "a listing is no edit");
    assert_eq!(r["prefixes"], json!(["data/more/"]));
    assert_eq!(r["objects"][0]["url"], "s3://s3-test-bucket/data/places.geojson");
    let r = s.run("s3.list", &json!({"connection": "s3-test"})).unwrap();
    assert_eq!(r["buckets"], json!(["s3-test-bucket"]));
    // An object by its s3:// address, signed with the connection's key (never its secret).
    let r = s.run("layer.addUrl", &json!({"url": "s3://s3-test-bucket/data/places.geojson"})).unwrap();
    assert_eq!(r["features"], 1);
    {
        let heads = seen.lock().unwrap();
        let get = heads.iter().find(|h| h.contains("/s3-test-bucket/data/places.geojson")).unwrap().to_ascii_lowercase();
        assert!(get.contains("authorization: aws4-hmac-sha256 credential=akidtest/"), "{get}");
        assert!(get.contains("x-amz-security-token: token-1") && get.contains("x-amz-content-sha256: unsigned-payload"));
        assert!(heads.iter().all(|h| !h.contains("very-secret-key")));
    }
    // A redirect isn't followed by a signed request; a missing key says so.
    let e = s.run("layer.addUrl", &json!({"url": "s3://s3-test-bucket/moved.geojson"})).unwrap_err();
    assert!(e.contains("redirect"), "{e}");
    assert!(s.run("layer.addUrl", &json!({"url": "s3://s3-test-bucket/nope.geojson"})).unwrap_err().contains("404"));
    // Bad parameters; credentials from this computer don't go to another endpoint.
    for bad in [
        json!({"name": "x", "endpoint": "ftp://h"}),
        json!({"name": "x", "accessKeyId": "A"}),
        json!({"name": "x", "region": "no such"}),
        json!({"name": "x", "buckets": ["NO_"]}),
        json!({"name": "x", "endpoint": "https://elsewhere.example", "profile": "default"}),
    ] {
        assert!(s.run("s3.connect", &bad).is_err(), "{bad}");
    }
    for bad in [json!({"url": "s3://"}), json!({"bucket": "NO_"}), json!({"connection": "nope"}), json!({"url": "https://x.org"})] {
        assert!(s.run("s3.list", &bad).is_err(), "{bad}");
    }
    s.run("s3.disconnect", &json!({"name": "s3-test"})).unwrap();
    assert!(s.run("s3.disconnect", &json!({"name": "s3-test"})).is_err());
    // Plugins never connect (they'd choose where the user's credentials go).
    assert!(crate::cmd::plugins::user_only("s3.connect"));
}

/// A streamed layer outside its zoom range reads nothing for the view, as if hidden: it is not
/// drawn there, so its features are not fetched (#412). Back in range, it reads again.
#[test]
fn streamed_layer_outside_its_zoom_range_reads_nothing() {
    use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
    let mut ds = Dataset::new("grid", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..200u32 {
        for j in 0..200u32 {
            let c = rustgis_geom::Coord::new(-179.0 + 1.79 * f64::from(i), -80.0 + 0.8 * f64::from(j));
            let n = u64::from(i * 200 + j);
            ds.features.push(Feature { id: n + 1, geometry: Some(Geometry::Point(c)), attrs: vec![Value::Int(n as i64)] });
        }
    }
    let (base, sent) = serve_ranges(rustgis_data::source::fgb::write(&ds).unwrap());
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/grid.fgb")})).unwrap();
    assert_eq!(r["streamed"], true, "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    // Drawn only when zoomed in beyond 1:10,000,000.
    s.run("layer.scaleRange", &json!({"layer": id, "min": 1e7})).unwrap();
    let before = sent.load(std::sync::atomic::Ordering::Relaxed);
    // Zoomed out (an export, which waits for its reads): not drawn, so not read.
    let v = view_of(&mut s, rustgis_geom::Extent::new(-60.0, -40.0, 60.0, 40.0));
    assert!(s.map().scale() > 1e7);
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let l = s.map().layer(id).unwrap();
    assert!(l.dataset.features.is_empty() && l.source.as_ref().unwrap().window().is_none());
    assert_eq!(sent.load(std::sync::atomic::Ordering::Relaxed), before);
    // Zoomed in, inside the range: read and drawn.
    let v = view_of(&mut s, rustgis_geom::Extent::new(10.0, 10.0, 12.0, 11.0));
    assert!(s.map().scale() < 1e7);
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let l = s.map().layer(id).unwrap();
    assert!(!l.dataset.features.is_empty(), "{:?}", l.source.as_ref().unwrap().error);
    assert!(sent.load(std::sync::atomic::Ordering::Relaxed) > before);
}

/// A FlatGeobuf and a GeoParquet by their address are read in place, by range: a view fetches a
/// small part of the file; the project keeps the address and reads it again; asked to, a file is
/// downloaded whole instead.
#[test]
fn flatgeobuf_and_geoparquet_by_address_are_read_in_place() {
    use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
    let mut ds = Dataset::new("grid", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..200u32 {
        for j in 0..200u32 {
            let c = rustgis_geom::Coord::new(-179.0 + 1.79 * f64::from(i), -80.0 + 0.8 * f64::from(j));
            let n = u64::from(i * 200 + j);
            ds.features.push(Feature { id: n + 1, geometry: Some(Geometry::Point(c)), attrs: vec![Value::Int(n as i64)] });
        }
    }
    let fgb = rustgis_data::source::fgb::write(&ds).unwrap();
    let total = fgb.len();
    let (base, sent) = serve_ranges(fgb.clone());
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/grid.fgb")})).unwrap();
    assert_eq!((r["inPlace"].as_bool(), r["streamed"].as_bool(), r["features"].as_u64()), (Some(true), Some(true), Some(40_000)), "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    // A view: read as the screen reads, in the background.
    let view = rustgis_geom::Extent::new(10.0, 10.0, 14.0, 13.0);
    let sync = |s: &mut Session| {
        let scale = s.map().scale();
        let mut pending = s.map_mut().sync_sources(&view, scale, false);
        let start = std::time::Instant::now();
        while pending && start.elapsed() < std::time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            pending = s.map_mut().sync_sources(&view, scale, false);
        }
        assert!(!pending);
    };
    sync(&mut s);
    let l = s.map().layer(id).unwrap();
    let w = l.source.as_ref().unwrap().window().unwrap().extent;
    assert!(!l.dataset.features.is_empty(), "{:?}", l.source.as_ref().unwrap().error);
    assert!(l.dataset.features.iter().all(|f| w.contains(f.geometry.as_ref().unwrap().extent().center())));
    let read = sent.load(std::sync::atomic::Ordering::Relaxed);
    assert!(read < total / 4, "{read} of {total} bytes read");
    // The project keeps the address, and reads it again.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.contains("grid.fgb") && !text.contains("embedded"));
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    sync(&mut t);
    let again = t.map().layers.iter().find(|l| l.is_streamed()).unwrap();
    assert!(!again.dataset.features.is_empty(), "{:?}", again.source.as_ref().unwrap().error);
    // Load All Features: every feature, fetched.
    assert_eq!(again.load_all().unwrap().features.len(), 40_000);
    // A project layer whose file can't be read in place (here a server that ignores ranges; a
    // GeoParquet without a bbox covering alike) is downloaded whole, not left empty, and keeps its
    // symbology; the project still keeps the address.
    let (plain, _) = serve_file(fgb.clone(), false);
    let mut w = Session::new(Project::default());
    w.run("project.openText", &json!({"text": text.replace(&base, &plain)})).unwrap();
    let renderer = w.map().layers.iter().find(|l| l.is_streamed()).unwrap().renderer.clone();
    sync(&mut w);
    let whole = w.map().layers.iter().find(|l| l.is_streamed()).unwrap();
    assert!(!whole.dataset.features.is_empty(), "{:?}", whole.source.as_ref().unwrap().error);
    assert_eq!(whole.renderer, renderer);
    assert!(serde_json::to_string(&w.project).unwrap().contains(&plain));
    // A project written by another app may list no fields for a streamed layer: it gets the
    // source's, so its features keep their attributes (and a renderer on a field finds it).
    let mut bare: serde_json::Value = serde_json::from_str(&text).unwrap();
    for l in bare["maps"][0]["layers"].as_array_mut().unwrap() {
        l["dataset"]["fields"] = json!([]);
    }
    let mut v = Session::new(Project::default());
    v.run("project.openText", &json!({"text": bare.to_string()})).unwrap();
    // Load All Features before the layer has drawn keeps them too.
    let all = v.map().layers.iter().find(|l| l.is_streamed()).unwrap().load_all().unwrap();
    assert_eq!(all.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["n"]);
    sync(&mut v);
    let l = v.map().layers.iter().find(|l| l.is_streamed()).unwrap();
    assert_eq!(l.dataset.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["n"]);
    // The values themselves, not nulls padded in for a missing field.
    assert!(!l.dataset.features.is_empty());
    assert!(l.dataset.features.iter().all(|f| f.attrs.len() == 1 && matches!(f.attrs[0], Value::Int(_))));
    // Copies of a layer still opening (undo snapshots, Load All Features) all open: they share
    // what opening brought, none takes it from the others.
    // The server holds its answers, so the copies are made while the header is still on its way.
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (other, _) = serve_gated(fgb.clone(), true, Some(gate.clone()));
    let mut u = Session::new(Project::default());
    u.run("project.openText", &json!({"text": text.replace(&base, &other)})).unwrap();
    let mut a = u.map().layers.iter().find(|l| l.is_streamed()).unwrap().source.clone().unwrap();
    a.open().unwrap();
    assert!(!a.is_open() && a.is_pending(), "opened before its header was fetched");
    let mut b = a.clone();
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    a.wait_open(std::time::Duration::from_secs(60));
    b.wait_open(std::time::Duration::from_secs(60));
    assert!(a.is_open() && b.is_open(), "{:?} {:?}", a.error, b.error);

    // A GeoParquet (with its bbox covering), from its footer at the end.
    let pq = rustgis_data::source::parquet::write(&ds).unwrap();
    let (base, _) = serve_ranges(pq);
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/grid.parquet"), "name": "PQ"})).unwrap();
    assert_eq!(r["inPlace"].as_bool(), Some(true), "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    sync(&mut s);
    let l = s.map().layer(id).unwrap();
    assert!(!l.dataset.features.is_empty(), "{:?}", l.source.as_ref().unwrap().error);
    assert_eq!(l.name, "PQ");

    // Asked to: downloaded whole.
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/grid.parquet"), "whole": true, "name": "Whole"})).unwrap();
    assert!(r.get("inPlace").is_none(), "{r}");
    // A server that ignores ranges answers the footer's request with the whole, longer file:
    // downloaded whole instead, not an error.
    let pq = rustgis_data::source::parquet::write(&ds).unwrap();
    assert!(pq.len() > 256 << 10);
    let (base, _) = serve_file(pq, false);
    let r = s.run("layer.addUrl", &json!({"url": format!("{base}/grid.parquet"), "name": "NoRanges"})).unwrap();
    assert!(r.get("inPlace").is_none(), "{r}");
    assert!(s.map().layer_by_name("NoRanges").is_some(), "{r}");
    // A file that isn't one: an error, not a hang.
    let (base, _) = serve_ranges(b"not a flatgeobuf at all".to_vec());
    assert!(s.run("layer.addUrl", &json!({"url": format!("{base}/x.fgb")})).is_err());
}

/// Data sources: what was added lands in Recent (once, newest first); favourites and saved
/// sources are kept and removed by command; nothing kept holds a secret; the library lists
/// well-known public sources.
#[test]
fn data_sources_recent_favourites_and_saved() {
    let dir = std::env::temp_dir().join(format!("rustgis-sources-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("towns.geojson");
    std::fs::write(
        &path,
        r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
    )
    .unwrap();
    let mut s = Session::new(Project::default());
    let p = path.display().to_string();
    s.run("layer.addData", &json!({"path": p})).unwrap();
    s.run("layer.addSample", &json!({"name": "cities"})).unwrap();
    // A failed add isn't a source.
    assert!(s.run("layer.addData", &json!({"path": dir.join("missing.geojson").display().to_string()})).is_err());
    let l = s.run("sources.list", &Value::Null).unwrap();
    let recent: Vec<&str> = l["recent"].as_array().unwrap().iter().filter_map(|r| r["address"].as_str()).collect();
    assert_eq!(recent, ["cities", p.as_str()]);
    assert_eq!(l["recent"][1]["name"], "towns");
    assert_eq!(l["recent"][1]["addWith"], "layer.addData {path}");
    assert!(l["library"].as_array().unwrap().iter().any(|x| x["group"] == "Catalogs" && x["kind"] == "stac"));
    // Favourites and saved services, kept without their secrets.
    s.run("sources.favourite", &json!({"kind": "file", "address": p})).unwrap();
    let r = s.run("sources.save", &json!({"kind": "service", "address": "https://me:pw@gis.example/wms?token=abc&map=m", "name": "My WMS", "extra": {"type": "wms", "password": "x"}})).unwrap();
    assert_eq!(r["address"], "https://gis.example/wms?map=m");
    s.run("sources.save", &json!({"kind": "database", "address": "postgres:host=h dbname=g user=u password=secret"})).unwrap();
    let text = s.sources.to_json();
    for secret in ["pw", "abc", "secret", "\"x\""] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    // The app keeps them in its preferences and reads them back.
    let back = rustgis_engine_sources_roundtrip(&text);
    assert_eq!((back.favourites.len(), back.saved.len(), back.recent.len()), (1, 2, 2));
    // Removed by list, kind and address; an unknown one is an error.
    s.run("sources.favourite", &json!({"kind": "file", "address": p, "on": false})).unwrap();
    assert!(s.sources.favourites.is_empty());
    s.run("sources.remove", &json!({"list": "saved", "kind": "service", "address": "https://gis.example/wms?map=m"})).unwrap();
    assert_eq!(s.sources.saved.len(), 1);
    for bad in [
        json!({"list": "saved", "kind": "service", "address": "https://nowhere"}),
        json!({"list": "elsewhere", "kind": "file", "address": "x"}),
        json!({"kind": "nope", "address": "x"}),
    ] {
        assert!(s.run("sources.remove", &bad).is_err(), "{bad}");
    }
    assert!(s.run("sources.save", &json!({"kind": "nope", "address": "x"})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connections_commands() {
    use crate::render::rustgis_tiles::s3;
    let mut s = Session::new(Project::default());
    let a = s.run("connection.new", &json!({"kind":"wms","address":"https://a.example/wms?token=t","name":"A"})).unwrap();
    assert_eq!(a["stripped"], true);
    assert_eq!(a["address"], "https://a.example/wms");
    s.run("connection.new", &json!({"kind":"wms","address":"https://a.example/wms","name":"B"})).unwrap();
    assert!(s.run("connection.new", &json!({"kind":"wms","address":"https://b.example/wms","name":"a"})).is_err());
    s.run("connection.edit", &json!({"id":"A","name":"C","group":"Work"})).unwrap();
    let listed = s.run("connection.list", &json!({"group":"work"})).unwrap();
    assert_eq!(listed["connections"][0]["id"], a["id"]);
    assert_eq!(listed["connections"][0]["name"], "C");
    let copy = s.run("connection.duplicate", &json!({"id":"C"})).unwrap();
    assert_eq!(copy["name"], "C (2)");
    s.run("connection.remove", &json!({"id":copy["id"]})).unwrap();
    assert!(s.connections.find(copy["id"].as_str().unwrap()).is_none());
    assert!(s.run("connection.edit", &json!({"id":"C","kind":"wfs"})).is_err());
    assert!(s.run("connection.edit", &json!({"id":"C","name":"b"})).is_err());
    assert!(s.run("connection.new", &json!({"kind":"s3","options":{"region":[1]}})).is_err());
    #[cfg(not(target_arch = "wasm32"))]
    {
        let db = s
            .run(
                "connection.new",
                &json!({"kind":"postgres","address":"postgres:host=127.0.0.1 port=9 dbname=connections_commands user=u"}),
            )
            .unwrap();
        s.run("connection.login", &json!({"id":db["id"],"password":"pw"})).unwrap();
        let list = s.run("connection.list", &json!({"kind":"postgres"})).unwrap();
        assert_eq!(list["connections"][0]["loggedIn"], true);
        assert!(!s.connections.to_json().contains("pw"));
        let copy = s.run("connection.duplicate", &json!({"id":db["id"]})).unwrap();
        assert_eq!(copy["loggedIn"], true, "the existing password store is shared by public DSN");
        let edited = s
            .run("connection.edit", &json!({"id":db["id"],"address":"postgres:host=127.0.0.1 port=9 dbname=connections_other user=u"}))
            .unwrap();
        assert_eq!(edited["loggedIn"], false, "a different endpoint must not inherit the old password");
        s.run("connection.remove", &json!({"id":db["id"]})).unwrap();
        let list = s.run("connection.list", &json!({"kind":"postgres"})).unwrap();
        assert_eq!(list["connections"][0]["id"], copy["id"]);
        assert_eq!(list["connections"][0]["loggedIn"], true, "removing a shortcut must not log out another connection using that DSN");
    }
    s.run("connection.new", &json!({"kind":"s3","name":"conn-test-s3","options":{"credentials":"keys","buckets":"b11"}})).unwrap();
    assert!(s.run("connection.test", &json!({"id":"conn-test-s3"})).unwrap_err().contains("log in first"));
    s.run("connection.login", &json!({"id":"conn-test-s3","accessKeyId":"AKIDTEST","secretAccessKey":"very-secret-key"})).unwrap();
    assert!(s3::connection("conn-test-s3").is_some());
    assert!(!s.connections.to_json().contains("very-secret"));
    s.run("connection.edit", &json!({"id":"conn-test-s3","address":"https://other.example"})).unwrap();
    assert!(s3::connection("conn-test-s3").is_none());
    s.run("connection.new", &json!({"kind":"wms","name":"Closed","address":"http://127.0.0.1:9/wms"})).unwrap();
    assert!(s.run("connection.test", &json!({"id":"Closed"})).unwrap_err().contains("127.0.0.1:9"));
    assert!(s.run("connection.login", &json!({"id":"Closed"})).unwrap_err().contains("don't log in"));
    s.run("connection.edit", &json!({"id":"Closed","kind":" wms "})).unwrap();
    let default = s.run("connection.new", &json!({"kind":"wms","address":"https://a.example/wms"})).unwrap();
    let renamed = s.run("connection.edit", &json!({"id":"B","name":""})).unwrap();
    assert_eq!(renamed["name"], "wms (2)");
    assert_ne!(default["id"], renamed["id"]);
    assert!(crate::sources::holds_secret(&json!({"id":"x","password":"p"})));
    for id in [
        "connection.new",
        "connection.edit",
        "connection.duplicate",
        "connection.remove",
        "connection.login",
        "connection.test",
        "connection.browse",
    ] {
        assert!(crate::cmd::plugins::user_only(id), "{id}");
    }
    assert!(!crate::cmd::plugins::user_only("connection.list"));
    assert!(!s.dirty && !s.can_undo());
}

#[test]
fn connections_loaded_names_route_login_and_remove_to_the_right_endpoint() {
    use crate::render::rustgis_tiles::s3;
    let mut s = Session::new(Project::default());
    s.connections = crate::connections::Connections::from_json(&json!({"connections":[
        {"id":"c1","kind":"s3","name":"connections-alias-target","options":{"credentials":"keys"}},
        {"id":"connections-alias-target","kind":"postgres","name":"Database","address":"postgres:host=127.0.0.1 port=9 dbname=connections_alias user=u"}
    ]}).to_string());
    let name = s.connections.find("c1").unwrap().name.clone();
    assert_eq!(name, "connections-alias-target (2)");
    let logged_in = s.run("connection.login", &json!({"id":name,"accessKeyId":"AKIDTEST","secretAccessKey":"alias-test-secret"})).unwrap();
    assert_eq!(logged_in["id"], "c1");
    assert!(s3::connection(&name).is_some());
    let removed = s.run("connection.remove", &json!({"id":name})).unwrap();
    assert_eq!(removed["removed"], "c1");
    assert!(s3::connection(&name).is_none());
    assert_eq!(s.connections.find("connections-alias-target").unwrap().name, "Database");
}

#[test]
fn connections_s3_rename_cannot_inherit_a_destination_session() {
    use crate::render::rustgis_tiles::s3;
    let mut s = Session::new(Project::default());
    let (old, destination, occupied) = ("rename-test-old", "rename-test-destination", "rename-test-occupied");
    let saved = s
        .run(
            "connection.new",
            &json!({
                "kind":"s3","name":old,"address":"https://saved.example","options":{"credentials":"keys"}
            }),
        )
        .unwrap();
    s.run("connection.login", &json!({"id":old,"accessKeyId":"SAVEDKEY","secretAccessKey":"saved-test-secret"})).unwrap();
    s.run(
        "s3.connect",
        &json!({
            "name":destination,"endpoint":"https://runtime.example","accessKeyId":"RUNTIMEKEY","secretAccessKey":"runtime-test-secret"
        }),
    )
    .unwrap();
    s.run("connection.new", &json!({"kind":"s3","name":occupied})).unwrap();
    assert!(s.run("connection.edit", &json!({"id":old,"name":occupied})).is_err());
    assert!(s3::connection(old).is_some(), "a rejected edit must preserve the original login");
    assert!(s3::connection(destination).is_some(), "a rejected edit must not disconnect another session");
    let grouped = s.run("connection.edit", &json!({"id":old,"group":"Work"})).unwrap();
    assert_eq!(grouped["loggedIn"], true, "a group-only edit must preserve the login");
    let renamed = s.run("connection.edit", &json!({"id":old,"name":destination})).unwrap();
    assert_eq!(renamed["id"], saved["id"]);
    assert_eq!(renamed["address"], "https://saved.example");
    assert_eq!(renamed["loggedIn"], false);
    assert!(s3::connection(old).is_none());
    assert!(s3::connection(destination).is_none());
    assert!(s.run("connection.test", &json!({"id":destination})).unwrap_err().contains("log in first"));
    s.run("connection.login", &json!({"id":destination,"accessKeyId":"NEWKEY","secretAccessKey":"new-test-secret"})).unwrap();
    assert_eq!(s3::connection(destination).unwrap().endpoint.as_deref(), Some("https://saved.example"));
    s.run("connection.remove", &json!({"id":destination})).unwrap();
}

#[test]
fn connection_test_probes_and_answers() {
    use crate::render::rustgis_tiles::s3;
    let mut s = Session::new(Project::default());
    let mut wms = json!({"kind":"wms","address":"https://a.example/wms","probe":true});
    let v = s.run("connection.test", &wms).unwrap();
    let fetch = v["fetch"].as_str().unwrap();
    assert!(fetch.starts_with("https://a.example/wms") && fetch.contains("REQUEST=GetCapabilities"));
    assert!(v.get("connection").is_none());
    assert!(s.connections.connections.is_empty());
    wms.as_object_mut().unwrap().remove("probe");
    wms["answer"] = json!(
        r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://a.example/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer></Layer></Capability></WMS_Capabilities>"#
    );
    assert_eq!(s.run("connection.test", &wms).unwrap()["result"]["layers"][0]["name"], "a");
    assert_eq!(
        s.run("connection.test", &json!({"kind":"stac","address":"https://s.example/v1","probe":true})).unwrap()["fetch"],
        "https://s.example/v1/collections"
    );
    assert_eq!(
        s.run(
            "connection.test",
            &json!({"kind":"stac","address":"https://s.example/v1","answer":r#"{"collections":[{"id":"c1","title":"C"}]}"#})
        )
        .unwrap()["result"]["count"],
        1
    );
    let mut p = json!({"kind":"s3","name":"probe-s3","options":{"credentials":"none","buckets":"bkt1"},"probe":true});
    assert!(s.run("connection.test", &p).unwrap()["fetch"].as_str().unwrap().contains("bkt1"));
    p.as_object_mut().unwrap().remove("probe");
    p["answer"] = json!("<ListBucketResult><Contents><Key>a.tif</Key><Size>1</Size></Contents></ListBucketResult>");
    assert_eq!(s.run("connection.test", &p).unwrap()["result"]["count"], 1);
    p["options"].as_object_mut().unwrap().remove("buckets");
    p["probe"] = json!(true);
    assert!(s.run("connection.test", &p).unwrap_err().contains("name a bucket"));
    s3::disconnect("probe-s3");
    #[cfg(not(target_arch = "wasm32"))]
    assert!(
        s.run("connection.test", &json!({"kind":"postgres","address":"postgres:host=127.0.0.1 port=9 dbname=x user=u","probe":true}))
            .unwrap()["fetch"]
            .is_null()
    );
    assert!(s.run("connection.test", &json!({"probe":true})).unwrap_err().contains("id, or kind and address"));
    assert!(s.connections.connections.is_empty());
}

/// Browsing a saved connection lists what each kind offers, a level at a time, with the command
/// that adds each dataset; offline, with the answers supplied. A node that isn't the
/// connection's is refused, and nothing browsed is saved or undoable.
#[test]
fn connection_browse_levels_and_adds() {
    use crate::render::rustgis_tiles::s3;
    let mut s = Session::new(Project::default());
    let new = |s: &mut Session, p: Value| s.run("connection.new", &p).unwrap();
    let wms_caps = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://a.example/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer><Layer><Name>b</Name><Title>B</Title></Layer></Layer></Capability></WMS_Capabilities>"#;

    // WMS: the address read, its layers, and the add that adds one.
    new(&mut s, json!({"kind": "wms", "address": "https://a.example/wms", "name": "W"}));
    let v = s.run("connection.browse", &json!({"id": "W", "probe": true})).unwrap();
    assert!(v["fetch"].as_str().is_some_and(|f| f.contains("REQUEST=GetCapabilities")), "{v}");
    let v = s.run("connection.browse", &json!({"id": "W", "answer": wms_caps})).unwrap();
    assert_eq!(v["service"], json!({"url": "https://a.example/wms", "type": "wms"}));
    let first = v["children"][0].clone();
    assert_eq!(first["what"], "map");
    assert_eq!(first["name"], "A");
    assert_eq!(first["add"]["command"], "service.add");
    assert_eq!(first["add"]["params"]["url"], "https://a.example/wms");
    assert_eq!(first["add"]["params"]["type"], "wms");
    assert_eq!(first["add"]["params"]["layer"], "a");
    assert!(first["add"]["params"].get("source_url").is_some());
    assert_eq!(v["children"].as_array().unwrap().len(), 2);
    let before = s.map().tiles.len();
    let mut add = first["add"]["params"].clone();
    add["text"] = json!(wms_caps);
    s.run("service.add", &add).unwrap();
    assert_eq!(s.map().tiles.len(), before + 1);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.map().tiles.len(), before);

    // ArcGIS: a directory's folder is a node of its own address.
    new(&mut s, json!({"kind": "arcgis", "address": "https://x.example.org/arcgis/rest/services", "name": "Arc"}));
    let dir = r#"{"currentVersion": 11, "folders": ["Hydro"], "services": [{"name": "Topo", "type": "MapServer"}]}"#;
    let v = s.run("connection.browse", &json!({"id": "Arc", "answer": dir})).unwrap();
    let folder = "service:arcgis:https://x.example.org/arcgis/rest/services/Hydro";
    let child = v["children"].as_array().unwrap().iter().find(|c| c["node"] == folder).unwrap_or_else(|| panic!("{v}"));
    assert_eq!(child["what"], "folder");
    assert_eq!(child["name"], "Hydro");
    let v = s.run("connection.browse", &json!({"id": "Arc", "node": folder, "probe": true})).unwrap();
    assert!(v["fetch"].as_str().is_some_and(|f| f.starts_with("https://x.example.org/arcgis/rest/services/Hydro")), "{v}");

    // CSW: a record, the service it links to as a node, and more records than listed.
    new(&mut s, json!({"kind": "csw", "address": "https://cat.example.org/csw", "name": "Cat"}));
    let records = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/">
        <csw:SearchResults numberOfRecordsMatched="3" nextRecord="2"><csw:Record><dc:title>Forests</dc:title>
          <dc:URI protocol="OGC:WMS">https://geo.example.org/wms</dc:URI><dc:URI protocol="OGC:WFS">http://192.168.1.2/wfs</dc:URI>
        </csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#;
    let v = s.run("connection.browse", &json!({"id": "Cat", "answer": records})).unwrap();
    assert_eq!(v["children"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["children"][0]["what"], "record");
    assert_eq!(v["children"][0]["children"][0]["node"], "service:wms:https://geo.example.org/wms");
    assert_eq!((v["matched"].as_u64(), v["truncated"].as_bool()), (Some(3), Some(true)));
    assert!(v.get("next").is_none());

    // STAC: collections, a collection's search, and an item's asset with the add of it.
    new(&mut s, json!({"kind": "stac", "address": "https://s.example/v1", "name": "Stac"}));
    let v = s.run("connection.browse", &json!({"id": "Stac", "probe": true})).unwrap();
    assert_eq!(v["fetch"], "https://s.example/v1/collections");
    let v = s.run("connection.browse", &json!({"id": "Stac", "answer": r#"{"collections":[{"id":"c1","title":"C"}]}"#})).unwrap();
    assert_eq!(v["children"][0]["node"], "collection:c1");
    assert_eq!(v["children"][0]["options"], true);
    let v = s.run("connection.browse", &json!({"id": "Stac", "node": "collection:c1", "probe": true})).unwrap();
    let fetch = v["fetch"].as_str().unwrap();
    assert!(fetch.contains("/search?") && fetch.contains("collections=c1") && fetch.contains("limit=20"), "{fetch}");
    let self_link = "https://s.example/v1/collections/c1/items/i1";
    let items = json!({"type": "FeatureCollection", "numberMatched": 1, "features": [{
        "type": "Feature", "id": "i1", "collection": "c1", "bbox": [0, 0, 1, 1], "geometry": null,
        "properties": {"datetime": "2024-01-01T00:00:00Z"},
        "assets": {"visual": {"href": "https://s.example/a.tif", "type": "image/tiff; application=geotiff"}},
        "links": [{"rel": "self", "href": self_link}]}]});
    let v = s.run("connection.browse", &json!({"id": "Stac", "node": "collection:c1", "answer": items.to_string()})).unwrap();
    assert_eq!(v["children"][0]["what"], "item");
    assert_eq!(
        v["children"][0]["children"][0]["add"],
        json!({"command": "stac.addAsset", "params": {"url": self_link, "asset": "visual", "name": "i1 visual"}})
    );
    assert!(v.get("truncated").is_none());

    // A static STAC catalog: catalogs and collections, then a collection's items, each opened
    // for its assets.
    new(&mut s, json!({"kind": "stac", "address": "https://s.example/static/catalog.json", "name": "Static"}));
    let root = json!({"type": "Catalog", "id": "r", "links": [{"rel": "child", "href": "./c1/collection.json", "title": "C1"}]});
    let v = s.run("connection.browse", &json!({"id": "Static", "answer": root.to_string()})).unwrap();
    assert_eq!(v["children"][0]["node"], "catalog:https://s.example/static/c1/collection.json");
    let collection = json!({"type": "Collection", "id": "c1", "links": [
        {"rel": "item", "href": "./i1/i1.json", "title": "Scene 1"}, {"rel": "child", "href": "./sub/catalog.json", "title": "Sub"}]});
    let node = "catalog:https://s.example/static/c1/collection.json";
    let v = s.run("connection.browse", &json!({"id": "Static", "node": node, "answer": collection.to_string()})).unwrap();
    let kinds: Vec<_> =
        v["children"].as_array().unwrap().iter().map(|c| (c["what"].as_str().unwrap(), c["node"].as_str().unwrap())).collect();
    assert_eq!(
        kinds,
        [("catalog", "catalog:https://s.example/static/c1/sub/catalog.json"), ("item", "item:https://s.example/static/c1/i1/i1.json")]
    );
    let item_node = "item:https://s.example/static/c1/i1/i1.json";
    let v = s.run("connection.browse", &json!({"id": "Static", "node": item_node, "probe": true})).unwrap();
    assert_eq!(v["fetch"], "https://s.example/static/c1/i1/i1.json");
    let item = json!({"type": "Feature", "id": "i1", "collection": "c1", "bbox": [0, 0, 1, 1], "geometry": null,
        "properties": {"datetime": "2024-01-01T00:00:00Z"},
        "assets": {"visual": {"href": "https://s.example/static/a.tif", "type": "image/tiff; application=geotiff"}},
        "links": [{"rel": "self", "href": "https://s.example/static/c1/i1/i1.json"}]});
    let v = s.run("connection.browse", &json!({"id": "Static", "node": item_node, "answer": item.to_string()})).unwrap();
    assert_eq!(
        v["children"][0]["add"],
        json!({"command": "stac.addAsset", "params": {"url": "https://s.example/static/c1/i1/i1.json", "asset": "visual", "name": "i1 visual"}})
    );
    // An asset's key names its item, so the same file under two items is two rows.
    assert_eq!(v["children"][0]["key"], "https://s.example/static/c1/i1/i1.json#visual");
    // A relative asset address is resolved against the item's own file.
    let relative = item.to_string().replace(r#""assets":{"#, r#""assets":{"data":{"href":"./data/b.tif","type":"image/tiff"},"#);
    let v = s.run("connection.browse", &json!({"id": "Static", "node": item_node, "answer": relative})).unwrap();
    let hrefs: Vec<&str> = v["children"].as_array().unwrap().iter().filter_map(|c| c["href"].as_str()).collect();
    assert!(hrefs.contains(&"https://s.example/static/c1/i1/data/b.tif"), "{hrefs:?}");
    // An item node belongs to a STAC connection, and is judged as its documents are.
    assert!(s.run("connection.browse", &json!({"id": "W", "node": item_node})).is_err());
    assert!(s.run("connection.browse", &json!({"id": "Static", "node": "item:http://127.0.0.1:9/i.json"})).is_err());
    // S3: buckets from the connection's options need no fetch; a bucket's level is read.
    new(&mut s, json!({"kind": "s3", "name": "BrowseB", "options": {"credentials": "none", "buckets": "bkt1"}}));
    let v = s.run("connection.browse", &json!({"id": "BrowseB", "probe": true})).unwrap();
    assert!(v["fetch"].is_null(), "{v}");
    let v = s.run("connection.browse", &json!({"id": "BrowseB"})).unwrap();
    assert_eq!(v["children"][0]["node"], "s3:bkt1/");
    let v = s.run("connection.browse", &json!({"id": "BrowseB", "node": "s3:bkt1/", "probe": true})).unwrap();
    assert!(v["fetch"].as_str().is_some_and(|f| f.contains("bkt1")), "{v}");
    let listing = "<ListBucketResult><CommonPrefixes><Prefix>p/</Prefix></CommonPrefixes><Contents><Key>p.tif</Key><Size>5</Size></Contents><Contents><Key>q/</Key><Size>0</Size></Contents></ListBucketResult>";
    let v = s.run("connection.browse", &json!({"id": "BrowseB", "node": "s3:bkt1/", "answer": listing})).unwrap();
    assert_eq!(v["children"][0]["node"], "s3:bkt1/p/");
    assert_eq!(v["children"][0]["name"], "p/");
    assert_eq!(v["children"][1]["add"]["params"]["url"], "s3://bkt1/p.tif");
    assert_eq!(v["children"].as_array().unwrap().len(), 2, "{v}");
    s3::disconnect("BrowseB");

    // Nodes that aren't the connection's, and ones that are hostile.
    let long = "x".repeat(5000);
    for (id, node) in [
        ("W", "collection:x"),
        ("BrowseB", "s3:BAD_/"),
        ("W", long.as_str()),
        ("W", "service:ftp:x"),
        ("W", "service:wms:http://127.0.0.1:9/wms"),
        ("Stac", "catalog:http://127.0.0.1:9/c.json"),
        ("Stac", "s3:bkt1/"),
        ("W", "nope"),
        ("W", "a\u{7}b"),
    ] {
        let e = s.run("connection.browse", &json!({"id": id, "node": node})).unwrap_err();
        assert!(!e.is_empty());
    }
    assert!(s.run("connection.browse", &json!({"id": "missing"})).unwrap_err().contains("no connection"));
    #[cfg(not(target_arch = "wasm32"))]
    {
        new(&mut s, json!({"kind": "postgres", "address": "postgres:host=127.0.0.1 port=9 dbname=x user=u", "name": "Pg"}));
        assert!(!s.run("connection.browse", &json!({"id": "Pg"})).unwrap_err().is_empty());
    }
    // A catalogue's download link too long to be one (the parser keeps addresses to 2000 bytes) is not listed.
    let long_link = format!("https://files.example.org/{}.tif", "a".repeat(5000));
    let records = format!(
        r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="1"><csw:Record><dc:title>Files</dc:title>
          <dc:URI protocol="WWW:DOWNLOAD">https://files.example.org/a.tif</dc:URI><dc:URI protocol="WWW:DOWNLOAD">{long_link}</dc:URI></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#
    );
    let v = s.run("connection.browse", &json!({"id": "Cat", "answer": records})).unwrap();
    let links = v["children"][0]["children"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{v}");
    assert_eq!(links[0]["add"]["params"]["url"], "https://files.example.org/a.tif");
    assert!(v.get("truncated").is_none(), "{v}");
    // An answer larger than a service document may be is refused before it is parsed.
    let huge = "x".repeat(65 << 20);
    assert!(s.run("connection.browse", &json!({"id": "W", "answer": huge})).unwrap_err().contains("too large"));
    assert!(
        s.run("connection.browse", &json!({"id": "W", "answer": "<x/>", "documents": [{"url": "https://a.example/x", "text": huge}]}))
            .unwrap_err()
            .contains("too large")
    );
    // An OGC API's feature collections are imported together: listed, with options, not added alone.
    new(&mut s, json!({"kind": "ogcapi", "address": "https://o.example/collections", "name": "Ogc"}));
    let collections = r#"{"collections":[{"id":"lakes","title":"Lakes","links":[{"rel":"items","type":"application/geo+json","href":"https://o.example/collections/lakes/items"}]}]}"#;
    let v = s.run("connection.browse", &json!({"id": "Ogc", "answer": collections})).unwrap();
    assert_eq!((v["children"][0]["what"].as_str(), v["children"][0]["options"].as_bool()), (Some("features"), Some(true)), "{v}");
    assert!(v["children"][0].get("add").is_none(), "{v}");
    let saved = s.connections.connections.len();
    assert!(saved >= 5);
    s.run("connection.browse", &json!({"id": "W", "answer": wms_caps})).unwrap();
    assert_eq!(s.connections.connections.len(), saved);
    assert!(!s.can_undo());
}

fn rustgis_engine_sources_roundtrip(text: &str) -> crate::sources::Sources {
    crate::sources::Sources::from_json(text)
}

/// A DXF drawing through the commands: read natively (not through GDAL) by path and upload, its
/// own layers listed and picked with "layer", and its coordinates converted with "crs".
#[test]
fn dxf_drawings_by_path_and_upload() {
    use base64::Engine;
    let pairs: &[(i32, &str)] = &[
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "LINE"),
        (8, "Roads"),
        (10, "500000"),
        (20, "4649776"),
        (11, "500100"),
        (21, "4649776"),
        (0, "POINT"),
        (8, "Wells"),
        (10, "500050"),
        (20, "4649800"),
        (0, "TEXT"),
        (8, "Wells"),
        (10, "500050"),
        (20, "4649800"),
        (1, "W1"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ];
    let text: String = pairs.iter().map(|(c, v)| format!("{c:>3}\n{v}\n")).collect();
    assert!(!rustgis_data::source::gdal::is_gdal_name("site.dxf"));
    let dir = std::env::temp_dir().join(format!("rustgis-dxf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("site.dxf");
    std::fs::write(&path, &text).unwrap();
    let path = path.display().to_string();
    let mut s = Session::new(Project::default());
    let listed = s.run("info.fileLayers", &json!({"path": path})).unwrap();
    assert_eq!(listed, json!({"count": 2, "layers": ["Roads", "Wells"]}));
    // Converted from UTM 33N: the line runs east from 15° E, 42° N.
    let r = s.run("layer.addData", &json!({"path": path, "crs": "EPSG:32633"})).unwrap();
    let mut names: Vec<String> = s.map().layers.iter().map(|l| l.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["site lines", "site points"], "{r}");
    let e = s.map().layer_by_name("site lines").unwrap().extent();
    assert!((e.min_x - 15.0).abs() < 1e-6 && (e.min_y - 42.0).abs() < 0.01, "{e:?}");
    // One DXF layer, by its name.
    let mut t = Session::new(Project::default());
    t.run("layer.addData", &json!({"path": path, "layer": "wells", "crs": "EPSG:32633"})).unwrap();
    let only: Vec<String> = t.map().layers.iter().map(|l| l.name.clone()).collect();
    assert_eq!(only, ["site points"]);
    assert_eq!(t.map().layers[0].dataset.features.len(), 2);
    assert!(t.run("layer.addData", &json!({"path": path, "layer": "nope"})).unwrap_err().contains("nothing drawn on layer 'nope'"));
    // Without "crs" the coordinates are kept, and the user is told.
    let mut u = Session::new(Project::default());
    u.run("layer.addDataBytes", &json!({"fileName": "upload.dxf", "data": base64::engine::general_purpose::STANDARD.encode(&text)}))
        .unwrap();
    assert_eq!(u.map().layers.len(), 2);
    assert!(u.notices.iter().any(|n| n.text.contains("doesn't say its coordinate system")), "{:?}", u.notices);
    assert!(u.run("layer.addDataBytes", &json!({"fileName": "bad.dxf", "data": "AAAA"})).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

/// Overture and Source Cooperative refuse what they can't read before asking anything of the
/// network (their answers are read by the catalog modules' own tests).
#[test]
fn catalog_commands_refuse_bad_parameters() {
    let mut s = Session::new(Project::default());
    for bad in [
        json!({}),
        json!({"theme": "roads"}),
        json!({"theme": "buildings", "type": "segment"}),
        json!({"theme": "buildings", "release": "../x"}),
        json!({"theme": "buildings", "bbox": [1, 2, 3]}),
        json!({"theme": "buildings", "bbox": [10, 50, 0, 40]}),
        json!({"theme": "buildings", "bbox": "everywhere"}),
    ] {
        assert!(s.run("overture.add", &bad).is_err(), "{bad}");
    }
    for bad in [
        json!({}),
        json!({"account": "giswqs"}),
        json!({"account": "../etc", "product": "p"}),
        json!({"account": "a", "product": "p", "path": "../../x/"}),
    ] {
        assert!(s.run("sourcecoop.list", &bad).is_err(), "{bad}");
    }
    assert!(s.run("sourcecoop.products", &json!({"account": "a/b"})).is_err());
    assert!(s.map().layers.is_empty() && s.map().tiles.is_empty());
}

/// Overture's files are added all or none: one that can't be read takes the ones added before it
/// out again, so trying again doesn't add them twice.
#[test]
fn overture_files_are_added_all_or_none() {
    use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
    let mut ds = Dataset::new("b", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..50u32 {
        let c = rustgis_geom::Coord::new(7.0 + 0.01 * f64::from(i), 46.0);
        ds.features.push(Feature { id: u64::from(i) + 1, geometry: Some(Geometry::Point(c)), attrs: vec![Value::Int(i64::from(i))] });
    }
    let (good, _) = serve_ranges(rustgis_data::source::fgb::write(&ds).unwrap());
    let (bad, _) = serve_ranges(b"not a flatgeobuf at all".to_vec());
    let mut s = Session::new(Project::default());
    let before = s.map().layers.len();
    let urls = vec![format!("{good}/part-0.fgb"), format!("{bad}/part-1.fgb")];
    let e = crate::cmd::catalogs::add_files(&mut s, "building", &urls).unwrap_err();
    assert!(e.contains("none of the 2 files was added"), "{e}");
    assert_eq!(s.map().layers.len(), before, "the first file's layer was taken out again");
    // The app's way (the files found and opened already) takes Overture's own files only: any
    // other address is refused before anything is read.
    let files = json!([format!("{good}/part-0.fgb"), format!("{bad}/part-1.fgb")]);
    let e = s.run("overture.add", &json!({"theme": "buildings", "files": files})).unwrap_err();
    assert!(e.contains("not one of Overture's release files"), "{e}");
    assert_eq!(s.map().layers.len(), before);
    for bad_files in [json!([]), json!("x"), json!(["https://x.org/a.png"]), json!(vec![format!("{good}/a.fgb"); 7])] {
        assert!(s.run("overture.add", &json!({"theme": "buildings", "files": bad_files})).is_err(), "{bad_files}");
    }
    assert_eq!(s.map().layers.len(), before);
    // Both readable: both added, numbered.
    let urls = vec![format!("{good}/part-0.fgb"), format!("{good}/part-1.fgb")];
    let r = crate::cmd::catalogs::add_files(&mut s, "building", &urls).unwrap();
    assert_eq!(r["files"], 2);
    let names: Vec<&str> = s.map().layers.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"Overture building (1/2)") && names.contains(&"Overture building (2/2)"), "{names:?}");
}

/// A multi-file add that fails puts the map back exactly: the same layers in the same order, and
/// the same active layer.
#[test]
fn a_failed_multi_file_add_leaves_the_map_as_it_was() {
    use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
    let mut ds = Dataset::new("b", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..20u32 {
        let c = rustgis_geom::Coord::new(7.0 + 0.01 * f64::from(i), 46.0);
        ds.features.push(Feature { id: u64::from(i) + 1, geometry: Some(Geometry::Point(c)), attrs: vec![Value::Int(i64::from(i))] });
    }
    let (good, _) = serve_ranges(rustgis_data::source::fgb::write(&ds).unwrap());
    let (bad, _) = serve_ranges(b"not a flatgeobuf at all".to_vec());
    let mut s = world();
    // A layer other than the last added is active.
    let rivers = s.map().layer_by_name("Rivers").unwrap().id;
    s.active_layer = Some(rivers);
    let project = serde_json::to_string(&s.project).unwrap();
    let urls = vec![format!("{good}/part-0.fgb"), format!("{good}/part-1.fgb"), format!("{bad}/part-2.fgb")];
    assert!(crate::cmd::catalogs::add_files(&mut s, "building", &urls).is_err());
    assert_eq!(serde_json::to_string(&s.project).unwrap(), project, "the same layers, in the same order");
    assert_eq!(s.active_layer, Some(rivers));
    // A box across the antimeridian (west past east) is an area, not an error.
    let fiji = crate::cmd::catalogs::area(&s, &json!({"bbox": [178.0, -18.0, -178.0, -17.0]})).unwrap();
    assert_eq!(fiji, [178.0, -18.0, -178.0, -17.0]);
    assert!(crate::cmd::catalogs::area(&s, &json!({"bbox": [200.0, 0.0, 10.0, 1.0]})).unwrap_err().contains("antimeridian"));
}

/// Geotagged photos: a folder of them becomes a point layer with each photo's time, camera and
/// thumbnail; photos without a position are named, not added; an upload joins the Photos layer;
/// a project keeps the thumbnails.
#[test]
fn geotagged_photos_become_a_photo_layer() {
    use base64::Engine;
    use rustgis_data::photo;
    let dir = std::env::temp_dir().join(format!("rustgis-photos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.jpg"), photo::example(true, (43.4674, "N"), (11.8851, "E"))).unwrap();
    std::fs::write(dir.join("b.JPEG"), photo::example(false, (43.4671, "N"), (11.8853, "E"))).unwrap();
    // A JPEG without EXIF, and a file that isn't a photo.
    std::fs::write(dir.join("plain.jpg"), b"\xFF\xD8\xFF\xD9").unwrap();
    std::fs::write(dir.join("notes.txt"), b"x").unwrap();
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addPhotos", &json!({"folder": dir.display().to_string()})).unwrap();
    assert_eq!((r["photos"].as_u64(), r["skipped"].as_u64()), (Some(2), Some(1)), "{r}");
    assert!(s.notices.iter().any(|n| n.text.contains("plain.jpg")), "the photo without EXIF is named");
    let l = s.map().layer_by_name("Photos").unwrap();
    assert!(photo::is_photo_layer(&l.dataset));
    let f = &l.dataset.features[0];
    assert_eq!(f.attrs[0], rustgis_data::Value::Text("a".into()));
    assert_eq!(f.attrs[2], rustgis_data::Value::Text("2008-10-22 16:29:49".into()));
    assert!(matches!(&f.attrs[6], rustgis_data::Value::Text(t) if t.starts_with("data:image/jpeg;base64,")));
    // An upload (the web app's way, one file at a time) joins the Photos layer.
    let one = base64::engine::general_purpose::STANDARD.encode(photo::example(true, (43.468, "N"), (11.881, "E")));
    s.run("layer.addDataBytes", &json!({"fileName": "c.jpg", "data": one})).unwrap();
    assert_eq!(s.map().layers.iter().filter(|l| l.name == "Photos").count(), 1);
    assert_eq!(s.map().layer_by_name("Photos").unwrap().dataset.features.len(), 3);
    // So does a photo added by its path.
    s.run("layer.addData", &json!({"path": dir.join("a.jpg").display().to_string()})).unwrap();
    assert_eq!(s.map().layer_by_name("Photos").unwrap().dataset.features.len(), 4);
    // The project keeps them, thumbnails and all.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("Photos").unwrap();
    assert_eq!(again.dataset.features.len(), 4);
    assert!(photo::is_photo_layer(&again.dataset));
    // Nothing to add: an error, and nothing added.
    assert!(s.run("layer.addPhotos", &json!({})).is_err());
    assert!(s.run("layer.addPhotos", &json!({"paths": [dir.join("plain.jpg").display().to_string()]})).is_err());
    // Joining a layer one photo short of full: the first photo by name is added, the rest are
    // left out (and never read), and the map zooms to the photo added, not the far-off one.
    let near = dir.join("near-full");
    std::fs::create_dir_all(&near).unwrap();
    std::fs::write(near.join("b-far.jpg"), photo::example(true, (10.0, "S"), (50.0, "W"))).unwrap();
    std::fs::write(near.join("a-near.jpg"), photo::example(true, (43.4674, "N"), (11.8851, "E"))).unwrap();
    let id = s.map().layer_by_name("Photos").unwrap().id;
    let l = s.map_mut().layer_mut(id).unwrap();
    let first = l.dataset.features[0].clone();
    while l.dataset.features.len() < photo::MAX_PHOTOS - 1 {
        l.dataset.push(first.geometry.clone(), first.attrs.clone());
    }
    let r = s.run("layer.addPhotos", &json!({"folder": near.display().to_string()})).unwrap();
    assert_eq!((r["photos"].as_u64(), r["skipped"].as_u64()), (Some(1), Some(1)), "{r}");
    let l = s.map().layer_by_name("Photos").unwrap();
    assert_eq!(l.dataset.features.len(), photo::MAX_PHOTOS);
    assert_eq!(l.dataset.features.last().unwrap().attrs[0], rustgis_data::Value::Text("a-near".into()));
    let c = rustgis_geom::merc_to_lonlat(s.map().camera.center);
    assert!((c.x - 11.8851).abs() < 0.01 && (c.y - 43.4674).abs() < 0.01, "zoomed to {c:?}");
    // Full: nothing more is added, and that's said.
    let r = s.run("layer.addPhotos", &json!({"folder": near.display().to_string()}));
    assert!(r.is_err_and(|e| e.contains("up to")));
    assert_eq!(s.map().layer_by_name("Photos").unwrap().dataset.features.len(), photo::MAX_PHOTOS);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn coverage_answers_that_are_not_geotiffs_are_errors() {
    use crate::cmd::service::coverage_check;
    assert!(coverage_check("c", b"II*\0rest").is_ok());
    assert!(coverage_check("c", b"MM\0*rest").is_ok());
    assert!(coverage_check("c", b"II+\0rest").is_ok());
    for bad in [&b"<ows:ExceptionReport/>"[..], b"Service unavailable", b"", b"\xff\xd8\xff", b"MM: service down", b"II"] {
        let e = coverage_check("c", bad).err().unwrap_or_default();
        assert!(e.starts_with("c: the service answered with an error"), "{e}");
    }
}

/// The keys a command's source reads from its parameters, found by scanning `src/cmd/*.rs`: the
/// command's function (or closure) and the functions it passes its parameters to (in its own
/// file, or another's as `module::name(…)`), a few calls deep.
fn keys_read_by_commands() -> Vec<(String, std::collections::BTreeSet<String>)> {
    use std::collections::{BTreeMap, BTreeSet};
    type Fn = (Vec<String>, String);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd");
    // Each module's functions: their `&Value` arguments and bodies.
    let mut modules: BTreeMap<String, (String, BTreeMap<String, Fn>)> = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        let mut fns: BTreeMap<String, Fn> = BTreeMap::new();
        let mut rest = src.as_str();
        while let Some(at) = rest.find("fn ") {
            let after = &rest[at + 3..];
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let Some(open) = after.find('{') else { break };
            let body = balanced(&after[open..]);
            fns.entry(name).or_insert((value_args(&after[..open]), body.to_string()));
            rest = &after[open..];
        }
        modules.insert(stem, (src, fns));
    }
    let mut out = Vec::new();
    for (stem, (src, fns)) in &modules {
        let mut rest = src.as_str();
        while let Some(at) = rest.find("C::new(") {
            let call = balanced(&rest[at + 6..]);
            rest = &rest[at + 6..];
            let parts: Vec<&str> = call.splitn(2, '"').collect();
            let id: String = parts.get(1).unwrap_or(&"").chars().take_while(|c| *c != '"').collect();
            // After the three strings: a function name or a closure.
            let mut tail = call;
            for _ in 0..6 {
                tail = &tail[tail.find('"').map_or(tail.len(), |i| i + 1)..];
            }
            let tail = tail.trim_start_matches([',', ' ', '\n']);
            let mut keys = BTreeSet::new();
            let mut todo: Vec<(String, Fn)> = Vec::new();
            if let Some(closure) = tail.strip_prefix('|') {
                let args = closure[..closure.find('|').unwrap()].split(',').map(|a| a.trim().to_string()).skip(1).collect();
                todo.push((stem.clone(), (args, tail.to_string())));
            } else {
                let name: String = tail.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                todo.extend(fns.get(&name).cloned().map(|f| (stem.clone(), f)));
            }
            let mut seen = BTreeSet::new();
            let mut depth = 0;
            while !todo.is_empty() && depth < 6 {
                for (module, (args, body)) in std::mem::take(&mut todo) {
                    let args: Vec<&String> = args.iter().filter(|a| !a.starts_with('_')).collect();
                    for a in &args {
                        // The helpers that read a key: `ps(p, "key")`…
                        for h in ["ps", "f", "b", "uint", "s_req", "color", "get_color", "band", "point", "limit", "method"] {
                            keys.extend(literal_keys(&body, &format!("{h}({a}, \"")));
                        }
                        keys.extend(literal_keys(&body, &format!("{a}.get(\"")));
                        keys.extend(literal_keys(&body, &format!("{a}[\"")));
                        if body.contains(&format!("layer_id({a})")) || body.contains(&format!("layer_id(&{a})")) {
                            keys.insert("layer".to_string());
                        }
                        if body.contains(&format!("table_index({a})")) {
                            keys.insert("table".to_string());
                        }
                    }
                    // The functions it passes one of its parameter values to, at any position.
                    for (m, (_, mfns)) in &modules {
                        for (name, f) in mfns {
                            let qualified = if m == "mod" { format!("super::{name}") } else { format!("{m}::{name}") };
                            let call = if *m == module { name.as_str() } else { qualified.as_str() };
                            let key = format!("{m}::{name}");
                            // A dispatcher by kind: its arms read keys for other commands.
                            if key == "items::item_bytes" {
                                continue;
                            }
                            if !seen.contains(&key) && args.iter().any(|a| calls(&body, call, a)) {
                                seen.insert(key);
                                todo.push((m.clone(), f.clone()));
                            }
                        }
                    }
                }
                depth += 1;
            }
            out.push((id, keys));
        }
    }
    out
}

/// `s` from its opening bracket to the matching close, skipping strings, raw strings and chars.
fn balanced(s: &str) -> &str {
    let b = s.as_bytes();
    let (mut depth, mut i) = (0i32, 0);
    while i < b.len() {
        match b[i] {
            b'r' if b.get(i + 1) == Some(&b'#') && b.get(i + 2) == Some(&b'"') => {
                i += s[i + 3..].find("\"#").map_or(b.len(), |n| n + 4);
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'\'' if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') => i += 3,
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    s
}

/// Whether `body` calls `name(…)` (not as part of a longer name) with `arg` itself as one of its
/// arguments (`p`, `&p`, `p.clone()`).
fn calls(body: &str, name: &str, arg: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    body.match_indices(&format!("{name}(")).any(|(i, m)| {
        let before = body[..i].chars().next_back();
        if word(before) || before == Some('.') || (!name.contains("::") && before == Some(':')) {
            return false;
        }
        let args = balanced(&body[i + m.len() - 1..]);
        let inner = &args[1..args.len().saturating_sub(1)];
        // The arguments, split at top-level commas.
        let (mut depth, mut start, mut found) = (0i32, 0, false);
        for (j, c) in inner.char_indices().chain([(inner.len(), ',')]) {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    let a = inner[start..j].trim();
                    found |= [arg.to_string(), format!("&{arg}"), format!("{arg}.clone()")].contains(&a.to_string());
                    start = j + 1;
                }
                _ => {}
            }
        }
        found
    })
}

/// The names of the `&Value` arguments in a function signature.
fn value_args(sig: &str) -> Vec<String> {
    sig.split([',', '('])
        .filter_map(|a| {
            a.split_once(':').filter(|(_, t)| t.trim().starts_with("&") && t.contains("Value")).map(|(n, _)| n.trim().to_string())
        })
        .collect()
}

/// The string literals that follow `prefix` (not part of a longer name) in `body`.
fn literal_keys(body: &str, prefix: &str) -> Vec<String> {
    body.match_indices(prefix)
        .filter(|(i, _)| !body[..*i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
        .map(|(i, _)| body[i + prefix.len()..].chars().take_while(|c| *c != '"').collect::<String>())
        .filter(|k| !k.is_empty())
        .collect()
}

#[test]
fn params_name_every_key_a_command_reads() {
    // Read by a shared helper but of no use to the command.
    let unused = [("gp.spatialJoin", "input"), ("profile.alongSelected", "row"), ("sources.remove", "name"), ("sources.remove", "extra")];
    let s = Session::new(Project::default());
    let mut missing = Vec::new();
    let found = keys_read_by_commands();
    assert!(found.len() > 300, "the scan found {} commands", found.len());
    for (id, keys) in found {
        let Some(spec) = s.registry.get(&id) else { continue };
        let named = spec.param_keys().unwrap_or_default();
        let absent: Vec<_> = keys
            .iter()
            .filter(|k| {
                !named.contains(&k.as_str()) && !spec.params.contains(&format!("\"{k}\"")) && !unused.contains(&(id.as_str(), k.as_str()))
            })
            .collect();
        if !absent.is_empty() {
            missing.push(format!("{id}: {absent:?} (params {})", spec.params));
        }
    }
    assert!(missing.is_empty(), "{} commands read keys their params don't name:\n{}", missing.len(), missing.join("\n"));
}

#[test]
fn needs_input_reads_descriptions() {
    use crate::needs_input;
    assert!(!needs_input("{}"));
    assert!(!needs_input(r#"{"layer"?, "name"?: text}"#));
    assert!(needs_input(r#"{"layer": raster, "points"?: [[column, row, lon, lat], …]}"#));
    assert!(needs_input(r#"{"path": folder | "url": "https://…", "name"?: text}"#));
    assert!(needs_input(r#"{"url": "https://…/x.json?f=json"}"#));
    assert!(!needs_input(r#"{"steps"?: [{"required": value}], "url"?: "https://…/?q=x"}"#));
    assert!(needs_input("a GeoJSON text: {type: …}"));
    assert!(!needs_input("a GeoJSON text"));
    assert!(needs_input("a service URL: https://…/query?f=json"));
    let s = Session::new(Project::default());
    assert!(s.registry.get("raster.georeference").is_some_and(crate::CommandSpec::needs_input));
    assert!(s.registry.get("map.zoomIn").is_some_and(|c| !c.needs_input()));
}

/// A description splits into one entry per parameter, for help that lists them (#662): commas
/// inside quotes, parentheses, brackets or braces stay in their entry.
#[test]
fn param_list_splits_a_description_into_entries() {
    use crate::{Param, param_list};
    let desc =
        r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "steps"?: [{"tool": id}, …], "where": "a, b"}"#;
    let list = param_list(desc).unwrap();
    let keys: Vec<(&str, bool)> = list.iter().map(|p| (p.key, p.optional)).collect();
    assert_eq!(keys, [("layer", true), ("class", true), ("steps", true), ("where", false)]);
    assert_eq!(list[1].text, r#""class"?: name (none: the Default class, by whatever name it has)"#);
    assert_eq!(param_list("{}"), Some(Vec::<Param>::new()));
    // An escaped quote (a plugin's description may have one) doesn't end a value early.
    let escaped = param_list(r#"{"title"?: "a \"quoted, word\"", "n": number}"#).unwrap();
    assert_eq!(escaped.iter().map(|p| p.key).collect::<Vec<_>>(), ["title", "n"]);
    assert_eq!(param_list("a GeoJSON text"), None);
    // Every command's description splits into entries, each led by a key `param_keys` finds: one
    // cut in the wrong place (an unclosed bracket or quote in its prose) would lead with another.
    let s = Session::new(Project::default());
    for c in &s.registry.commands {
        let list = param_list(c.params).unwrap_or_else(|| panic!("{}: {}", c.id, c.params));
        let keys = c.param_keys().unwrap_or_default();
        for p in &list {
            assert!(keys.contains(&p.key), "{}: entry {:?} in {}", c.id, p.text, c.params);
        }
    }
}

#[test]
fn param_keys_reads_descriptions() {
    use crate::param_keys;
    assert_eq!(param_keys("{}"), Some(vec![]));
    assert_eq!(param_keys(r#"{"layer"?, "name": string}"#), Some(vec!["layer", "name"]));
    // Values, nested objects and prose are not keys.
    assert_eq!(
        param_keys(r#"{"mode": "a"|"b", "steps"?: [{"tool": id}], "extent"?: [w, s, e, n] (clip, "view")}"#),
        Some(vec!["mode", "steps", "extent"])
    );
    // Alternatives.
    assert_eq!(param_keys(r#"{"path": folder | "url": "https://…", "name"?: text}"#), Some(vec!["path", "url", "name"]));
    assert_eq!(param_keys(r#"{"lon" | "x": number, "lat" | "y": number}"#), Some(vec!["lon", "x", "lat", "y"]));
    assert_eq!(param_keys(r##"{"color": "#RRGGBB"|"#RRGGBBAA"}"##), Some(vec!["color"]));
    assert_eq!(param_keys("a GeoJSON text"), None);
    // Every command's description reads as keys, and names a key once.
    for c in &Session::new(Project::default()).registry.commands {
        let keys = c.param_keys().unwrap_or_else(|| panic!("{}: params {} are not an object", c.id, c.params));
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "{}: {}", c.id, c.params);
    }
}

#[test]
fn unknown_params_are_said() {
    let mut s = world();
    let mark = s.notice_mark();
    s.run("layer.moveDown", &json!({"layer": "Cities", "steps": 13})).unwrap();
    let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said, ["layer.moveDown ignores 'steps' (it reads layer)"]);
    // Known keys, and commands that take none, say nothing more.
    let mark = s.notice_mark();
    s.run("layer.rename", &json!({"layer": "Cities", "name": "Towns"})).unwrap();
    assert!(s.notices_since(mark).is_empty());
    s.run("layout.close", &json!({"x": 1})).unwrap();
    assert_eq!(s.notices_since(mark).first().map(|n| n.text.as_str()), Some("layout.close ignores 'x' (it reads no parameters)"));
    // Many or long keys are said briefly.
    let mut junk = serde_json::Map::new();
    for i in 0..1000 {
        junk.insert(format!("{i:04}{}", "k".repeat(500)), Value::Null);
    }
    let mark = s.notice_mark();
    s.run("layout.close", &Value::Object(junk)).unwrap();
    let said = s.notices_since(mark).first().map(|n| n.text.clone()).unwrap_or_default();
    assert!(said.len() < 500 && said.contains("995 more"), "{said}");
}

#[test]
fn geojson_read_in_place_when_asked_or_large() {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
    for i in 0..500 {
        ds.push(Some(Geometry::Point(Coord::new(-170.0 + i as f64 * 0.68, (i % 50) as f64))), vec![V::Int(i)]);
    }
    let p = std::env::temp_dir().join(format!("rustgis-engine-{}-pts.geojson", std::process::id()));
    rustgis_data::io::write_path(&ds, &p).unwrap();
    // Small: loaded whole, as before.
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": p.display().to_string()})).unwrap();
    assert!(!s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().is_streamed());
    // Asked to stream: read in place, saved as its file, opened again in place.
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": p.display().to_string(), "stream": true})).unwrap();
    assert_eq!(r["streamed"], true, "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    let v = view_of(&mut s, rustgis_geom::Extent::new(-180.0, -10.0, 180.0, 60.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 500);
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert!(t.map().layers[0].is_streamed() && t.map().layers[0].source.as_ref().unwrap().is_open());
    // Load All Features: the same features as a whole read.
    s.run("layer.loadAll", &json!({"layer": id})).unwrap();
    let whole = rustgis_data::io::read_path(&p).unwrap().0;
    assert_eq!(s.map().layer(id).unwrap().dataset.features, whole.features);
    // Larger than IN_PLACE_ABOVE (trailing whitespace pads it): read in place without being asked,
    // and streamed past STREAM_ABOVE features.
    let n = rustgis_map::source::STREAM_ABOVE as usize + 1;
    let mut many = Dataset::new("many", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
    for i in 0..n {
        many.push(Some(Geometry::Point(Coord::new(-179.0 + 358.0 * i as f64 / n as f64, 0.0))), vec![V::Int(i as i64)]);
    }
    let large = std::env::temp_dir().join(format!("rustgis-engine-{}-large.geojson", std::process::id()));
    let mut text = rustgis_data::io::to_geojson(&many).to_string().into_bytes();
    let json_len = text.len();
    let pad = (rustgis_data::source::geojson::IN_PLACE_ABOVE as usize + 1).saturating_sub(text.len());
    text.resize(text.len() + pad, b' ');
    std::fs::write(&large, &text).unwrap();
    let mut u = Session::new(Project::default());
    let r = u.run("layer.addData", &json!({"path": large.display().to_string()})).unwrap();
    assert_eq!(r["streamed"], true, "{r}");
    assert_eq!(r["features"], n as u64, "{r}");
    // A large file that isn't JSON says why, rather than falling back to a whole read.
    text.truncate(json_len / 2);
    text.resize(rustgis_data::source::geojson::IN_PLACE_ABOVE as usize + 1, b' ');
    std::fs::write(&large, &text).unwrap();
    let e = u.run("layer.addData", &json!({"path": large.display().to_string()})).unwrap_err();
    assert!(e.contains("GeoJSON") && !e.contains("too large"), "{e}");
    let _ = std::fs::remove_file(&large);
    // A document that isn't a FeatureCollection is read whole even when asked to stream.
    let one = std::env::temp_dir().join(format!("rustgis-engine-{}-one.geojson", std::process::id()));
    std::fs::write(&one, r#"{"type":"Point","coordinates":[1,2]}"#).unwrap();
    let r = s.run("layer.addData", &json!({"path": one.display().to_string(), "stream": true})).unwrap();
    assert!(!s.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().is_streamed());
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(one);
}

/// A Shapefile of `n` points along the equator, numbered.
fn line_of_points_shp(name: &str, n: usize) -> std::path::PathBuf {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
    let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
    for i in 0..n {
        ds.push(Some(Geometry::Point(Coord::new(-179.0 + 358.0 * i as f64 / n.max(2) as f64, 0.0))), vec![V::Int(i as i64)]);
    }
    let p = std::env::temp_dir().join(format!("rustgis-engine-{}-{name}.shp", std::process::id()));
    rustgis_data::io::write_path(&ds, &p).unwrap();
    p
}

#[test]
fn large_shapefiles_are_read_in_place() {
    // Small: loaded whole, so it can be edited.
    let small = line_of_points_shp("small-shp", 50);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": small.display().to_string()})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(!s.map().layer(id).unwrap().is_streamed());
    assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 50);

    // Past the streaming threshold: read in place, a view at a time.
    let n = rustgis_map::source::STREAM_ABOVE as usize + 1;
    let big = line_of_points_shp("big-shp", n);
    let mut s = Session::new(Project::default());
    let r = s.run("layer.addData", &json!({"path": big.display().to_string()})).unwrap();
    assert_eq!(r["streamed"], true, "{r}");
    assert_eq!(r["features"], n as u64, "{r}");
    let id = r["layer"].as_u64().unwrap() as u32;
    let v = view_of(&mut s, rustgis_geom::Extent::new(-10.0, -5.0, 10.0, 5.0));
    crate::render::render_png(s.map_mut(), &v).unwrap();
    let l = s.map().layer(id).unwrap();
    assert!(l.is_streamed());
    assert!(!l.dataset.features.is_empty() && l.dataset.features.len() < n);

    // Saved as its file, opened again in place.
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(text.len() < 20_000, "{}", text.len());
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert!(t.map().layers[0].is_streamed() && t.map().layers[0].source.as_ref().unwrap().is_open());

    // Load All Features: every point, editable.
    let r = s.run("layer.loadAll", &json!({"layer": id})).unwrap();
    assert_eq!(r["features"], n as u64);
    assert!(!s.map().layer(id).unwrap().is_streamed());
    // "stream": false loads even a large one whole.
    let mut u = Session::new(Project::default());
    let r = u.run("layer.addData", &json!({"path": big.display().to_string(), "stream": false})).unwrap();
    assert!(!u.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().is_streamed());
    for p in [small, big] {
        for ext in ["shp", "shx", "dbf", "prj", "cpg"] {
            let _ = std::fs::remove_file(p.with_extension(ext));
        }
    }
}

/// A PostGIS layer in a project names its connection without the password, and opening the
/// project never sends the user's own credentials: a server that wants one is reported, with how
/// to connect again.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn postgis_layers_reopen_without_secrets() {
    // Nothing listens on port 1: the connection is refused at once.
    let mut s = Session::new(Project::default());
    let mut src: rustgis_map::source::SourceLayer = serde_json::from_value(json!({
        "postgis": {"database": "postgres:host=127.0.0.1 port=1 dbname=gis user=me", "table": "roads", "family": "Lines"}
    }))
    .unwrap();
    let e = src.open().unwrap_err();
    assert!(e.contains("connect again with Add Data › Database"), "{e}");
    assert!(!src.is_open());
    // Saved again, it keeps its table and never a password.
    let saved = serde_json::to_string(&src).unwrap();
    assert!(saved.contains("\"table\":\"roads\"") && !saved.contains("password"), "{saved}");
    // A connection that isn't there fails as an error, never a panic, and adds nothing.
    let before = s.map().layers.len();
    let e = s.run("layer.addDatabase", &json!({"database": "postgres:host=127.0.0.1 port=1 dbname=gis", "password": "pw"})).unwrap_err();
    assert!(e.starts_with("gis: ") && !e.contains("pw"), "{e}");
    let e = s.run("info.databaseTables", &json!({"database": "postgresql://u:pw@127.0.0.1:1/gis"})).unwrap_err();
    assert!(!e.contains(":pw"), "{e}");
    assert_eq!(s.map().layers.len(), before);
}

/// A live PostGIS database through the commands: `RUSTGIS_TEST_POSTGIS="postgres:host=… dbname=…
/// user=… password=…"` names one with PostGIS's `us_cities`-style tables (any will do); every
/// table is added, drawn, saved and opened again.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore]
fn postgis_live_end_to_end() {
    let Ok(dsn) = std::env::var("RUSTGIS_TEST_POSTGIS") else { return };
    let mut s = Session::new(Project::default());
    let listed = s.run("info.databaseTables", &json!({"database": dsn})).unwrap();
    let names: Vec<String> = listed["layers"].as_array().unwrap().iter().filter_map(|v| v.as_str()).map(str::to_string).collect();
    assert!(!names.is_empty());
    let r = s.run("layer.addDatabase", &json!({"database": dsn, "layer": names[0]})).unwrap();
    assert!(r["features"].as_u64().unwrap() > 0, "{r}");
    // Streamed when asked: saved without its features or the password, read again when opened.
    let r = s.run("layer.addDatabase", &json!({"database": dsn, "layer": names[0], "stream": true, "name": "live"})).unwrap();
    assert_eq!(r["streamed"], true);
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().postgis.is_some());
    let dir = std::env::temp_dir().join(format!("rustgis-engine-postgis-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("map.png");
    s.run("share.exportMap", &json!({"path": png.display().to_string()})).unwrap();
    assert!(std::fs::metadata(&png).unwrap().len() > 1000);
    let text = serde_json::to_string(&s.project).unwrap();
    assert!(!text.contains("password"), "the project keeps no password");
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let l = t.map().layer_by_name("live").unwrap();
    assert!(l.source.as_ref().unwrap().is_open(), "{:?}", l.source.as_ref().unwrap().error);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A MySQL or MariaDB layer in a project names its connection without the password, and opening
/// the project never sends one it wasn't given this session (`MYSQL_PWD` included).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn mysql_layers_reopen_without_secrets() {
    let mut s = Session::new(Project::default());
    let mut src: rustgis_map::source::SourceLayer = serde_json::from_value(json!({
        "mysql": {"database": "mysql://me@127.0.0.1:1/gis?ssl-mode=DISABLED", "table": "roads", "family": "Lines"}
    }))
    .unwrap();
    let e = src.open().unwrap_err();
    assert!(e.contains("connect again with Add Data › Database"), "{e}");
    assert!(!src.is_open());
    let saved = serde_json::to_string(&src).unwrap();
    assert!(saved.contains("\"mysql\":{") && saved.contains("\"table\":\"roads\"") && !saved.contains("password"), "{saved}");
    // A connection that isn't there fails as an error, never a panic, and adds nothing; the
    // password, in the address or given beside it, is in no message.
    let before = s.map().layers.len();
    let e = s.run("layer.addDatabase", &json!({"database": "mysql://me:s3cret@127.0.0.1:1/gis", "password": "pw"})).unwrap_err();
    assert!(e.starts_with("gis: ") && !e.contains("s3cret") && !e.contains("pw"), "{e}");
    let e = s.run("info.databaseTables", &json!({"database": "mariadb://u:s3cret@127.0.0.1:1"})).unwrap_err();
    assert!(e.starts_with("mysql: ") && !e.contains("s3cret"), "{e}");
    // An address that can't be read is said so.
    let e = s.run("layer.addDatabase", &json!({"database": "mysql://h:port/gis"})).unwrap_err();
    assert!(e.contains("port 'port'"), "{e}");
    // SQL queries don't run on one.
    let e = s.run("layer.addSql", &json!({"sql": "SELECT 1", "database": "mysql://h/gis"})).unwrap_err();
    assert!(e.contains("MySQL"), "{e}");
    assert_eq!(s.map().layers.len(), before);
}

/// A live MySQL or MariaDB database through the commands: `RUSTGIS_TEST_MYSQL="mysql://user:
/// password@host:port/database"` names one with tables of geometry (any will do); every table is
/// added, drawn, saved and opened again.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore]
fn mysql_live_end_to_end() {
    let Ok(address) = std::env::var("RUSTGIS_TEST_MYSQL") else { return };
    let mut s = Session::new(Project::default());
    let listed = s.run("info.databaseTables", &json!({"database": address})).unwrap();
    let names: Vec<String> = listed["layers"].as_array().unwrap().iter().filter_map(|v| v.as_str()).map(str::to_string).collect();
    assert!(!names.is_empty());
    // Every table: a column of mixed types becomes a layer per type.
    let r = s.run("layer.addDatabase", &json!({"database": address})).unwrap();
    assert!(r["layers"].as_array().unwrap().len() >= names.len(), "{r}");
    assert!(r["features"].as_u64().unwrap() > 0, "{r}");
    // Streamed when asked: saved without its features or the password, read again when opened.
    let r = s.run("layer.addDatabase", &json!({"database": address, "layer": names[0], "stream": true, "name": "live"})).unwrap();
    assert_eq!(r["streamed"], true);
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().mysql.is_some());
    let dir = std::env::temp_dir().join(format!("rustgis-engine-mysql-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("map.png");
    s.run("share.exportMap", &json!({"path": png.display().to_string()})).unwrap();
    assert!(std::fs::metadata(&png).unwrap().len() > 1000);
    let text = serde_json::to_string(&s.project).unwrap();
    let password = address.split_once("://").and_then(|(_, r)| r.split_once('@')).and_then(|(u, _)| u.split_once(':')).map(|(_, p)| p);
    assert!(password.is_none_or(|p| !text.contains(&format!(":{p}@"))), "the project keeps no password");
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let l = t.map().layer_by_name("live").unwrap();
    assert!(l.source.as_ref().unwrap().is_open(), "{:?}", l.source.as_ref().unwrap().error);
    let _ = std::fs::remove_dir_all(&dir);
}

/// SQL Server project reopening and failed commands never expose credentials or add layers.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn sqlserver_layers_reopen_without_secrets() {
    let mut s = Session::new(Project::default());
    let mut src: rustgis_map::source::SourceLayer = serde_json::from_value(json!({
        "sqlserver": {"database": "mssql://me@127.0.0.1:1/gis?encrypt=false", "table": "roads", "family": "Lines"}
    }))
    .unwrap();
    let e = src.open().unwrap_err();
    assert!(e.contains("connect again with Add Data › Database"), "{e}");
    assert!(!src.is_open());
    let saved = serde_json::to_string(&src).unwrap();
    assert!(saved.contains("\"sqlserver\":{") && saved.contains("\"table\":\"roads\"") && !saved.contains("password"), "{saved}");
    let before = s.map().layers.len();
    let e = s.run("layer.addDatabase", &json!({"database": "mssql://me:s3cret@127.0.0.1:1/gis", "password": "pw"})).unwrap_err();
    assert!(e.starts_with("gis: ") && !e.contains("s3cret") && !e.contains("pw"), "{e}");
    let e = s.run("layer.addDatabase", &json!({"database": "mssql://h:port/gis"})).unwrap_err();
    assert!(e.contains("port 'port'"), "{e}");
    let e = s.run("layer.addSql", &json!({"sql": "SELECT 1", "database": "mssql://h/gis"})).unwrap_err();
    assert!(e.contains("SQL Server"), "{e}");
    #[cfg(not(windows))]
    {
        let e = s.run("info.databaseTables", &json!({"database": "mssql://127.0.0.1:1/gis"})).unwrap_err();
        assert!(e.contains("give a SQL Server login"), "{e}");
    }
    assert_eq!(s.map().layers.len(), before);
}

/// `RUSTGIS_TEST_MSSQL` names a live database: add every table, stream one, draw, save and reopen.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore]
fn sqlserver_live_end_to_end() {
    let Ok(address) = std::env::var("RUSTGIS_TEST_MSSQL") else { return };
    let mut s = Session::new(Project::default());
    let listed = s.run("info.databaseTables", &json!({"database": address})).unwrap();
    let names: Vec<String> = listed["layers"].as_array().unwrap().iter().filter_map(|v| v.as_str()).map(str::to_string).collect();
    assert!(!names.is_empty());
    let r = s.run("layer.addDatabase", &json!({"database": address})).unwrap();
    assert!(r["layers"].as_array().unwrap().len() >= names.len(), "{r}");
    assert!(r["features"].as_u64().unwrap() > 0, "{r}");
    let r = s.run("layer.addDatabase", &json!({"database": address, "layer": names[0], "stream": true, "name": "live"})).unwrap();
    assert_eq!(r["streamed"], true);
    let id = r["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().sqlserver.is_some());
    let dir = std::env::temp_dir().join(format!("rustgis-engine-sqlserver-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    s.run("share.exportMap", &json!({"path": dir.join("map.png").display().to_string()})).unwrap();
    let text = serde_json::to_string(&s.project).unwrap();
    let password = rustgis_data::source::msconn::split_password(&address).and_then(|(_, p)| p);
    assert!(password.is_none_or(|p| !text.contains(&p)), "the project keeps no password");
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let l = t.map().layer_by_name("live").unwrap();
    assert!(l.source.as_ref().unwrap().is_open(), "{:?}", l.source.as_ref().unwrap().error);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Deleting a map or layout other than the one in view (a tab's close button) keeps that view;
/// deleting the one in view falls back to the first map; the last map stays.
#[test]
fn removing_another_map_or_layout_keeps_the_one_in_view() {
    let mut s = world();
    for n in ["B", "C"] {
        s.run("map.new", &json!({"name": n})).unwrap();
    }
    let active = |s: &Session| s.map().name.clone();
    assert_eq!(active(&s), "C");
    s.run("map.remove", &json!({"name": "B"})).unwrap();
    assert_eq!(active(&s), "C", "a map before the active one");
    s.run("map.new", &json!({"name": "D"})).unwrap();
    s.run("map.activate", &json!({"name": "C"})).unwrap();
    s.run("map.remove", &json!({"name": "D"})).unwrap();
    assert_eq!(active(&s), "C", "a map after the active one");
    let first = s.project.maps[0].name.clone();
    s.run("map.remove", &json!({"name": "C"})).unwrap();
    assert_eq!((active(&s), s.project.maps.len()), (first.clone(), 1), "the active map itself");
    assert!(s.run("map.remove", &json!({"name": first})).is_err(), "the last map");
    assert_eq!(s.project.maps.len(), 1);

    for n in ["L1", "L2", "L3"] {
        s.run("layout.new", &json!({"name": n})).unwrap();
    }
    let open = |s: &Session| s.active_layout.and_then(|i| s.project.layouts.get(i)).map(|l| l.name.clone());
    s.run("layout.activate", &json!({"name": "L2"})).unwrap();
    s.run("layout.remove", &json!({"name": "L1"})).unwrap();
    assert_eq!(open(&s).as_deref(), Some("L2"), "a layout before the open one");
    s.run("layout.remove", &json!({"name": "L3"})).unwrap();
    assert_eq!(open(&s).as_deref(), Some("L2"), "a layout after the open one");
    s.run("layout.remove", &json!({"name": "L2"})).unwrap();
    assert_eq!((open(&s), s.project.layouts.len()), (None, 0), "the open layout itself");
    s.run("layout.new", &json!({"name": "L4"})).unwrap();
    s.run("layout.close", &Value::Null).unwrap();
    s.run("layout.remove", &json!({"name": "L4"})).unwrap();
    assert_eq!(open(&s), None, "with the map in view, it stays in view");
}

/// A map's name finds one map (tabs, Browser, map frames): renaming to another map's name adds a
/// number, and a map keeps its own name in another case.
#[test]
fn map_rename_keeps_map_names_unique() {
    let mut s = world();
    let first = s.map().name.clone();
    s.run("map.new", &json!({"name": "B"})).unwrap();
    let r = s.run("map.rename", &json!({"name": first.to_uppercase()})).unwrap();
    assert_eq!(r["name"], format!("{} 2", first.to_uppercase()));
    s.run("map.new", &json!({"name": "C"})).unwrap();
    assert_eq!(s.run("map.rename", &json!({"name": format!("{} 2", first)})).unwrap()["name"], format!("{first} 2 2"));
    let own = format!("{} 2 2", first.to_lowercase());
    assert_eq!(s.run("map.rename", &json!({"name": own})).unwrap()["name"], own.as_str(), "its own name, in another case");
    let names: Vec<String> = s.project.maps.iter().map(|m| m.name.to_lowercase()).collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "{names:?}");
    for p in [json!({}), json!({"name": "  "}), json!({"name": 3})] {
        assert!(s.run("map.rename", &p).is_err(), "{p}");
    }
    // A long name keeps to 200 characters, number included.
    let long = "é".repeat(300);
    s.run("map.rename", &json!({"name": long})).unwrap();
    s.run("map.new", &Value::Null).unwrap();
    let r = s.run("map.rename", &json!({"name": long})).unwrap();
    let name = r["name"].as_str().unwrap_or_default();
    assert_eq!((name.chars().count(), name.ends_with(" 2")), (200, true), "{name}");
}

#[test]
fn bookmarks_export_and_import_across_projections() {
    let mut s = Session::new(sample::world_project());
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 5_000_000})).unwrap();
    s.run("bookmark.add", &json!({"name": "Here"})).unwrap();
    let dir = std::env::temp_dir().join(format!("rustgis-bookmarks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("marks.rgisbookmarks").display().to_string();
    s.run("bookmark.save", &json!({"path": file})).unwrap();
    let before = s.map().bookmarks.len();
    s.run("bookmark.remove", &json!({"name": "Here"})).unwrap();
    assert_eq!(s.map().bookmarks.len(), before - 1);
    // Imported into a map in another projection, the bookmark still shows the same place.
    s.run("map.projection", &json!({"type": "europeEqualArea"})).unwrap();
    let r = s.run("bookmark.import", &json!({"path": file})).unwrap();
    // Places a Europe-only projection can't show are left out; same-named bookmarks are replaced.
    let added = r["added"].as_array().unwrap();
    assert!(added.iter().any(|n| n == "Here") && !added.iter().any(|n| n == "Americas"), "{added:?}");
    assert_eq!(s.map().bookmarks.len(), before);
    // Re-importing replaces bookmarks where they are, and names each once.
    let order: Vec<String> = s.map().bookmarks.iter().map(|b| b.name.clone()).collect();
    let twice = json!({"bookmarks": [{"name": "Europe", "lon": 10, "lat": 50, "metresPerPixel": 1000}, {"name": "europe", "lon": 11, "lat": 50, "metresPerPixel": 1000}]});
    let r = s.run("bookmark.import", &json!({"text": twice.to_string()})).unwrap();
    assert_eq!(r["added"], json!(["Europe"]));
    assert_eq!(s.map().bookmarks.iter().map(|b| b.name.clone()).collect::<Vec<_>>(), order);
    s.run("bookmark.go", &json!({"name": "Here"})).unwrap();
    let c = s.map().center_lonlat();
    assert!((c.x - 10.0).abs() < 1e-6 && (c.y - 50.0).abs() < 1e-6, "{c:?}");
    assert!((s.scale() / 5_000_000.0 - 1.0).abs() < 0.02, "{}", s.scale());
    // Junk is refused, and junk entries are skipped.
    assert!(s.run("bookmark.import", &json!({"text": "{}"})).is_err());
    let junk = json!({"bookmarks": [{"name": "Bad", "lon": 500, "lat": 0, "metresPerPixel": 1}, {"name": "", "lon": 0, "lat": 0, "metresPerPixel": 1}]});
    assert!(s.run("bookmark.import", &json!({"text": junk.to_string()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The map turns to a bearing (#395): set, turned further, reset, kept by bookmarks (and their
/// files), saved with the project, and reported by project info.
#[test]
fn the_map_turns_to_a_bearing() {
    let mut s = Session::new(sample::world_project());
    assert_eq!(s.map().camera.bearing, 0.0);
    assert_eq!(s.run("map.setBearing", &json!({"bearing": 90})).unwrap(), json!({"bearing": 90.0}));
    assert_eq!(s.run("map.setBearing", &json!({"by": -100})).unwrap(), json!({"bearing": -10.0}));
    assert_eq!(s.run("map.setBearing", &json!({"bearing": 350})).unwrap(), json!({"bearing": -10.0}));
    assert!(s.run("map.setBearing", &json!({})).is_err());
    assert!(s.run("map.setBearing", &json!({"bearing": "north"})).is_err());
    assert!(s.run("map.setBearing", &json!({"bearing": "north", "by": 10})).is_err());
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 50.0, "scale": 5_000_000, "bearing": 45})).unwrap();
    assert_eq!(s.map().camera.bearing, 45.0);
    assert!(s.run("map.goToXY", &json!({"lon": 0.0, "lat": 0.0, "bearing": "east"})).is_err());
    assert!((s.map().center_lonlat().x - 10.0).abs() < 1e-6, "a refused bearing doesn't move the map");
    // Previous Extent turns the map back.
    s.run("map.setBearing", &json!({"bearing": 0})).unwrap();
    s.run("map.prevExtent", &json!({})).unwrap();
    assert_eq!(s.map().camera.bearing, 45.0);
    assert_eq!(s.run("info.project", &json!({})).unwrap()["bearing"], json!(45.0));
    // A bookmark keeps the bearing, in the project and in a bookmarks file.
    s.run("bookmark.add", &json!({"name": "Turned"})).unwrap();
    s.run("map.resetNorth", &json!({})).unwrap();
    assert_eq!(s.map().camera.bearing, 0.0);
    s.run("bookmark.go", &json!({"name": "Turned"})).unwrap();
    assert_eq!(s.map().camera.bearing, 45.0);
    let file = crate::cmd::navigate::bookmarks_file(s.map()).unwrap();
    s.run("map.resetNorth", &json!({})).unwrap();
    s.run("bookmark.remove", &json!({"name": "Turned"})).unwrap();
    s.run("bookmark.import", &json!({"text": String::from_utf8(file).unwrap()})).unwrap();
    s.run("bookmark.go", &json!({"name": "Turned"})).unwrap();
    assert!((s.map().camera.bearing - 45.0).abs() < 1e-9);
    // Saved with the project.
    let saved: crate::Project = serde_json::from_str(&serde_json::to_string(&s.project).unwrap()).unwrap();
    assert!((saved.maps[saved.active_map].camera.bearing - 45.0).abs() < 1e-9);
    // The visible extent holds the turned view's corners: wider than the same view facing north.
    let turned = crate::cmd::workspace::view_extent(&s);
    s.run("map.resetNorth", &json!({})).unwrap();
    let north = crate::cmd::workspace::view_extent(&s);
    assert!(turned.width() > north.width() * 1.1, "{turned:?} vs {north:?}");
}

/// The map tilts (#395 phase 2): set, tilted further, kept within 0 to 85, reset with the
/// bearing, kept by bookmarks and their files, in Previous Extent and in project info; a tilted
/// map exports.
#[test]
fn the_map_tilts() {
    let mut s = Session::new(sample::world_project());
    assert_eq!(s.run("map.setPitch", &json!({"pitch": 50})).unwrap(), json!({"pitch": 50.0}));
    assert_eq!(s.run("map.setPitch", &json!({"by": 50})).unwrap(), json!({"pitch": 85.0}));
    assert_eq!(s.run("map.setPitch", &json!({"by": -100})).unwrap(), json!({"pitch": 0.0}));
    assert!(s.run("map.setPitch", &json!({})).is_err());
    assert!(s.run("map.setPitch", &json!({"pitch": "steep", "by": 5})).is_err());
    assert_eq!(s.run("map.setPitch", &json!({"by": "up"})).unwrap_err(), "by must be a number of degrees");
    // Huge pitches are kept to the limit; no number (NaN, infinity) is refused.
    assert_eq!(s.run("map.setPitch", &json!({"pitch": 1e300})).unwrap(), json!({"pitch": 85.0}));
    assert_eq!(s.run("map.setPitch", &json!({"pitch": -1e300})).unwrap(), json!({"pitch": 0.0}));
    for junk in [json!("NaN"), json!("inf"), json!("-infinity"), json!([1]), json!({})] {
        assert!(s.run("map.setPitch", &json!({"pitch": junk})).is_err(), "{junk}");
        // A refused Go To XY leaves the view where it was.
        let before = s.map().camera;
        assert!(s.run("map.goToXY", &json!({"lon": 30, "lat": 30, "pitch": junk})).is_err(), "{junk}");
        assert_eq!(s.map().camera, before);
    }
    assert!(s.run("map.goToXY", &json!({"lon": 0, "lat": 0, "pitch": "steep"})).is_err());
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 48.0, "scale": 10_000_000, "pitch": 45, "bearing": 20})).unwrap();
    assert_eq!((s.map().camera.pitch, s.map().camera.bearing), (45.0, 20.0));
    assert_eq!(s.run("info.project", &json!({})).unwrap()["pitch"], json!(45.0));
    // A bookmark and its file keep the pitch.
    s.run("bookmark.add", &json!({"name": "Tilted"})).unwrap();
    let file = crate::cmd::navigate::bookmarks_file(s.map()).unwrap();
    s.run("map.resetNorth", &json!({})).unwrap();
    assert_eq!((s.map().camera.pitch, s.map().camera.bearing), (0.0, 0.0));
    // Previous Extent tilts it back.
    s.run("map.prevExtent", &json!({})).unwrap();
    assert_eq!(s.map().camera.pitch, 45.0);
    s.run("map.resetNorth", &json!({})).unwrap();
    s.run("bookmark.remove", &json!({"name": "Tilted"})).unwrap();
    s.run("bookmark.import", &json!({"text": String::from_utf8(file).unwrap()})).unwrap();
    s.run("bookmark.go", &json!({"name": "Tilted"})).unwrap();
    assert!((s.map().camera.pitch - 45.0).abs() < 1e-9);
    // A tilted map exports.
    let dir = std::env::temp_dir().join(format!("rustgis-tilt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("tilted.png");
    s.run("share.exportMap", &json!({"path": png.display().to_string(), "width": 320, "height": 200})).unwrap();
    assert!(std::fs::metadata(&png).unwrap().len() > 1000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bookmark manager's commands (#351): names stay unique, a bookmark is renamed, updated,
/// described and moved in place, Pan To keeps the scale, and any map's bookmarks are reachable.
#[test]
fn bookmarks_are_managed_in_place_on_any_map() {
    let mut s = world();
    let names = |s: &Session| s.map().bookmarks.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
    let first = names(&s);
    assert!(first.contains(&"Europe".to_string()), "{first:?}");
    // A taken name is numbered instead of replacing the bookmark that has it.
    assert_eq!(s.run("bookmark.add", &json!({"name": "europe"})).unwrap()["name"], "europe 2");
    assert_eq!(s.run("bookmark.add", &json!({"name": "Europe"})).unwrap()["name"], "Europe 3");
    assert_eq!(names(&s).len(), first.len() + 2);
    // A long name that is taken keeps its number within the 120 characters.
    let long = "x".repeat(150);
    let mut longs = Vec::new();
    for _ in 0..2 {
        longs.push(s.run("bookmark.add", &json!({"name": long})).unwrap()["name"].as_str().unwrap().to_string());
    }
    assert_eq!(longs.iter().map(|n| n.chars().count()).collect::<Vec<_>>(), [120, 120]);
    assert!(longs[1].ends_with(" 2") && longs[0] != longs[1], "{longs:?}");
    for n in &longs {
        s.run("bookmark.remove", &json!({"name": n})).unwrap();
    }
    // Rename: unique among the others, its own name in another case is fine, one undo step.
    assert_eq!(s.run("bookmark.rename", &json!({"name": "Europe 3", "newName": "World"})).unwrap()["name"], "World 2");
    assert_eq!(s.run("bookmark.rename", &json!({"name": "World 2", "newName": "WORLD 2"})).unwrap()["name"], "WORLD 2");
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(names(&s).contains(&"World 2".to_string()));
    assert!(s.run("bookmark.rename", &json!({"name": "Nowhere", "newName": "X"})).is_err());
    assert!(s.run("bookmark.rename", &json!({"name": "World 2", "newName": "  "})).is_err());
    // Update takes the current view, keeping the bookmark's place in the list.
    let at = names(&s).iter().position(|n| n == "Africa");
    s.run("map.goToXY", &json!({"lon": 100.0, "lat": -5.0, "scale": 2_000_000})).unwrap();
    s.run("bookmark.update", &json!({"name": "africa"})).unwrap();
    assert_eq!(names(&s).iter().position(|n| n == "Africa"), at);
    assert_eq!(s.map().bookmarks.iter().find(|b| b.name == "Africa").map(|b| b.camera), Some(s.map().camera));
    // A description, trimmed and kept to 2000 characters; empty clears it.
    s.run("bookmark.describe", &json!({"name": "Africa", "description": "  Indonesia, really  "})).unwrap();
    assert_eq!(s.map().bookmarks.iter().find(|b| b.name == "Africa").map(|b| b.description.as_str()), Some("Indonesia, really"));
    s.run("bookmark.describe", &json!({"name": "Africa", "description": "é".repeat(3000)})).unwrap();
    assert_eq!(s.map().bookmarks.iter().find(|b| b.name == "Africa").map(|b| b.description.chars().count()), Some(2000));
    // Move: to the front, and past the end to the back.
    s.run("bookmark.move", &json!({"name": "Africa", "to": 0})).unwrap();
    assert_eq!(names(&s).first().map(String::as_str), Some("Africa"));
    s.run("bookmark.move", &json!({"name": "Africa", "to": 999})).unwrap();
    assert_eq!(names(&s).last().map(String::as_str), Some("Africa"));
    for p in [json!({"name": "Africa"}), json!({"name": "Africa", "to": -1}), json!({"name": "Africa", "to": "x"})] {
        assert!(s.run("bookmark.move", &p).is_err(), "{p}");
    }
    // Pan To moves to the place at the current scale; Go To takes the bookmark's scale too.
    s.run("map.goToXY", &json!({"lon": 0.0, "lat": 0.0, "scale": 40_000_000})).unwrap();
    let scale = s.scale();
    s.run("bookmark.go", &json!({"name": "Europe", "pan": true})).unwrap();
    assert!((s.scale() / scale - 1.0).abs() < 1e-6, "Pan To keeps the scale");
    s.run("bookmark.go", &json!({"name": "Europe"})).unwrap();
    assert!((s.scale() / scale - 1.0).abs() > 0.01, "Go To takes the bookmark's scale");
    // Another map's bookmark is reached by `map`.
    let world_map = s.map().name.clone();
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    assert!(s.run("bookmark.go", &json!({"name": "Europe"})).is_err(), "not on this map");
    s.run("bookmark.add", &json!({"name": "Here", "map": world_map, "description": "added from Second"})).unwrap();
    // Going to another map's bookmark moves the map in view to its place (as desktop GIS do): Second
    // stays in view, at the place and scale World shows for it.
    s.run("bookmark.go", &json!({"name": "Here", "map": world_map})).unwrap();
    assert_eq!(s.map().name, "Second");
    let (there, there_scale) = (s.map().center_lonlat(), s.scale());
    assert!(s.run("bookmark.go", &json!({"name": "Here", "map": 0})).is_err(), "a map that isn't a name");
    // Update takes the view on screen, so only with the bookmark's map in view; describing works
    // from anywhere.
    assert!(s.run("bookmark.update", &json!({"name": "Here", "map": world_map})).is_err());
    s.run("bookmark.describe", &json!({"name": "Here", "map": world_map, "description": "added from Second"})).unwrap();
    s.run("map.activate", &json!({"name": world_map})).unwrap();
    s.run("bookmark.go", &json!({"name": "Here"})).unwrap();
    let here = s.map().center_lonlat();
    assert!((here.x - there.x).abs() < 1e-6 && (here.y - there.y).abs() < 1e-6, "{here:?} {there:?}");
    assert!((s.scale() / there_scale - 1.0).abs() < 0.02, "{} {there_scale}", s.scale());
    // The description travels in a bookmarks file.
    let file = super::cmd::navigate::bookmarks_file(s.map()).unwrap();
    let text = String::from_utf8(file).unwrap();
    assert!(text.contains("added from Second"), "{text}");
    s.run("bookmark.remove", &json!({"name": "Here"})).unwrap();
    s.run("bookmark.import", &json!({"text": text})).unwrap();
    let described = |s: &Session| s.map().bookmarks.iter().find(|b| b.name == "Here").map(|b| b.description.clone());
    assert_eq!(described(&s).as_deref(), Some("added from Second"));
    // A file without descriptions (older, or written by hand) leaves the one a bookmark has; an
    // empty one clears it.
    let mut old: Value = serde_json::from_str(&text).unwrap();
    for b in old["bookmarks"].as_array_mut().unwrap() {
        b.as_object_mut().unwrap().remove("description");
    }
    s.run("bookmark.import", &json!({"text": old.to_string()})).unwrap();
    assert_eq!(described(&s).as_deref(), Some("added from Second"));
    for b in old["bookmarks"].as_array_mut().unwrap() {
        b["description"] = json!("");
    }
    s.run("bookmark.import", &json!({"text": old.to_string()})).unwrap();
    assert_eq!(described(&s).as_deref(), Some(""));
    // Unnamed, a bookmark takes the next `Bookmark N` free, never `Bookmark N 2` (the New
    // Bookmark dialog offers the same name).
    let n = s.map().bookmarks.len();
    s.run("bookmark.add", &json!({"name": format!("Bookmark {}", n + 2)})).unwrap();
    assert_eq!(super::cmd::navigate::new_bookmark_name(s.map()), format!("Bookmark {}", n + 3));
    assert_eq!(s.run("bookmark.add", &Value::Null).unwrap()["name"], format!("Bookmark {}", n + 3));
    // "Bookmark 03" isn't "Bookmark 3".
    s.run("bookmark.add", &json!({"name": format!("Bookmark 0{}", n + 4)})).unwrap();
    assert_eq!(super::cmd::navigate::new_bookmark_name(s.map()), format!("Bookmark {}", n + 4));
    // A map keeps at most 10,000 bookmarks, added or imported.
    let camera = s.map().camera;
    let marks = &mut s.map_mut().bookmarks;
    while marks.len() < 10_000 {
        let name = format!("Bookmark {}", marks.len() + 1);
        marks.push(rustgis_map::Bookmark::new(name, camera));
    }
    assert!(s.run("bookmark.add", &Value::Null).is_err());
    assert_eq!(s.map().bookmarks.len(), 10_000);
}

#[test]
fn map_rename_names_any_map_and_what_shows_it_follows() {
    use rustgis_map::layout::ElementKind;
    use rustgis_map::presentation::PageKind;
    let mut s = world();
    let first = s.map().name.clone();
    s.run("layout.new", &json!({"page": "a4"})).unwrap();
    s.run("presentation.new", &Value::Null).unwrap();
    s.run("presentation.mapPage", &Value::Null).unwrap();
    s.run("offline.download", &json!({"name": "Paris", "extent": [1.0, 48.0, 4.0, 50.0]})).unwrap();
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    // The Browser renames a map that isn't in view, which stays out of view.
    let r = s.run("map.rename", &json!({"map": first.to_uppercase(), "name": "Atlas"})).unwrap();
    assert_eq!((r["name"].as_str(), s.map().name.as_str()), (Some("Atlas"), "Second"));
    let shown = |s: &Session| {
        let frames: Vec<String> = s.project.layouts[0]
            .elements
            .iter()
            .filter_map(|e| match &e.kind {
                ElementKind::MapFrame { map, .. } => Some(map.clone()),
                _ => None,
            })
            .collect();
        let pages: Vec<String> = s.project.presentations[0]
            .pages
            .iter()
            .filter_map(|p| match &p.kind {
                PageKind::Map { map, .. } => Some(map.clone()),
                _ => None,
            })
            .collect();
        (frames, pages, s.project.offline_areas[0].map.clone())
    };
    assert_eq!(shown(&s), (vec!["Atlas".to_string()], vec!["Atlas".to_string()], "Atlas".to_string()));
    // One undo step puts the name back everywhere.
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(shown(&s), (vec![first.clone()], vec![first.clone()], first.clone()));
    assert!(s.project.maps.iter().any(|m| m.name == first));
    assert!(s.run("map.rename", &json!({"map": "Nowhere", "name": "X"})).is_err());
    // A selector that isn't a name never falls back to the active map.
    for m in [json!(0), json!(true), json!(["Atlas"])] {
        assert!(s.run("map.rename", &json!({"map": m, "name": "X"})).is_err(), "{m}");
    }
    assert_eq!(s.map().name, "Second");
}

#[test]
fn layout_elements_duplicate_offset_on_top_and_undo() {
    let mut s = world();
    s.run("layout.new", &json!({"name": "D"})).unwrap();
    let li = s.active_layout.unwrap();
    let legend = s.project.layouts[li].elements.iter().find(|e| e.name == "Legend").unwrap().clone();
    let r = s.run("layout.duplicateElement", &json!({"element": legend.id})).unwrap();
    let id = r["element"].as_u64().unwrap() as u32;
    let l = &s.project.layouts[li];
    let copy = l.element(id).unwrap();
    // A new id and name, the same kind and size, offset 5 mm, drawn on top.
    assert_ne!(id, legend.id);
    assert_eq!((copy.name.as_str(), copy.x, copy.y, copy.w, copy.h), ("Legend 2", legend.x + 5.0, legend.y + 5.0, legend.w, legend.h));
    assert_eq!(copy.kind, legend.kind);
    assert_eq!(l.elements.last().map(|e| e.id), Some(id));
    // A given offset; the next copy takes the next free name.
    let r = s.run("layout.duplicateElement", &json!({"element": legend.id, "dx": 0, "dy": -20})).unwrap();
    assert_eq!(r["name"], "Legend 3");
    let third_id = r["element"].as_u64().unwrap() as u32;
    let third = s.project.layouts[li].element(third_id).unwrap();
    assert_eq!((third.x, third.y), (legend.x, legend.y - 20.0));
    for bad in [json!({}), json!({"element": 9999}), json!({"element": legend.id, "dx": 1e9})] {
        assert!(s.run("layout.duplicateElement", &bad).is_err(), "{bad}");
    }
    s.run("edit.undo", &Value::Null).unwrap();
    let l = &s.project.layouts[li];
    assert!(l.element(third_id).is_none() && l.element(id).is_some(), "one undo removes the last copy only");
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.project.layouts[li].element(id).is_none(), "undone");
}

#[test]
fn locked_layout_elements_take_no_change_until_unlocked() {
    use rustgis_map::layout::ElementKind;
    let mut s = world();
    s.run("layout.new", &json!({"name": "L"})).unwrap();
    let li = s.active_layout.unwrap();
    let frame = s.project.layouts[li].first_frame().unwrap();
    let legend = s.project.layouts[li].elements.iter().find(|e| e.name == "Legend").unwrap().id;
    let title = s.run("layout.addElement", &json!({"kind": "text", "text": "Title", "x": 10, "y": 10})).unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    s.run("layout.update", &json!({"element": title, "locked": true})).unwrap();
    let before = s.project.layouts[li].element(title).unwrap().clone();
    assert!(before.locked);
    // Every change is refused, and the element stays as it was.
    for bad in [
        json!({"x": 50}),
        json!({"w": 80, "h": 20}),
        json!({"text": "Oops"}),
        json!({"name": "T"}),
        json!({"visible": false}),
        json!({"locked": true, "x": 1}),
    ] {
        let mut p = bad.clone();
        p["element"] = json!(title);
        let err = s.run("layout.update", &p).unwrap_err();
        assert!(err.contains("locked"), "{bad}: {err}");
    }
    assert_eq!(s.project.layouts[li].element(title), Some(&before));
    assert!(s.run("layout.removeElement", &json!({"element": title})).is_err_and(|e| e.contains("locked")));
    assert!(s.project.layouts[li].element(title).is_some());
    // Locking it again, or a null value, changes nothing and isn't refused.
    s.run("layout.update", &json!({"element": title, "locked": true, "x": null})).unwrap();
    // A copy can be made, and it isn't locked.
    let copy = s.run("layout.duplicateElement", &json!({"element": title})).unwrap()["element"].as_u64().unwrap() as u32;
    assert!(!s.project.layouts[li].element(copy).unwrap().locked);
    // Unlocking may come with other changes.
    s.run("layout.update", &json!({"element": title, "locked": false, "x": 50})).unwrap();
    let e = s.project.layouts[li].element(title).unwrap();
    assert_eq!((e.locked, e.x), (false, 50.0));
    // Locking and moving at once: it moves, then locks.
    s.run("layout.update", &json!({"element": title, "x": 60, "locked": true})).unwrap();
    let e = s.project.layouts[li].element(title).unwrap();
    assert_eq!((e.locked, e.x), (true, 60.0));
    // Undone, the lock goes back as any other change does.
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(!s.project.layouts[li].element(title).unwrap().locked);

    // A locked map frame keeps its extent and gets no grid or extent indicator…
    s.run("layout.update", &json!({"element": frame, "locked": true})).unwrap();
    let kind = s.project.layouts[li].element(frame).unwrap().kind.clone();
    assert!(s.run("layout.frameZoom", &json!({"element": frame, "to": "full"})).is_err_and(|e| e.contains("locked")));
    assert!(s.run("layout.update", &json!({"element": frame, "scale": 1_000_000})).is_err_and(|e| e.contains("locked")));
    assert!(s.run("layout.addElement", &json!({"kind": "grid", "frame": frame})).is_err_and(|e| e.contains("locked")));
    let inset = s.run("layout.addFrame", &json!({"x": 150, "y": 150, "w": 40, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;
    assert!(
        s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": frame, "of": inset})).is_err_and(|e| e.contains("locked"))
    );
    assert_eq!(s.project.layouts[li].element(frame).unwrap().kind, kind);
    // …but a map series still turns its pages.
    s.run("layout.update", &json!({"element": frame, "locked": false})).unwrap();
    s.run("layout.series", &json!({"layer": "Lakes", "frame": frame})).unwrap();
    s.run("layout.update", &json!({"element": frame, "locked": true})).unwrap();
    let center = |s: &Session| match &s.project.layouts[li].element(frame).unwrap().kind {
        ElementKind::MapFrame { center, .. } => *center,
        _ => panic!("not a frame"),
    };
    let first = center(&s);
    s.run("layout.seriesPage", &json!({"page": "next"})).unwrap();
    assert_ne!(center(&s), first, "the series moved the locked frame");

    // A frame whose surround is locked isn't deleted: the surround would go with it.
    s.run("layout.update", &json!({"element": frame, "locked": false})).unwrap();
    s.run("layout.update", &json!({"element": legend, "locked": true})).unwrap();
    let err = s.run("layout.removeElement", &json!({"element": frame})).unwrap_err();
    assert!(err.contains("Legend"), "{err}");
    assert!(s.project.layouts[li].element(frame).is_some() && s.project.layouts[li].element(legend).is_some());

    // Saved with the lock, and a layout from before locks loads unlocked.
    let saved = serde_json::to_value(&s.project.layouts[li]).unwrap();
    let back: rustgis_map::layout::Layout = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(back, s.project.layouts[li]);
    let old = saved["elements"].as_array().unwrap().iter().find(|e| e["id"] == json!(copy)).unwrap();
    assert!(old.get("locked").is_none(), "an unlocked element doesn't write the field");
    let mut older = saved.clone();
    for e in older["elements"].as_array_mut().unwrap() {
        e.as_object_mut().unwrap().remove("locked");
    }
    let back: rustgis_map::layout::Layout = serde_json::from_value(older).unwrap();
    assert!(back.elements.iter().all(|e| !e.locked));
}

#[test]
fn layout_shapes_are_added_restyled_and_saved() {
    use rustgis_map::layout::{ElementKind, LineDir};
    let mut s = world();
    s.run("layout.new", &json!({"name": "Shapes", "template": "blank"})).unwrap();
    let li = s.active_layout.unwrap();
    let add = |s: &mut Session, p: Value| s.run("layout.addElement", &p).unwrap()["element"].as_u64().unwrap() as u32;
    let rect = add(&mut s, json!({"kind": "rectangle", "x": 20, "y": 20, "w": 60, "h": 30}));
    let circle = add(&mut s, json!({"kind": "circle"}));
    let ellipse = add(&mut s, json!({"kind": "ellipse", "fill": "#FF000080"}));
    let line = add(&mut s, json!({"kind": "line", "dir": "vertical", "width": 3}));
    let l = &s.project.layouts[li];
    let e = |id: u32| l.element(id).unwrap();
    // A rectangle given a box is a shape; without one it's still the page's neatline.
    assert_eq!((e(rect).name.as_str(), e(rect).x, e(rect).w), ("Rectangle", 20.0, 60.0));
    assert!(matches!(e(rect).kind, ElementKind::Rectangle { .. }));
    assert_eq!(e(circle).name, "Circle");
    assert_eq!(e(circle).w, e(circle).h, "a circle's box is square");
    assert!(e(ellipse).w > e(ellipse).h);
    assert!(matches!(e(ellipse).kind, ElementKind::Ellipse { fill, .. } if fill.a() == 0x80));
    assert!(matches!(e(line).kind, ElementKind::Line { dir: LineDir::Vertical, width, .. } if width == 3.0));
    assert!(e(line).h > e(line).w, "a vertical line's box is tall");

    // Restyled through layout.update; the outline is "outline" or "color".
    s.run("layout.update", &json!({"element": circle, "fill": "#00FF00", "color": "#0000FF", "width": 0})).unwrap();
    s.run("layout.update", &json!({"element": line, "dir": "rising", "color": "#123456"})).unwrap();
    let l = &s.project.layouts[li];
    match &l.element(circle).unwrap().kind {
        ElementKind::Ellipse { fill, outline, width } => {
            assert_eq!((fill.hex(), outline.hex(), *width), ("#00FF00".into(), "#0000FF".into(), 0.0))
        }
        k => panic!("{k:?}"),
    }
    match &l.element(line).unwrap().kind {
        ElementKind::Line { color, width, dir, .. } => assert_eq!((color.hex(), *width, *dir), ("#123456".into(), 3.0, LineDir::Rising)),
        k => panic!("{k:?}"),
    }
    // Bad values change nothing.
    for bad in [
        json!({"kind": "line", "dir": "sideways"}),
        json!({"kind": "line", "dir": 3}),
        json!({"kind": "line", "width": "thick"}),
        json!({"kind": "circle", "fill": "red"}),
        json!({"kind": "ellipse", "width": true}),
    ] {
        assert!(s.run("layout.addElement", &bad).is_err(), "{bad}");
    }
    for bad in [
        json!({"element": line, "dir": "up"}),
        json!({"element": line, "dir": 1}),
        json!({"element": circle, "width": "thick"}),
        json!({"element": rect, "outline": "#XYZ"}),
    ] {
        assert!(s.run("layout.update", &bad).is_err(), "{bad}");
    }
    assert!(matches!(s.project.layouts[li].element(line).unwrap().kind, ElementKind::Line { dir: LineDir::Rising, .. }));
    // A null width or direction is left as it is.
    s.run("layout.update", &json!({"element": line, "width": null, "dir": null})).unwrap();
    assert!(matches!(s.project.layouts[li].element(line).unwrap().kind,
        ElementKind::Line { dir: LineDir::Rising, width, .. } if width == 3.0));
    // Any part of a box makes a rectangle a shape, not the page's neatline.
    let partial = add(&mut s, json!({"kind": "rectangle", "x": 30, "y": 40, "h": 10}));
    let e = s.project.layouts[li].element(partial).unwrap();
    assert_eq!((e.name.as_str(), e.x, e.y, e.h), ("Rectangle", 30.0, 40.0, 10.0));
    for p in [json!({"kind": "rectangle"}), json!({"kind": "rectangle", "x": null, "w": null})] {
        let neat = add(&mut s, p);
        assert_eq!(s.project.layouts[li].element(neat).unwrap().name, "Neatline");
    }
    // Saved and read back.
    let back: rustgis_map::layout::Layout = serde_json::from_value(serde_json::to_value(&s.project.layouts[li]).unwrap()).unwrap();
    assert_eq!(back, s.project.layouts[li]);
}

#[test]
fn layout_arrows_point_turn_and_keep_their_length() {
    use rustgis_map::layout::{ElementKind, LineHead, line_angle, line_length};
    let mut s = world();
    s.run("layout.new", &json!({"name": "Arrows", "template": "blank"})).unwrap();
    let li = s.active_layout.unwrap();
    // The angle, length, heads and name of a line element.
    let read = |s: &Session, id: u32| {
        let e = s.project.layouts[li].element(id).unwrap();
        match e.kind {
            ElementKind::Line { dir, head, .. } => {
                let r = (e.x, e.y, e.w, e.h);
                (line_angle(r, dir, head), line_length(r, dir), head, e.name.clone(), (e.x + e.w / 2.0, e.y + e.h / 2.0))
            }
            ref k => panic!("{k:?}"),
        }
    };
    let id = s.run("layout.addElement", &json!({"kind": "arrow", "angle": 45, "length": 50})).unwrap()["element"].as_u64().unwrap() as u32;
    let (a, len, head, name, centre) = read(&s, id);
    assert!((a - 45.0).abs() < 1e-9 && (len - 50.0).abs() < 1e-9, "{a}° {len} mm");
    assert_eq!((head, name.as_str()), (LineHead::End, "Arrow"));

    // Turned to point left: the head moves to the line's start; length and centre stay.
    s.run("layout.update", &json!({"element": id, "angle": 180})).unwrap();
    let (a, len, head, _, c) = read(&s, id);
    assert!((a.abs() - 180.0).abs() < 1e-9 && (len - 50.0).abs() < 1e-9, "{a}° {len} mm");
    assert_eq!(head, LineHead::Start);
    assert!((c.0 - centre.0).abs() < 1e-9 && (c.1 - centre.1).abs() < 1e-9);
    // Made longer, it keeps pointing the same way.
    s.run("layout.update", &json!({"element": id, "length": 80})).unwrap();
    let (a, len, ..) = read(&s, id);
    assert!((a.abs() - 180.0).abs() < 1e-9 && (len - 80.0).abs() < 1e-9);
    // Two heads, then none: a plain line.
    s.run("layout.update", &json!({"element": id, "head": "both"})).unwrap();
    assert_eq!(read(&s, id).2, LineHead::Both);

    // A plain line takes an angle too; other elements don't.
    let line = s.run("layout.addElement", &json!({"kind": "line", "angle": -30})).unwrap()["element"].as_u64().unwrap() as u32;
    assert_eq!(read(&s, line).3, "Line");
    let rect = s.run("layout.addElement", &json!({"kind": "rectangle", "w": 20, "h": 20})).unwrap()["element"].as_u64().unwrap() as u32;
    for bad in [
        json!({"element": rect, "angle": 10}),
        json!({"element": id, "head": "sideways"}),
        json!({"element": id, "angle": "steep"}),
        json!({"element": id, "length": 0}),
        json!({"element": id, "length": -5}),
    ] {
        assert!(s.run("layout.update", &bad).is_err(), "{bad}");
    }
    assert!(s.run("layout.addElement", &json!({"kind": "arrow", "head": "up"})).is_err());
    assert_eq!(read(&s, id).2, LineHead::Both, "bad values changed nothing");
    // Undo turns it back.
    s.run("edit.undo", &Value::Null).unwrap(); // the rectangle
    s.run("edit.undo", &Value::Null).unwrap(); // the line
    s.run("edit.undo", &Value::Null).unwrap(); // both heads
    s.run("edit.undo", &Value::Null).unwrap(); // the length
    let (a, len, ..) = read(&s, id);
    assert!((a.abs() - 180.0).abs() < 1e-9 && (len - 50.0).abs() < 1e-9);
}

#[test]
fn layout_arrows_keep_shallow_and_steep_angles_exactly() {
    use rustgis_map::layout::{ElementKind, line_angle, line_length};
    let mut s = world();
    s.run("layout.new", &json!({"name": "Arrows", "template": "blank"})).unwrap();
    let li = s.active_layout.unwrap();
    let read = |s: &Session, id: u32| {
        let e = s.project.layouts[li].element(id).unwrap();
        match e.kind {
            ElementKind::Line { dir, head, .. } => {
                let r = (e.x, e.y, e.w, e.h);
                (line_angle(r, dir, head), line_length(r, dir), (e.x, e.y))
            }
            ref k => panic!("{k:?}"),
        }
    };
    let add = |s: &mut Session, p: Value| s.run("layout.addElement", &p).unwrap()["element"].as_u64().unwrap() as u32;
    // A box under 5 mm one way is not stretched to the minimum a dragged element gets.
    for (angle, length) in [(3.0, 60.0), (85.0, 50.0), (-2.0, 40.0)] {
        let id = add(&mut s, json!({"kind": "arrow", "angle": angle, "length": length}));
        let (a, len, _) = read(&s, id);
        assert!((a - angle).abs() < 1e-9 && (len - length).abs() < 1e-9, "{angle}°: {a}° {len} mm");
    }
    // x and y move an angled line without bending it, when added or turned.
    let id = add(&mut s, json!({"kind": "arrow", "angle": 30, "length": 40, "x": 12, "y": 34}));
    assert_eq!(read(&s, id).2, (12.0, 34.0));
    s.run("layout.update", &json!({"element": id, "angle": 60, "x": 50})).unwrap();
    let (a, len, (x, _)) = read(&s, id);
    assert!((a - 60.0).abs() < 1e-9 && (len - 40.0).abs() < 1e-9 && x == 50.0, "{a}° {len} mm at {x}");
    // A length is more than 0 when added too, and a dir doesn't come with an angle or a length.
    assert!(s.run("layout.addElement", &json!({"kind": "arrow", "angle": 10, "length": 0})).is_err());
    assert!(s.run("layout.addElement", &json!({"kind": "line", "angle": 10, "dir": "falling"})).is_err());
    for bad in [json!({"element": id, "dir": "rising", "angle": 10}), json!({"element": id, "dir": "falling", "length": 30})] {
        assert!(s.run("layout.update", &bad).is_err(), "{bad}");
    }
}

#[test]
fn merged_undo_steps_undo_as_one() {
    let mut s = world();
    s.run("layout.new", &json!({"name": "Merge"})).unwrap();
    let li = s.active_layout.unwrap();
    let title = s.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
    let at = |s: &Session| s.project.layouts[li].element(title.id).map(|e| (e.x, e.y)).unwrap();
    let nudge = |s: &mut Session, key: &str, x: f64| {
        s.merge_undo(key);
        s.run("layout.update", &json!({"element": title.id, "x": x})).unwrap();
    };
    let depth = s.undo_depth();
    // A run under one key is one step.
    for x in [10.0, 11.0, 12.0] {
        nudge(&mut s, "a", x);
    }
    assert_eq!(s.undo_depth(), depth + 1);
    // A change in between starts a new step, even under the same key; so does another key.
    s.run("layout.update", &json!({"element": title.id, "y": 5.0})).unwrap();
    nudge(&mut s, "a", 13.0);
    nudge(&mut s, "b", 14.0);
    assert_eq!(s.undo_depth(), depth + 4);
    // Undoing a run goes back to before its first change.
    for _ in 0..4 {
        s.run("edit.undo", &Value::Null).unwrap();
    }
    assert_eq!(at(&s), (title.x, title.y));
    // After an undo the same key starts afresh, not merging into the step before.
    nudge(&mut s, "a", 20.0);
    nudge(&mut s, "a", 21.0);
    assert_eq!(s.undo_depth(), depth + 1);
    // A key asked for by a command that failed is gone after it: the next change is its own step.
    s.merge_undo("a");
    assert!(s.run("layout.update", &json!({"element": 999_999, "x": 1})).is_err());
    s.run("layout.update", &json!({"element": title.id, "x": 30.0})).unwrap();
    assert_eq!(s.undo_depth(), depth + 2);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(at(&s).0, 21.0);
    // So is one asked for by a command refused before it ran (unknown, blocked).
    nudge(&mut s, "c", 40.0);
    s.merge_undo("c");
    assert!(s.run("no.such.command", &Value::Null).is_err());
    let before = s.undo_depth();
    s.run("layout.update", &json!({"element": title.id, "x": 41.0})).unwrap();
    assert_eq!(s.undo_depth(), before + 1);
}

#[test]
fn layout_guides_are_set_kept_and_undone() {
    let mut s = world();
    s.run("layout.new", &json!({"name": "G"})).unwrap();
    let li = s.active_layout.unwrap();
    let r = s.run("layout.guides", &json!({"x": [20, 148.5], "y": [30]})).unwrap();
    assert_eq!(r, json!({"x": [20.0, 148.5], "y": [30.0]}));
    // A list given replaces that direction only.
    s.run("layout.guides", &json!({"y": [10, 40]})).unwrap();
    let g = s.project.layouts[li].guides.clone();
    assert_eq!((g.x, g.y), (vec![20.0, 148.5], vec![10.0, 40.0]));
    // Bad positions change nothing.
    for bad in [json!({"x": [1, "a"]}), json!({"x": [1e9]}), json!({"y": 5}), json!({"x": vec![1.0; 65]})] {
        assert!(s.run("layout.guides", &bad).is_err(), "{bad}");
    }
    assert_eq!(s.project.layouts[li].guides.y, vec![10.0, 40.0]);
    // Saved with the layout, and only when there are some.
    let saved = serde_json::to_value(&s.project.layouts[li]).unwrap();
    assert_eq!(saved["guides"]["x"], json!([20.0, 148.5]));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.layouts[li].guides.y, vec![30.0], "undo restores the guides before");
    s.run("layout.guides", &json!({"clear": true})).unwrap();
    assert!(s.project.layouts[li].guides.is_empty());
    assert!(serde_json::to_value(&s.project.layouts[li]).unwrap().get("guides").is_none());
}

#[test]
fn layout_names_keep_to_200_characters_with_their_number() {
    let mut s = world();
    let long = "é".repeat(300);
    s.run("layout.new", &json!({"name": long})).unwrap();
    let r = s.run("layout.new", &json!({"name": long})).unwrap();
    let name = r["name"].as_str().unwrap_or_default().to_string();
    assert_eq!((name.chars().count(), name.ends_with(" 2")), (200, true), "{name}");
    s.run("layout.new", &json!({"name": "Other"})).unwrap();
    let r = s.run("layout.rename", &json!({"name": "Other", "newName": long})).unwrap();
    let name = r["name"].as_str().unwrap_or_default();
    assert_eq!((name.chars().count(), name.ends_with(" 3")), (200, true), "{name}");
}

/// Layers › Copy and Paste (#491): whole layers and groups, into another map or the same one.
#[test]
fn layers_are_copied_and_pasted_between_maps() {
    let mut s = world();
    let first = s.map().name.clone();
    let names = |m: &rustgis_map::Map| m.layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    assert!(s.run("layer.paste", &Value::Null).is_err(), "nothing copied yet");
    // A group of two styled layers, one masked by the other, with features selected.
    s.run("group.add", &json!({"name": "Places", "layers": ["Cities", "Countries"]})).unwrap();
    s.run("layer.mask", &json!({"layer": "Cities", "mask": "Countries"})).unwrap();
    s.run("layer.definitionQuery", &json!({"layer": "Cities", "where": "pop_max > 1000000"})).unwrap();
    s.map_mut().layers.iter_mut().find(|l| l.name == "Cities").unwrap().transparency = 40.0;
    s.run("select.byAttributes", &json!({"layer": "Cities", "where": "name = 'Paris'"})).unwrap();
    let r = s.run("layer.copy", &json!({"group": "Places"})).unwrap();
    assert_eq!(r["layers"], json!(["Cities", "Countries"]), "in drawing order");
    // Pasted into an empty map: the same order, group, mask and style, no selection; one undo step.
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    let r = s.run("layer.paste", &Value::Null).unwrap();
    assert_eq!((names(s.map()), r["unmasked"].clone()), (vec!["Cities".to_string(), "Countries".into()], json!([])));
    assert_eq!(s.active_layer, s.map().layers.first().map(|l| l.id));
    let m = s.map();
    let cities = m.layer_by_name("Cities").unwrap();
    assert_eq!((cities.mask.as_deref(), cities.transparency, cities.selection.len()), (Some("Countries"), 40.0, 0));
    assert_eq!(cities.definition_query.as_deref(), Some("pop_max > 1000000"));
    assert_eq!((m.groups.len(), m.group_of(cities.id).map(|g| g.name.as_str())), (1, Some("Places")));
    assert!(cities.dataset.features.shares(&s.project.maps[0].layer_by_name("Cities").unwrap().dataset.features), "shared until edited");
    // Again into the same map: unique names, the mask follows its pasted layer, on top.
    s.run("layer.paste", &Value::Null).unwrap();
    assert_eq!(names(s.map()), ["Cities copy", "Countries copy", "Cities", "Countries"]);
    let m = s.map();
    let copy = m.layer_by_name("Cities copy").unwrap();
    assert_eq!((copy.mask.as_deref(), m.group_of(copy.id).map(|g| g.name.as_str())), (Some("Countries copy"), Some("Places copy")));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!((s.map().layers.len(), s.map().groups.len()), (2, 1));
    // One layer, into a map out of view (the view stays), then into a map without its mask.
    s.run("map.activate", &json!({"name": first})).unwrap();
    s.run("layer.copy", &json!({"layer": "Cities"})).unwrap();
    s.run("layer.paste", &json!({"map": "Second"})).unwrap();
    assert_eq!(s.map().name, first);
    let second = &s.project.maps[1];
    let copy = second.layer_by_name("Cities copy").unwrap();
    assert_eq!((copy.mask.as_deref(), second.group_of(copy.id).is_none()), (Some("Countries"), true));
    s.run("map.new", &json!({"name": "Third"})).unwrap();
    let r = s.run("layer.paste", &Value::Null).unwrap();
    assert_eq!((r["unmasked"].clone(), s.map().layers[0].mask.clone()), (json!(["Cities"]), None));
    // Only a polygon layer masks (as the renderer finds it): a point layer of that name doesn't.
    s.run("layer.rename", &json!({"layer": "Cities", "name": "Countries"})).unwrap();
    let r = s.run("layer.paste", &Value::Null).unwrap();
    assert_eq!((r["unmasked"].clone(), s.map().layers[0].mask.clone()), (json!(["Cities"]), None));
    s.run("edit.undo", &Value::Null).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    // Into a group, at its top.
    s.run("layer.paste", &json!({"map": "Second", "group": "Places"})).unwrap();
    let second = &s.project.maps[1];
    let group = second.groups.iter().find(|g| g.name == "Places").unwrap();
    let top = second.layers.iter().find(|l| group.children.contains(&l.id)).map(|l| l.name.as_str());
    assert_eq!((group.children.len(), top), (3, Some("Cities copy 2")));
    // A group and a position at once is refused as such (Second has the group).
    let both = s.run("layer.paste", &json!({"map": "Second", "group": "Places", "to": 0})).unwrap_err();
    assert!(both.contains("not both"), "{both}");
    for bad in [json!({"map": "Nowhere"}), json!({"group": "Nowhere"}), json!({"to": -1})] {
        assert!(s.run("layer.paste", &json!(bad)).is_err(), "{bad}");
    }
    for bad in [json!({"layer": "Nowhere"}), json!({"group": "Nowhere"}), json!({"layer": [1, "Nowhere"]}), json!({"layer": []})] {
        assert!(s.run("layer.copy", &bad).is_err(), "{bad}");
    }
    // A copy that fails, an empty one included, keeps the last copy.
    let kept: Vec<String> = s.layer_clipboard.as_ref().map(|c| c.layers.iter().map(|l| l.name.clone()).collect()).unwrap_or_default();
    assert_eq!(kept, ["Cities"]);
    // All or nothing: a failure after the layers went in (a later step of a task) takes them back.
    let (layers, groups) = (names(&s.project.maps[1]), s.project.maps[1].groups.len());
    let r = s.guarded("Task", true, |s| {
        s.run("layer.paste", &json!({"map": "Second"}))?;
        Err("a later step failed".into())
    });
    assert!(r.is_err());
    assert_eq!((names(&s.project.maps[1]), s.project.maps[1].groups.len()), (layers, groups));
    // An empty group is copied too, and its paste is said as a group (no layers came with it).
    s.run("group.add", &json!({"name": "Empty"})).unwrap();
    s.run("layer.copy", &json!({"group": "Empty"})).unwrap();
    let r = s.run("layer.paste", &Value::Null).unwrap();
    assert_eq!((r["layers"].clone(), r["groups"].as_array().map(Vec::len)), (json!([]), Some(1)));
    assert_eq!(s.notices.last().map(|n| n.text.as_str()), Some("Pasted 1 group into Third"));
}

#[test]
fn map_duplicate_copies_a_map_beside_it_and_leaves_layouts_alone() {
    use rustgis_map::layout::ElementKind;
    let mut s = world();
    let first = s.map().name.clone();
    let layers = s.map().layers.len();
    s.run("layout.new", &json!({"page": "a4"})).unwrap();
    s.run("select.byAttributes", &json!({"layer": "Cities", "where": "name = 'Paris'"})).unwrap();
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    // A map out of view, copied beside it and brought into view.
    let r = s.run("map.duplicate", &json!({"map": first})).unwrap();
    let copy = format!("{first} copy");
    assert_eq!(r["name"], copy.as_str());
    let names: Vec<&str> = s.project.maps.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, [first.as_str(), copy.as_str(), "Second"]);
    assert_eq!((s.project.active_map, s.map().layers.len()), (1, layers));
    assert_eq!(s.active_layer, s.map().layers.first().map(|l| l.id));
    // The copy's own: no selection, and changing it leaves the original alone.
    assert_eq!(s.map().selection_count(), 0);
    assert!(s.project.maps[0].selection_count() > 0);
    s.map_mut().layers[0].name = "Changed".into();
    s.map_mut().layers[0].dataset.features.clear();
    assert_ne!(s.project.maps[0].layers[0].name, "Changed");
    assert!(!s.project.maps[0].layers[0].dataset.features.is_empty());
    // Layouts keep showing the original.
    let frames: Vec<&str> = s.project.layouts[0]
        .elements
        .iter()
        .filter_map(|e| match &e.kind {
            ElementKind::MapFrame { map, .. } => Some(map.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(frames, [first.as_str()]);
    // Copying again numbers the name; a given name is used; one undo removes a copy.
    assert_eq!(s.run("map.duplicate", &json!({"map": first})).unwrap()["name"], format!("{copy} 2"));
    assert_eq!(s.run("map.duplicate", &json!({"name": " Atlas "})).unwrap()["name"], "Atlas");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.maps.len(), 4);
    for m in [json!(0), json!("Nowhere")] {
        assert!(s.run("map.duplicate", &json!({"map": m})).is_err(), "{m}");
    }
    // A long name keeps to 200 characters with " copy".
    s.run("map.rename", &json!({"name": "é".repeat(300)})).unwrap();
    let name = s.run("map.duplicate", &Value::Null).unwrap()["name"].as_str().unwrap_or_default().to_string();
    assert_eq!((name.chars().count(), name.ends_with(" copy")), (200, true), "{name}");
}

#[test]
fn layout_duplicate_copies_every_element_and_opens_the_copy() {
    let mut s = world();
    s.run("layout.new", &json!({"name": "Europe", "page": "a4"})).unwrap();
    s.run("layout.new", &json!({"name": "Other"})).unwrap();
    let r = s.run("layout.duplicate", &json!({"layout": "europe"})).unwrap();
    assert_eq!((r["name"].as_str(), r["layout"].as_u64()), (Some("Europe copy"), Some(1)));
    let names: Vec<&str> = s.project.layouts.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Europe", "Europe copy", "Other"]);
    assert_eq!(s.active_layout, Some(1));
    let (a, b) = (&s.project.layouts[0], &s.project.layouts[1]);
    assert_eq!((&a.elements, a.width, a.height), (&b.elements, b.width, b.height));
    s.project.layouts[1].elements.clear();
    assert!(!s.project.layouts[0].elements.is_empty());
    // The open layout by default, with a given name; one undo removes it.
    assert_eq!(s.run("layout.duplicate", &json!({"newName": "Print"})).unwrap()["name"], "Print");
    assert_eq!(s.active_layout, Some(2));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.layouts.len(), 3);
    // Undo takes the copy away, so the layout open before it opens again, not whatever sits at
    // the copy's old position; an open layout the undo keeps stays open (redo).
    let open = |s: &Session| s.project.layouts.get(s.active_layout.unwrap_or(usize::MAX)).map(|l| l.name.clone());
    assert_eq!(open(&s).as_deref(), Some("Europe copy"));
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!((open(&s).as_deref(), s.project.layouts[2].name.as_str()), (Some("Europe copy"), "Print"));
    s.run("edit.undo", &Value::Null).unwrap();
    s.run("layout.activate", &json!({"name": "Other"})).unwrap();
    s.run("layout.duplicate", &json!({"layout": "Europe"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(open(&s).as_deref(), Some("Other"));
    // Undoing a change made on the map leaves the layout opened since then open, and the map
    // view stays the map view.
    s.run("layout.close", &Value::Null).unwrap();
    s.run("map.rename", &json!({"name": "Atlas"})).unwrap();
    s.run("layout.activate", &json!({"name": "Europe"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(open(&s).as_deref(), Some("Europe"));
    s.run("layout.close", &Value::Null).unwrap();
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(s.active_layout, None);
    // A selector that is neither a name nor an index never falls back to the open layout.
    for l in [json!("Nowhere"), json!(false), json!(-1), json!(9), json!(["Europe"])] {
        assert!(s.run("layout.duplicate", &json!({"layout": l})).is_err(), "{l}");
    }
    assert_eq!(s.project.layouts.len(), 3);
    assert!(s.run("layout.duplicate", &json!({"layout": 0.0})).is_ok(), "an index sent as a float (forms)");
}

/// A command that fails part way (a task's later step) leaves open the layout that was open
/// before it, not one an earlier step opened.
#[test]
fn a_failed_command_reopens_the_layout_open_before_it() {
    let mut s = world();
    s.run("layout.new", &json!({"name": "A"})).unwrap();
    s.run("layout.new", &json!({"name": "B"})).unwrap();
    s.run("layout.activate", &json!({"name": "A"})).unwrap();
    let r = s.guarded("Task", true, |s| {
        s.run("layout.activate", &json!({"name": "B"}))?;
        Err("a later step failed".into())
    });
    assert!(r.is_err());
    assert_eq!(s.active_layout.and_then(|i| s.project.layouts.get(i)).map(|l| l.name.as_str()), Some("A"));
}

/// Text graphics can be bold (#516): added, styled or edited so; a graphics layer made before the
/// field reads as not bold and gains the field when one is made bold.
#[test]
fn text_graphics_can_be_bold() {
    use rustgis_map::graphics;
    let mut s = world();
    let a = s.run("graphics.add", &json!({"shape": "text", "points": [[10, 50]], "text": "A", "bold": true})).unwrap();
    let id = a["layer"].as_u64().unwrap() as u32;
    let b = s.run("graphics.add", &json!({"shape": "text", "points": [[20, 40]], "text": "B", "layer": id})).unwrap();
    let (ra, rb) = (a["row"].as_u64().unwrap() as usize, b["row"].as_u64().unwrap() as usize);
    let ds = |s: &Session| s.map().layer(id).unwrap().dataset.clone();
    assert!(graphics::bold(&ds(&s), ra) && !graphics::bold(&ds(&s), rb));
    s.run("graphics.edit", &json!({"layer": id, "row": rb, "bold": true})).unwrap();
    assert!(graphics::bold(&ds(&s), rb));
    s.run("select.rows", &json!({"layer": id, "rows": [ra]})).unwrap();
    s.run("graphics.style", &json!({"bold": false})).unwrap();
    assert!(!graphics::bold(&ds(&s), ra));
    // A layer from before the field: not bold, until made so.
    let l = s.map_mut().layer_mut(id).unwrap();
    let fi = l.dataset.field_index("bold").unwrap();
    l.dataset.fields.remove(fi);
    l.dataset.features.iter_mut().for_each(|f| {
        f.attrs.remove(fi);
    });
    assert!(!graphics::bold(&ds(&s), rb));
    s.run("graphics.edit", &json!({"layer": id, "row": rb, "bold": true})).unwrap();
    assert!(graphics::bold(&ds(&s), rb) && !graphics::bold(&ds(&s), ra));
}

/// Text graphics take italic, alignment and letter spacing (#535) as a layout's text does: added,
/// styled or edited so, checked and limited the same; a layer made before the fields reads as
/// plain text and gains each field when it is first set; one typed wrong in the attribute table
/// reads as plain too.
#[test]
fn text_graphics_take_italic_alignment_and_letter_spacing() {
    use rustgis_map::graphics;
    use rustgis_map::layout::TextAlign;
    let mut s = world();
    let a = s
        .run(
            "graphics.add",
            &json!({"shape": "text", "points": [[10, 50]], "text": "A\nB", "italic": true, "align": "center", "letterSpacing": 3}),
        )
        .unwrap();
    let id = a["layer"].as_u64().unwrap() as u32;
    let b = s.run("graphics.add", &json!({"shape": "text", "points": [[20, 40]], "text": "B", "layer": id})).unwrap();
    let (ra, rb) = (a["row"].as_u64().unwrap() as usize, b["row"].as_u64().unwrap() as usize);
    let ds = |s: &Session| s.map().layer(id).unwrap().dataset.clone();
    let style = |s: &Session, r| (graphics::italic(&ds(s), r), graphics::align(&ds(s), r), graphics::letter_spacing(&ds(s), r));
    assert_eq!(style(&s, ra), (true, TextAlign::Center, 3.0));
    assert_eq!(style(&s, rb), (false, TextAlign::Left, 0.0), "not given: plain");
    // Edited, styled and limited as a layout's text is.
    s.run("graphics.edit", &json!({"layer": id, "row": rb, "align": "Right", "letterSpacing": 900})).unwrap();
    assert_eq!(style(&s, rb), (false, TextAlign::Right, 50.0));
    s.run("graphics.edit", &json!({"layer": id, "row": rb, "letterSpacing": -40})).unwrap();
    assert_eq!(style(&s, rb).2, -5.0);
    s.run("select.rows", &json!({"layer": id, "rows": [ra]})).unwrap();
    s.run("graphics.style", &json!({"italic": false})).unwrap();
    assert_eq!(style(&s, ra), (false, TextAlign::Center, 3.0), "only what is given changes");
    // Wrong values are refused, and change nothing; the message names the parameter.
    for (bad, name) in
        [(json!({"align": "up"}), "alignment"), (json!({"italic": "maybe"}), "italic"), (json!({"letterSpacing": "wide"}), "letterSpacing")]
    {
        let mut p = bad.clone();
        p["layer"] = json!(id);
        p["row"] = json!(ra);
        let err = s.run("graphics.edit", &p).unwrap_err();
        assert!(err.contains(name), "{bad}: {err}");
        let mut p = bad;
        p["shape"] = json!("text");
        p["points"] = json!([[0, 0]]);
        p["text"] = json!("C");
        assert!(s.run("graphics.add", &p).is_err());
    }
    assert_eq!(style(&s, ra), (false, TextAlign::Center, 3.0));
    assert!(s.run("graphics.style", &json!({})).unwrap_err().contains("letterSpacing"), "the style's own words list them");
    // Placed again with a style: both happen; with a bad style, neither (the session puts back a
    // command that fails); with only nulls for a style, it is just placed.
    let at = |s: &Session| ds(s).features[ra].geometry.clone();
    let before = at(&s);
    for bad in [json!({"align": "up"}), json!({"fill": "red"})] {
        let mut p = bad;
        p["layer"] = json!(id);
        p["row"] = json!(ra);
        p["points"] = json!([[11, 50]]);
        assert!(s.run("graphics.edit", &p).is_err());
        assert_eq!(at(&s), before, "placed again though its style was refused");
    }
    s.run("graphics.edit", &json!({"layer": id, "row": ra, "points": [[12, 50]], "italic": null, "fill": null})).unwrap();
    let nulls = at(&s);
    assert!(nulls != before && !graphics::italic(&ds(&s), ra), "nulls are no style: placed, style kept");
    s.run("graphics.edit", &json!({"layer": id, "row": ra, "points": [[11, 50]], "italic": true})).unwrap();
    assert!(graphics::italic(&ds(&s), ra) && at(&s) != nulls);
    // A layer from before the fields: plain, until each is set.
    let l = s.map_mut().layer_mut(id).unwrap();
    for name in ["italic", "align", "letter_spacing"] {
        let fi = l.dataset.field_index(name).unwrap();
        l.dataset.fields.remove(fi);
        l.dataset.features.iter_mut().for_each(|f| {
            f.attrs.remove(fi);
        });
    }
    assert_eq!(style(&s, ra), (false, TextAlign::Left, 0.0));
    s.run("graphics.edit", &json!({"layer": id, "row": ra, "align": "center"})).unwrap();
    assert_eq!(style(&s, ra), (false, TextAlign::Center, 0.0));
    assert!(ds(&s).field_index("align").is_some() && ds(&s).field_index("italic").is_none(), "only the field set is gained");
    // Typed wrong in the attribute table: read as plain, within range.
    let l = s.map_mut().layer_mut(id).unwrap();
    let (fa, fs) = (
        l.dataset.ensure_field("align", rustgis_data::FieldType::Text),
        l.dataset.ensure_field("letter_spacing", rustgis_data::FieldType::Double),
    );
    l.dataset.features[ra].attrs[fa] = rustgis_data::Value::Text("sideways".into());
    l.dataset.features[ra].attrs[fs] = rustgis_data::Value::Float(f64::NAN);
    assert_eq!(style(&s, ra), (false, TextAlign::Left, 0.0));
}

/// Text graphics pasted into a graphics layer made before their style fields keep their style
/// (#583): the layer gains the fields they carry, from the session's clipboard and from GeoJSON
/// text alike; features that aren't graphics add none, even with a field of the same name.
#[test]
fn pasted_graphics_keep_their_style_in_an_older_graphics_layer() {
    use rustgis_map::graphics;
    use rustgis_map::layout::TextAlign;
    let mut s = world();
    let a = s
        .run(
            "graphics.add",
            &json!({"shape": "text", "points": [[10, 50]], "text": "Styled", "bold": true, "italic": true, "align": "right", "letterSpacing": 4}),
        )
        .unwrap();
    let (from, row) = (a["layer"].as_u64().unwrap() as u32, a["row"].as_u64().unwrap() as usize);
    // A graphics layer from before the style fields.
    let old = |s: &mut Session, name: &str| {
        let id = s.run("graphics.newLayer", &json!({"name": name})).unwrap()["layer"].as_u64().unwrap() as u32;
        let l = s.map_mut().layer_mut(id).unwrap();
        for f in ["bold", "italic", "align", "letter_spacing"] {
            let fi = l.dataset.field_index(f).unwrap();
            l.dataset.fields.remove(fi);
            l.dataset.features.iter_mut().for_each(|ft| {
                ft.attrs.remove(fi);
            });
        }
        id
    };
    let style = |s: &Session, id: u32| {
        let ds = &s.map().layer(id).unwrap().dataset;
        let r = ds.features.len() - 1;
        (graphics::bold(ds, r), graphics::italic(ds, r), graphics::align(ds, r), graphics::letter_spacing(ds, r))
    };
    s.run("select.rows", &json!({"layer": from, "rows": [row]})).unwrap();
    let text = s.run("edit.copy", &Value::Null).unwrap()["text"].as_str().unwrap().to_string();
    // From the session's clipboard (the Paste command) and from its GeoJSON (Ctrl+V in the app).
    for via_text in [false, true] {
        let to = old(&mut s, "Older");
        let mut p = json!({"layer": to});
        if via_text {
            p["text"] = json!(text);
        }
        s.run("edit.paste", &p).unwrap();
        assert_eq!(style(&s, to), (true, true, TextAlign::Right, 4.0), "via text: {via_text}");
    }
    // Features that aren't graphics add no field, though one is named as a style: with no `kind`,
    // or with a `kind` of their own that isn't a graphic's.
    for properties in [r#"{"align":"N"}"#, r#"{"kind":"inspection","align":"center"}"#] {
        let to = old(&mut s, "Notes");
        let plain = format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","geometry":{{"type":"Point","coordinates":[1,2]}},"properties":{properties}}}]}}"#
        );
        s.run("edit.paste", &json!({"layer": to, "text": plain})).unwrap();
        assert!(s.map().layer(to).unwrap().dataset.field_index("align").is_none(), "{properties}");
    }
    // Pasted together, a graphic with no alignment and another feature with one: the feature's
    // value doesn't make the layer gain the field.
    let to = old(&mut s, "Mixed");
    let mixed = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"kind":"text","text":"A"}},
        {"type":"Feature","geometry":{"type":"Point","coordinates":[3,4]},"properties":{"kind":"inspection","align":"center"}}]}"#;
    s.run("edit.paste", &json!({"layer": to, "text": mixed})).unwrap();
    assert!(s.map().layer(to).unwrap().dataset.field_index("align").is_none());
}

/// A text graphic is placed again by its points (dragged on the map, #516), as one undo step;
/// other shapes move with `edit.move`.
#[test]
fn a_text_graphic_is_placed_again_by_its_points() {
    let mut s = world();
    let t = s.run("graphics.add", &json!({"shape": "text", "points": [[10, 50]], "text": "A"})).unwrap();
    let id = t["layer"].as_u64().unwrap() as u32;
    let row = t["row"].as_u64().unwrap() as usize;
    let at = |s: &Session| s.map().layer(id).unwrap().dataset.features[row].geometry.as_ref().unwrap().label_point().unwrap();
    s.run("graphics.edit", &json!({"layer": id, "row": row, "points": [[12, 48]]})).unwrap();
    assert_eq!((at(&s).x, at(&s).y), (12.0, 48.0));
    assert_eq!(s.undo_label(), Some("Edit Graphic"));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!((at(&s).x, at(&s).y), (10.0, 50.0));
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[0, 0], [5, 5]], "layer": id})).unwrap();
    assert!(s.run("graphics.edit", &json!({"layer": id, "row": r["row"], "points": [[1, 1]]})).is_err());
}

/// A point, a text graphic's too, turns and scales on itself: Rotate and Scale leave it out, and
/// with nothing else selected they change nothing and leave no undo step (#516).
#[test]
fn rotating_or_scaling_a_text_changes_nothing() {
    let mut s = world();
    let t = s.run("graphics.add", &json!({"shape": "text", "points": [[10, 50]], "text": "A"})).unwrap();
    let layer = t["layer"].as_u64().unwrap();
    s.run("select.rows", &json!({"layer": layer, "rows": [t["row"]]})).unwrap();
    let label = s.undo_label();
    assert!(s.run("edit.rotate", &json!({"angle": 34})).is_err());
    assert!(s.run("edit.scale", &json!({"factor": 2})).is_err());
    assert_eq!(s.undo_label(), label, "no undo step");
    // With a shape beside it, the shape turns.
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[0, 0], [5, 5]], "layer": layer})).unwrap();
    s.run("select.rows", &json!({"layer": layer, "rows": [t["row"], r["row"]]})).unwrap();
    assert_eq!(s.run("edit.rotate", &json!({"angle": 34})).unwrap()["modified"], 1);
}

/// A command that opens another project part way and then fails (a plugin's command through its
/// bridge) leaves the project before it open, counted as it was: the interface keeps what it
/// holds for it.
#[test]
fn a_failed_command_keeps_the_project_open_before_it() {
    let mut s = world();
    // Opened from a file, edited, unsaved, with a task made here.
    s.path = Some("atlas.rgis".into());
    s.run("map.rename", &json!({"name": "Edited"})).unwrap();
    s.trusted_tasks.insert("mine".into());
    let (name, opened, undo) = (s.project.name.clone(), s.opened, s.undo_label());
    assert!(undo.is_some() && s.dirty);
    let r = s.guarded("Task", true, |s| {
        s.run("project.new", &json!({"name": "Other"}))?;
        Err("a later step failed".into())
    });
    assert!(r.is_err());
    assert_eq!((s.project.name.as_str(), s.opened), (name.as_str(), opened));
    // It comes back with what it held: its undo history, its unsaved state, the trust it had, and
    // its file, so Save writes there.
    assert_eq!(s.undo_label(), undo);
    assert!(s.dirty && s.trusted_tasks.contains("mine"));
    assert_eq!(s.path.as_deref(), Some(std::path::Path::new("atlas.rgis")));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_ne!(s.map().name, "Edited", "the edit before still undoes");
    // A step failing after a replacement in its own command leaves the new project as it is; the
    // task failing gives the first one back.
    s.run("map.rename", &json!({"name": "Edited"})).unwrap();
    let undo = s.undo_label();
    let r = s.guarded("Task", true, |s| {
        s.run("project.new", &json!({"name": "Other"}))?;
        // A step that runs (no condition stops it first) and fails inside, rolled back on its own.
        let rev = s.revision;
        assert!(s.run("map.rename", &json!({"name": ""})).is_err());
        assert!(s.revision > rev, "it ran as a command");
        assert_eq!(s.project.name, "Other");
        Err("a later step failed".into())
    });
    assert!(r.is_err());
    assert_eq!((s.project.name.as_str(), s.undo_label()), (name.as_str(), undo));
    // Its extent history comes back usable, though the project the task opened had another
    // projection, and moving its view took the history for that projection.
    s.record_extent();
    s.map_mut().camera.center.x += 1e6;
    s.record_extent();
    assert!(s.can_extent_back());
    let r = s.guarded("Task", true, |s| {
        s.run("project.new", &json!({"name": "Other"}))?;
        s.run("map.projection", &json!({"type": "mollweide"}))?;
        s.record_extent();
        Err("a later step failed".into())
    });
    assert!(r.is_err());
    assert!(s.can_extent_back(), "the extent history is the restored map's");
    // Opened for good, it counts, and the old project's history and file go with it.
    s.run("project.new", &json!({"name": "Other"})).unwrap();
    assert_eq!((s.opened, s.undo_label(), s.path.as_deref()), (opened + 1, None, None));
    assert!(!s.trusted_tasks.contains("mine"));
}

/// A run from the history as code (#539): a Python call, a Script pane line, a CLI command. A
/// point buffered where the map was clicked goes by the command that ran it, with the point; keys
/// are left out; a run from before the tool's id was kept is found by its name.
#[test]
fn a_run_is_copied_from_the_history_as_code() {
    let mut s = world();
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100}})).unwrap();
    let r = &s.project.gp_history[0];
    assert!((r.id.as_str(), r.at > 0) == ("buffer", true), "the id and when it ran are kept");
    let code = |s: &mut Session, run: usize, kind: &str| -> Result<String, String> {
        s.run("gp.historyCode", &json!({"run": run, "as": kind})).map(|v| v["code"].as_str().unwrap_or("").to_string())
    };
    let py = code(&mut s, 0, "python").unwrap();
    assert!(py.starts_with("s.run(\n    \"gp.run\",\n    tool=\"buffer\",\n    params={\n"), "{py}");
    assert!(py.contains("\"distance\": 100,") && py.contains("\"input\": \"Cities\",") && py.ends_with(")"), "{py}");
    assert_eq!(code(&mut s, 0, "command").unwrap(), r#"gp.run {"params":{"distance":100,"input":"Cities"},"tool":"buffer"}"#);
    let cli = code(&mut s, 0, "cli").unwrap();
    assert!(
        cli.starts_with("rustgis-cli run --project '")
            && cli.ends_with(r#"--cmd 'gp.run={"params":{"distance":100,"input":"Cities"},"tool":"buffer"}'"#),
        "{cli}"
    );
    // A point buffer: by its command, with the point.
    s.run("gp.bufferPoint", &json!({"lon": 10, "lat": 50, "distance": 5})).unwrap();
    let py = code(&mut s, 1, "python").unwrap();
    assert!(py.contains("\"gp.bufferPoint\"") && py.contains("lon=10") && py.contains("lat=50") && py.contains("distance=5"), "{py}");
    // From an older project: no id, found by its name; its address without the key.
    let old = crate::GpRecord {
        tool: "Buffer".into(),
        id: String::new(),
        params: json!({"input": "https://h.example/a?token=s3cr3t", "distance": 1, "connection": "PG:host=db user=me password=hunter2 dbname=x", "https://k.example/?token=k3y": 1, "odbc": "DRIVER=x;UID=me;PWD=odbcpw", "libpq": "host=db user=me password=libpqpw dbname=x", "gdal": "MYSQL:db,user=me,password=gdalpw", "password": "pa55word", "api_token": "abc123", "lookup": {"key": "NAME"}, "where": "key=5 AND sig=2", "spaced": "host=db password = sp1 user=me password= sp2 port=1 password =sp3", "quoted": "user=me password='top secret' dbname=x", "braced": "UID=me;PWD={a;b c};X=1", "pem": "-----BEGIN RSA PRIVATE KEY-----\nMIIEpemsecret\n-----END RSA PRIVATE KEY-----\nafter", "service": {"url": "https://api.example.com", "key": "shortk"}, "escaped": "password='esc1\\' esc2' dbname=y"}),
        command: None,
        at: 0,
        ok: false,
        messages: vec!["no layer https://h.example/a?token=s3cr3t".into()],
        millis: 0,
        outputs: Vec::new(),
    };
    s.project.gp_history.push(old.clone());
    // Open in Tool finds the tool of a `gp.run` run, an older one's by its name; not a point
    // buffer's, which another command made.
    use crate::cmd::history::tool_of;
    let runs = &s.project.gp_history;
    assert_eq!((tool_of(&runs[0]).as_deref(), tool_of(&runs[1]), tool_of(&runs[2]).as_deref()), (Some("buffer"), None, Some("buffer")));
    let py = code(&mut s, 2, "python").unwrap();
    assert!(py.contains("tool=\"buffer\"") && py.contains("https://h.example/a") && !py.contains("s3cr3t"), "{py}");
    // A libpq connection keeps its words but its password, though spaces part them.
    assert!(py.contains("host=db") && !py.contains("hunter2"), "{py}");
    // A connection with no prefix `redact` knows keeps its words but its secret-named values.
    for pw in ["odbcpw", "libpqpw", "gdalpw"] {
        assert!(!py.contains(pw), "{pw} in {py}");
    }
    assert!(
        py.contains("UID=me;PWD=\"") && py.contains("host=db user=me password= dbname=x") && py.contains("user=me,password=\""),
        "{py}"
    );
    // A quoted or braced value goes whole; a private key pasted in goes; a plain `key` beside
    // an address is that service's key.
    // An escaped quote (libpq's `\'`) doesn't close a quoted value.
    for secret in ["top", "secret'", "b c", "pemsecret", "shortk", "esc1", "esc2"] {
        assert!(!py.contains(secret), "{secret} in {py}");
    }
    assert!(
        py.contains("password= dbname=x") && py.contains("PWD=;X=1") && py.contains("after") && py.contains("password= dbname=y"),
        "{py}"
    );
    // A bare `key` in an expression is a field, kept.
    assert!(py.contains("key=5 AND sig=2"), "{py}");
    // libpq's spaces around the `=` don't let the value through.
    assert!(!py.contains("sp1") && !py.contains("sp2") && !py.contains("sp3") && py.contains("user=me password= port=1"), "{py}");
    // A key is redacted as a value is.
    assert!(py.contains("https://k.example/") && !py.contains("k3y"), "{py}");
    // A value under a secret's name is left empty; a plain `key` that reads as a name is kept.
    assert!(!py.contains("pa55word") && !py.contains("abc123") && py.contains("\"password\": \"\"") && py.contains("\"NAME\""), "{py}");
    // Nothing tells which tool an unknown name was.
    s.project.gp_history.push(crate::GpRecord { tool: "Mystery".into(), ..old.clone() });
    assert!(code(&mut s, 3, "python").is_err() && code(&mut s, 9, "python").is_err());
    // A project file passing another command off as a run: not written into code.
    let forged = crate::GpRecord { command: Some(("file.delete\nproject.new".into(), json!({}))), ..old };
    s.project.gp_history.push(forged.clone());
    assert!(code(&mut s, 4, "command").is_err());
    assert_eq!(crate::cmd::history::tool_of(&forged), None);
    // The whole history: the project opened, each run in order; a failed run commented out, as
    // is one that can't be run again.
    let out = s.run("gp.historyScript", &Value::Null).unwrap();
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("import rustgis\n\ns = rustgis.Session(") && text.contains("# 1. Buffer ("), "{text}");
    assert!(text.contains("UTC)\ns.run(\n    \"gp.run\""), "{text}");
    assert!(text.contains("# 3. Buffer: failed when it ran (no layer https://h.example/a)\n# s.run("), "{text}");
    assert!(text.contains("# 4. Mystery: Mystery can't be run again"), "{text}");
    assert!(text.contains("# 5. Buffer: Buffer can't be run again") && !text.contains("file.delete"), "{text}");
    // Text from the file stays in its comment, whatever line break it holds (a lone `\r` ends a
    // line for Python).
    let sneaky = crate::GpRecord {
        tool: "x\rimport os".into(),
        id: "buffer".into(),
        ok: false,
        messages: vec!["boom\u{2028}import sys\nimport os".into()],
        ..forged
    };
    s.project.gp_history.push(crate::GpRecord { command: None, ..sneaky });
    let text = s.run("gp.historyScript", &Value::Null).unwrap()["text"].as_str().unwrap().to_string();
    assert!(!text.contains(['\r', '\u{2028}']), "{text:?}");
    assert!(text.lines().filter(|l| l.contains("import os") || l.contains("import sys")).all(|l| l.starts_with('#')), "{text}");
    assert!(!text.contains("s3cr3t") && !text.contains("hunter2"));
    assert!(text.contains("# The project hasn't been saved yet: save it as this file first.\nimport rustgis"), "{text}");
    // Named as Save names it, spaces and all.
    assert!(text.contains("\"World Atlas.rgis\""), "{text}");
    // Saved, then changed: the script says to save it again first.
    s.path = Some("atlas.rgis".into());
    s.dirty = true;
    let text = s.run("gp.historyScript", &Value::Null).unwrap()["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("# The project has changes that aren't saved: save it first") && !text.contains("hasn't been saved yet"),
        "{text}"
    );
    s.dirty = false;
    let text = s.run("gp.historyScript", &Value::Null).unwrap()["text"].as_str().unwrap().to_string();
    assert!(!text.contains("aren't saved") && text.contains("rustgis.Session(\"atlas.rgis\")"), "{text}");
    s.path = None;
    // A file name every system takes, whatever the project is called.
    s.project.name = "A*B?\"C<D>|E".into();
    let out = s.run("gp.historyScript", &Value::Null).unwrap();
    assert_eq!(out["fileName"], "A_B__C_D__E_history.py");
    s.project.gp_history.clear();
    assert!(s.run("gp.historyScript", &Value::Null).is_err(), "nothing to save");
}

/// A secret in a query string joined by `&`, or in a failed run's error quoting a connection,
/// is left out of the copied code and the history script; so is one with an `&` in it, or an
/// ODBC braced one with an escaped `}}`.
#[test]
fn secrets_in_a_query_or_a_run_s_error_stay_out_of_copied_code() {
    let mut s = world();
    s.project.gp_history.push(crate::GpRecord {
        tool: "Buffer".into(),
        id: "buffer".into(),
        params: json!({"input": "Cities", "query": "a=1&password=qp&b=2", "amp": "password=pw1&zz9", "quoted": "password='q1&b=c2'", "odbc": "PWD={abc}}def;tail};UID=me", "apostrophe": "a=O'Reilly&password=ap1&b=2", "stray": "a='x&password=ap2"}),
        command: None,
        at: 0,
        ok: false,
        messages: vec!["can't connect: host=db password=mp a=1&token=mp3".into()],
        millis: 0,
        outputs: Vec::new(),
    });
    let py = s.run("gp.historyCode", &json!({"run": 0})).unwrap()["code"].as_str().unwrap().to_string();
    assert!(py.contains("a=1&password=&b=2") && !py.contains("qp"), "{py}");
    // An `&` in a value, or in quotes, isn't another pair: the whole value goes. So does an ODBC
    // braced value with an escaped `}}` in it.
    for secret in ["pw1", "zz9", "q1", "c2", "abc", "def", "tail"] {
        assert!(!py.contains(secret), "{secret} in {py}");
    }
    assert!(py.contains("\"PWD=;UID=me\""), "{py}");
    // A quote inside a word opens nothing; one that opens a value and never closes doesn't hide
    // the pairs after it.
    assert!(!py.contains("ap1") && !py.contains("ap2"), "{py}");
    assert!(py.contains("a=O'Reilly&password=&b=2") && py.contains("a='x&password="), "{py}");
    let text = s.run("gp.historyScript", &Value::Null).unwrap()["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("failed when it ran (can't connect: host=db password= a=1&token=)")
            && !text.contains("password=mp")
            && !text.contains("mp3"),
        "{text}"
    );
}

#[path = "../../../tests/support/ogc_server.rs"]
mod ogc_server;

#[test]
fn ogc_landing_fixture_lists_and_loads_features() {
    use ogc_server::{CATALOG, ITEMS, ROOT, Server};
    let server = Server::fixture();
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &json!({"url": server.root()})).unwrap();
    assert_eq!(listing["count"], 1);
    assert_eq!(listing["layers"][0]["name"], "places");
    assert_eq!(listing["layers"][0]["source_url"], format!("{}{CATALOG}?f=json&key=fixture-key", server.base));
    assert_eq!(listing["layers"][0]["offer"]["url"], format!("{}{ITEMS}?key=fixture-key&limit=10000", server.base));
    assert!(listing.get("next").is_none());
    let result = session.run("service.add", &json!({"url": server.root(), "layer": "places"})).unwrap();
    assert_eq!(result["features"], 1);
    let dataset = &session.map().layer_by_name("Places").unwrap().dataset;
    assert_eq!(dataset.features.len(), 1);
    assert_eq!(dataset.features[0].geometry, Some(rustgis_data::Geometry::Point(rustgis_data::Coord::new(10., 50.))));
    let name = dataset.fields.iter().position(|f| f.name == "name").unwrap();
    assert_eq!(dataset.features[0].attrs[name], rustgis_data::Value::Text("Test Place".into()));
    server.assert_fixture_requests(&[ROOT, CATALOG, ROOT, CATALOG, ITEMS]);
}

#[test]
fn ogc_landing_supplied_documents_never_fetch_capabilities() {
    use ogc_server::{CATALOG, Server, fixture_collections, fixture_landing};
    let server = Server::fixture();
    let mut session = Session::new(Project::default());
    let mut params = json!({"url":server.root(), "text":fixture_landing()});
    let initial = session.run("service.layers", &params).unwrap();
    assert_eq!(initial["count"], 0);
    let next = initial["next"].as_str().unwrap();
    assert_eq!(next, format!("{}{CATALOG}?f=json&key=fixture-key", server.base));
    params["documents"] = json!([{"url":next,"text":fixture_collections()}]);
    let complete = session.run("service.layers", &params).unwrap();
    assert_eq!(complete["count"], 1);
    assert!(complete.get("next").is_none());
    assert!(server.requests().is_empty());
    params["documents"][0]["text"] = json!("{\"collections\":[]}");
    let empty = session.run("service.layers", &params).unwrap();
    assert_eq!(empty["count"], 0);
    assert!(empty.get("next").is_none());
}

#[test]
fn ogc_landing_keeps_map_tiles_and_collection_provenance() {
    use ogc_server::{CATALOG, ITEMS, Route, Server, fixture_collections, fixture_features, fixture_landing};
    let mut landing: Value = serde_json::from_str(fixture_landing()).unwrap();
    landing["links"].as_array_mut().unwrap().push(json!({
        "rel":"tilesets-map", "type":"application/json", "href":"/map/tiles"
    }));
    let mut collections: Value = serde_json::from_str(fixture_collections()).unwrap();
    // The dataset map and a feature collection can share the id "map".
    collections["collections"][0]["id"] = json!("map");
    let server = Server::new(vec![Route::new(ITEMS, fixture_features())]);
    let root = format!("{}?f=json", server.root());
    let catalog = format!("{}{CATALOG}?f=json&key=fixture-key", server.base);
    let mut params = json!({"url":server.root(),"text":landing.to_string(),"documents":[{"url":catalog,"text":collections.to_string()}]});
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &params).unwrap();
    assert_eq!(listing["count"], 2);
    assert_eq!(listing["kind"], "OGC API Features and Tiles");
    assert_eq!(listing["layers"][0]["source_url"], root);
    assert_eq!(listing["layers"][1]["source_url"], catalog);
    params["layer"] = json!("map");
    params["source_url"] = json!(root);
    params["tiles"] = json!(format!("{}/map/tiles/{{z}}/{{x}}/{{y}}.png", server.base));
    session.run("service.add", &params).unwrap();
    assert_eq!(session.map().tiles.len(), 1);
    assert!(session.map().tiles[0].url.contains("/map/tiles/"));
    params["source_url"] = json!(catalog);
    session.run("service.add", &params).unwrap();
    assert_eq!(session.map().layer_by_name("Places").unwrap().dataset.features.len(), 1);
    server.assert_fixture_requests(&[ITEMS]);

    // A bad collections answer must not discard the landing page's usable map.
    params["documents"][0]["text"] = json!("not JSON");
    let partial = session.run("service.layers", &params).unwrap();
    assert_eq!(partial["count"], 1);
    assert!(partial.get("next").is_none());
    assert!(!partial["notes"].as_array().unwrap().is_empty());
}

#[test]
fn ogc_landing_discovery_is_bounded_and_checks_addresses() {
    let mut session = Session::new(Project::default());
    let link = |url: &str| json!({"links":[{"rel":"data","type":"application/json","href":url}]}).to_string();
    let root = "https://example.org/ogc/root";
    for target in ["http://127.0.0.1/catalog", "http://169.254.169.254/catalog", "file:///etc/passwd"] {
        let result = session.run("service.layers", &json!({"url":root,"text":link(target)}));
        assert!(result.is_err(), "{target}");
    }
    let error = session.run("service.layers", &json!({"url":root,"text":link(&format!("{root}?f=json"))})).unwrap_err();
    assert!(error.contains("loop"), "{error}");
    let docs: Vec<Value> = (1..=3)
        .map(|n| {
            json!({
                "url":format!("https://example.org/ogc/{n}"),
                "text":link(&format!("https://example.org/ogc/{}",n+1))
            })
        })
        .collect();
    let error = session.run("service.layers", &json!({"url":root,"text":link("https://example.org/ogc/1"),"documents":docs})).unwrap_err();
    assert!(error.contains("four documents"), "{error}");
    let docs = json!([
        {"url":"https://example.org/ogc/a","text":link("https://example.org/ogc/b")},
        {"url":"https://example.org/ogc/b","text":link("https://example.org/ogc/a")}
    ]);
    assert!(
        session
            .run("service.layers", &json!({"url":root,"text":link("https://example.org/ogc/a"),"documents":docs}))
            .unwrap_err()
            .contains("loop")
    );
    for documents in [json!({}), json!([{}]), json!([{"url":root,"text":23}]), json!([{}, {}, {}, {}])] {
        assert!(session.run("service.layers", &json!({"url":root,"text":ogc_server::fixture_landing(),"documents":documents})).is_err());
    }
    for text in ["{", "{}", "{\"links\":[]}"] {
        assert!(session.run("service.layers", &json!({"url":root,"text":text})).is_err());
    }
}

#[test]
fn ogc_landing_http_failure_keeps_existing_map_layers() {
    use ogc_server::{CATALOG, ROOT, Route, Server, fixture_landing};
    for with_map in [false, true] {
        let mut landing: Value = serde_json::from_str(fixture_landing()).unwrap();
        if with_map {
            landing["links"].as_array_mut().unwrap().push(json!({"rel":"tilesets-map","href":"/map/tiles"}));
        }
        let mut bad = Route::new(CATALOG, "unavailable");
        bad.status = 503;
        let server = Server::new(vec![Route::new(ROOT, &landing.to_string()), bad]);
        let mut session = Session::new(Project::default());
        let result = session.run("service.layers", &json!({"url":server.root()}));
        if with_map {
            let result = result.unwrap();
            assert_eq!(result["count"], 1);
            assert!(result.get("next").is_none());
            assert!(result["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("503")));
        } else {
            assert!(result.unwrap_err().contains("503"));
        }
        server.assert_fixture_requests(&[ROOT, CATALOG]);
    }
}

#[test]
fn ogc_discovery_preserves_all_offers_within_the_collection_limit() {
    let collections: Vec<Value> = (0..1001)
        .map(|i| {
            json!({
                "id": format!("collection-{i}"),
                "links": [
                    {"rel":"tilesets-map", "href":format!("/tiles/{i}")},
                    {"rel":"items", "type":"application/geo+json", "href":format!("/items/{i}")}
                ]
            })
        })
        .collect();
    let mut session = Session::new(Project::default());
    let listing = session
        .run("service.layers", &json!({"url":"https://example.org/collections","text":json!({"collections":collections}).to_string()}))
        .unwrap();
    assert_eq!(listing["count"], 2002);
    assert_eq!(listing["layers"][2001]["name"], "collection-1000");
    assert_eq!(listing["layers"][2001]["offer"]["type"], "features");
}

#[test]
fn ogc_discovery_keeps_features_whose_id_matches_a_generated_tile_name() {
    let doc = json!({"collections":[
        {"id":"roads", "links":[{"rel":"tilesets-map","href":"/tiles/roads"}]},
        {"id":"roads (map tiles)", "links":[{"rel":"items","type":"application/geo+json","href":"/items/other"}]}
    ]});
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &json!({"url":"https://example.org/collections","text":doc.to_string()})).unwrap();
    assert_eq!(listing["count"], 2);
    assert_eq!(listing["layers"][0]["name"], listing["layers"][1]["name"]);
    assert_eq!(listing["layers"][0]["offer"]["type"], "tiles");
    assert_eq!(listing["layers"][1]["offer"]["type"], "features");
    assert_eq!(listing["layers"][1]["offer"]["url"], "https://example.org/items/other?limit=10000");
}

#[test]
fn ogc_group_import_refresh_and_persistence() {
    use ogc_server::{CATALOG, ITEMS, LINE_ITEMS, ROOT, Server, fixture_features};
    let server = Server::group_fixture();
    let mut s = Session::new(Project::default());
    let added = s.run("service.import", &json!({"url":server.root()})).unwrap();
    let group = added["group"].as_u64().unwrap() as u32;
    assert_eq!(s.map().service_groups.len(), 1);
    assert_eq!(s.map().service_groups[0].name, "My OGC Layer");
    assert_eq!(s.map().service_groups[0].children.len(), 2);
    assert_eq!(s.map().layers.len(), 2);
    let points = s.map().layer_by_name("Places").unwrap().id;
    let lines = s.map().layer_by_name("Routes").unwrap().id;
    assert_eq!(s.map().layer(lines).unwrap().geometry_type(), rustgis_data::GeometryType::Polyline);
    server.assert_fixture_requests(&[ROOT, CATALOG, ITEMS, LINE_ITEMS]);
    // One operation, one undo entry, including the parent and all children.
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layers.is_empty());
    assert!(s.map().service_groups.is_empty());
    s.run("edit.redo", &Value::Null).unwrap();
    s.run("layer.rename", &json!({"layer":points,"name":"My places"})).unwrap();
    s.run("layer.color", &json!({"layer":points,"color":"#123456"})).unwrap();
    s.run("layer.visibility", &json!({"layer":points,"visible":false})).unwrap();
    let style = serde_json::to_value(&s.map().layer(points).unwrap().renderer).unwrap();
    let old_lines = serde_json::to_value(&s.map().layer(lines).unwrap().dataset).unwrap();
    let project = serde_json::to_string(&s.project).unwrap();
    s = Session::new(Project::from_json(&project).unwrap());
    assert_eq!(s.map().service_groups[0].id, group);
    server.set(ITEMS, &fixture_features().replace("Test Place", "Updated Place"));
    let before = server.requests().len();
    s.run("service.refresh", &json!({"layer":points})).unwrap();
    let point = s.map().layer(points).unwrap();
    assert_eq!(point.name, "My places");
    assert!(!point.visible);
    assert_eq!(serde_json::to_value(&point.renderer).unwrap(), style);
    assert!(serde_json::to_string(&point.dataset).unwrap().contains("Updated Place"));
    assert_eq!(serde_json::to_value(&s.map().layer(lines).unwrap().dataset).unwrap(), old_lines);
    assert_eq!(server.requests()[before..].iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), [ROOT, CATALOG, ITEMS]);
    let before = server.requests().len();
    s.run("service.refresh", &json!({"group":group})).unwrap();
    assert_eq!(server.requests()[before..].iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), [ROOT, CATALOG, ITEMS, LINE_ITEMS]);
    assert_eq!(s.map().service_groups[0].children.iter().map(|c| c.layer).collect::<Vec<_>>(), [points, lines]);
    assert!(!s.map().layer(points).unwrap().visible);
}

#[test]
fn ogc_group_failures_are_atomic_and_membership_refreshes() {
    use ogc_server::{CATALOG, ITEMS, LINE_ITEMS, Server, fixture_collections, fixture_group_collections, fixture_lines};
    let server = Server::group_fixture();
    let mut s = Session::new(Project::default());
    server.set(LINE_ITEMS, "not GeoJSON");
    assert!(s.run("service.import", &json!({"url":server.root()})).is_err());
    assert!(s.map().layers.is_empty());
    assert!(s.map().service_groups.is_empty());
    server.set(LINE_ITEMS, fixture_lines());
    let group = s.run("service.import", &json!({"url":server.root()})).unwrap()["group"].clone();
    let old = serde_json::to_value(&s.project).unwrap();
    server.set(ITEMS, r#"{"type":"FeatureCollection","features":[]}"#);
    server.set(LINE_ITEMS, "not GeoJSON");
    assert!(s.run("service.refresh", &json!({"group":group})).is_err());
    assert_eq!(serde_json::to_value(&s.project).unwrap(), old);
    // A failed linked document must not masquerade as an empty collection list.
    server.set(CATALOG, "not JSON");
    assert!(s.run("service.refresh", &json!({"group":group})).is_err());
    assert_eq!(serde_json::to_value(&s.project).unwrap(), old);
    server.set(CATALOG, fixture_collections());
    s.run("service.refresh", &json!({"group":group})).unwrap();
    assert_eq!(s.map().layers.len(), 1);
    assert_eq!(s.map().service_groups[0].children.len(), 1);
    assert!(s.map().layers[0].dataset.features.is_empty());
    s.run("edit.undo", &Value::Null).unwrap();
    // Undo deliberately invalidates geometry caches; compare all persisted state except that revision.
    let mut restored = serde_json::to_value(&s.project).unwrap();
    for layer in restored["maps"][0]["layers"].as_array_mut().unwrap() {
        layer["geometry_rev"] = json!(0);
    }
    assert_eq!(restored, old);
    server.set(CATALOG, fixture_group_collections());
    server.set(LINE_ITEMS, fixture_lines());
    s.run("service.refresh", &json!({"group":group})).unwrap();
    let point = s.map().layer_by_name("Places").unwrap().id;
    s.run("layer.remove", &json!({"layer":point})).unwrap();
    s.run("service.refresh", &json!({"group":group})).unwrap();
    assert_eq!(s.map().layers.len(), 2);
    assert!(s.map().layer_by_name("Places").is_some());
    s.run("service.group", &json!({"group":group,"visible":false,"expanded":false,"name":"Survey"})).unwrap();
    assert!(s.map().layers.iter().all(|l| !l.visible));
    assert_eq!(s.map().service_groups[0].name, "Survey");
    assert!(!s.map().service_groups[0].expanded);
    // A newly advertised collection joins the group; a hidden group stays hidden.
    let mut more: Value = serde_json::from_str(fixture_group_collections()).unwrap();
    more["collections"].as_array_mut().unwrap().push(json!({"id":"new-routes","title":"New Routes",
        "links":[{"rel":"items","type":"application/geo+json","href":"/new-routes"}]}));
    server.set("/new-routes", fixture_lines());
    server.set(CATALOG, &more.to_string());
    s.run("service.refresh", &json!({"group":group})).unwrap();
    assert_eq!(s.map().service_groups[0].children.len(), 3);
    assert_eq!(s.map().layers.len(), 3);
    assert!(s.map().layers.iter().all(|l| !l.visible));
    assert!(s.map().layer_by_name("New Routes").is_some());
    s.run("service.group", &json!({"group":group,"remove":true})).unwrap();
    assert!(s.map().layers.is_empty());
    assert!(s.map().service_groups.is_empty());
}

#[test]
fn ogc_group_supplied_responses_never_fetch_and_need_every_child() {
    use ogc_server::{CATALOG, ROOT, fixture_features, fixture_group_collections, fixture_landing, fixture_lines};
    let mut s = Session::new(Project::default());
    let mut p = json!({"url":format!("https://example.org{ROOT}"), "text":fixture_landing(),
        "documents":[{"url":format!("https://example.org{CATALOG}?f=json&key=fixture-key"),"text":fixture_group_collections()}]});
    let plan = s.run("service.groupPlan", &p).unwrap();
    let points = plan["layers"][0]["offer"]["url"].clone();
    let lines = plan["layers"][1]["offer"]["url"].clone();
    p["responses"] = json!([{"url":points,"text":fixture_features()}]);
    assert!(s.run("service.import", &p).unwrap_err().contains("missing downloaded response"));
    assert!(s.map().layers.is_empty());
    p["responses"] = json!([{"url":points,"text":fixture_features()},{"url":lines,"text":fixture_lines()}]);
    s.run("service.import", &p).unwrap();
    assert_eq!(s.map().layers.len(), 2);
    assert_eq!(s.map().service_groups[0].children.len(), 2);
    for id in ["service.import", "service.refresh", "service.groupPlan"] {
        assert!(crate::cmd::tiles::NETWORK.contains(&id));
    }
}

#[test]
fn ogc_group_refresh_keeps_children_across_rotated_keys_and_rejects_bad_answers() {
    use ogc_server::{CATALOG, ROOT, fixture_features, fixture_group_collections, fixture_landing, fixture_lines};
    let doc = |key: &str| {
        json!({"url":format!("https://example.org{ROOT}"), "text":fixture_landing().replace("fixture-key", key),
            "documents":[{"url":format!("https://example.org{CATALOG}?f=json&key={key}"),
                "text":fixture_group_collections().replace("fixture-key", key)}]})
    };
    let answers = |s: &mut Session, p: &Value, points: &str, lines: &str| {
        let plan = s.run("service.groupPlan", p).unwrap();
        json!([{"url":plan["layers"][0]["offer"]["url"],"text":points},{"url":plan["layers"][1]["offer"]["url"],"text":lines}])
    };
    let mut s = Session::new(Project::default());
    let mut p = doc("fixture-key");
    p["responses"] = answers(&mut s, &p, fixture_features(), fixture_lines());
    let added = s.run("service.import", &p).unwrap();
    let group = added["group"].clone();
    let ids: Vec<u32> = s.map().service_groups[0].children.iter().map(|c| c.layer).collect();
    s.run("layer.rename", &json!({"layer":ids[0], "name":"Renamed"})).unwrap();
    s.map_mut().label_lock = Some(vec![(ids[0], 0), (ids[1], 0)]);

    // A rotated key changes every source address but not the collections: the children stay.
    let mut q = doc("rotated-key");
    q["responses"] = answers(&mut s, &q, fixture_features(), fixture_lines());
    q["group"] = group.clone();
    s.run("service.refresh", &q).unwrap();
    let g = &s.map().service_groups[0];
    assert_eq!(g.children.iter().map(|c| c.layer).collect::<Vec<_>>(), ids);
    assert!(g.children.iter().all(|c| c.source_url.contains("rotated-key")), "{:?}", g.children);
    assert_eq!(s.map().layers.len(), 2);
    assert_eq!(s.map().layer(ids[0]).unwrap().name, "Renamed");
    assert!(s.map().label_lock.as_ref().is_some_and(|l| l.is_empty()));

    // A child refresh follows a second rotation and records its new source.
    let mut c = doc("third-key");
    c["responses"] = answers(&mut s, &c, fixture_features(), fixture_lines());
    c["layer"] = json!(ids[1]);
    s.run("service.refresh", &c).unwrap();
    let g = &s.map().service_groups[0];
    assert!(g.children[1].source_url.contains("third-key") && g.children[0].source_url.contains("rotated-key"));
    assert_eq!(s.map().layers.len(), 2);

    // A FeatureCollection without a features array is malformed, not empty; `[]` is empty.
    let before = s.map().layer(ids[0]).unwrap().dataset.features.len();
    for bad in [r#"{"type":"FeatureCollection"}"#, r#"{"type":"FeatureCollection","features":null}"#] {
        let mut r = doc("rotated-key");
        r["responses"] = answers(&mut s, &r, bad, fixture_lines());
        r["group"] = group.clone();
        assert!(s.run("service.refresh", &r).unwrap_err().contains("FeatureCollection"));
        assert_eq!(s.map().layer(ids[0]).unwrap().dataset.features.len(), before);
    }
    let mut r = doc("rotated-key");
    r["responses"] = answers(&mut s, &r, r#"{"type":"FeatureCollection","features":[]}"#, fixture_lines());
    r["group"] = group.clone();
    s.run("service.refresh", &r).unwrap();
    assert_eq!(s.map().layer(ids[0]).unwrap().dataset.features.len(), 0);

    // A collections document past the listing limit can't be imported or refresh the group.
    let many: Vec<Value> = (0..2001)
        .map(|i| json!({"id":format!("c{i}"),"links":[{"rel":"items","type":"application/geo+json","href":format!("collections/c{i}/items")}]}))
        .collect();
    let mut t = doc("fixture-key");
    t["documents"][0]["text"] = json!(json!({"collections":many}).to_string());
    assert!(s.run("service.groupPlan", &t).unwrap_err().contains("more than 2000"));
    t["group"] = group;
    assert!(s.run("service.refresh", &t).unwrap_err().contains("more than 2000"));
    assert_eq!(s.map().layers.len(), 2);
}

#[test]
fn ogc_refresh_reconciles_fields_and_preserves_compatible_settings() {
    use ogc_server::{ITEMS, Server, fixture_features};
    for renderer in ["unique", "graduated", "proportional"] {
        for refresh_child in [false, true] {
            let server = Server::group_fixture();
            let features = |properties: Value| {
                let mut doc: Value = serde_json::from_str(fixture_features()).unwrap();
                doc["features"][0]["properties"] = properties;
                doc.to_string()
            };
            server.set(ITEMS, &features(json!({"name":"Place", "value":12, "keep":1})));
            let mut s = Session::new(Project::default());
            let group = s.run("service.import", &json!({"url":server.root()})).unwrap()["group"].clone();
            let layer = s.map().layer_by_name("Places").unwrap().id;
            let target = if refresh_child { json!({"layer":layer}) } else { json!({"group":group}) };
            s.run("layer.symbology", &json!({"layer":layer, "type":renderer, "field":"value"})).unwrap();
            let l = s.map_mut().layer_mut(layer).unwrap();
            l.labels.enabled = true;
            l.labels.default.field = "name".into();
            l.labels.default.size = 19.;
            l.definition_query = Some("value > 0".into());
            l.display_filters = ["value > 0", "keep = 1"]
                .map(|q| rustgis_map::effects::DisplayFilter { name: q.into(), min_scale: None, max_scale: None, where_: q.into() })
                .into();
            let style = l.renderer.clone();
            let labels = l.labels.clone();
            // Reordered and additional fields do not invalidate references by name.
            server.set(ITEMS, &features(json!({"extra":true, "name":"Updated", "value":20, "keep":1})));
            s.run("service.refresh", &target).unwrap();
            let l = s.map().layer(layer).unwrap();
            assert_eq!(l.renderer, style);
            assert_eq!(l.labels, labels);
            assert_eq!(l.definition_query.as_deref(), Some("value > 0"));
            assert_eq!(l.display_filters.len(), 2);
            // Geometry is unchanged, but fields used by the old configuration disappear.
            server.set(ITEMS, &features(json!({"replacement":"New", "keep":1})));
            s.run("service.refresh", &target).unwrap();
            let l = s.map().layer(layer).unwrap();
            assert!(matches!(l.renderer, rustgis_map::Renderer::Single(_)));
            assert!(!l.labels.enabled);
            assert!(l.labels.default.field.is_empty());
            assert_eq!(l.labels.default.size, 19.);
            assert!(l.definition_query.is_none());
            assert_eq!(l.display_filters.len(), 1);
            assert_eq!(l.display_filters[0].where_, "keep = 1");
            s.run("edit.undo", &Value::Null).unwrap();
            let l = s.map().layer(layer).unwrap();
            assert_eq!(l.renderer, style);
            assert_eq!(l.labels, labels);
            // Numeric renderers cannot use a field which has become text; unique values can.
            server.set(ITEMS, &features(json!({"name":"Place", "value":"text", "keep":1})));
            s.run("service.refresh", &target).unwrap();
            assert_eq!(matches!(s.map().layer(layer).unwrap().renderer, rustgis_map::Renderer::Single(_)), renderer != "unique");
        }
    }
}

#[test]
fn layer_color_honours_an_explicit_fill_alpha() {
    let mut s = world();
    let alpha = |s: &Session| s.map().layer_by_name("Countries").unwrap().renderer.base_symbol().fill.a();
    for a in ["00", "40", "FF"] {
        s.run("layer.color", &json!({"layer": "Countries", "color": format!("#336699{a}")})).unwrap();
        assert_eq!(alpha(&s), u8::from_str_radix(a, 16).unwrap(), "alpha {a} from the colour picker is kept");
    }
    // A plain #RRGGBB keeps the layer's translucent fill.
    s.run("layer.color", &json!({"layer": "Countries", "color": "#336699C8"})).unwrap();
    s.run("layer.color", &json!({"layer": "Countries", "color": "#996633"})).unwrap();
    assert_eq!(alpha(&s), 0xC8);
    // It never makes the fill fainter than the floor `with_fill` gives a polygon.
    s.run("layer.color", &json!({"layer": "Countries", "color": "#33669940"})).unwrap();
    s.run("layer.color", &json!({"layer": "Countries", "color": "#996633"})).unwrap();
    assert_eq!(alpha(&s), 150);
}

#[test]
fn layer_color_leaves_the_outline_alone() {
    let mut s = world();
    let symbol = |s: &Session| s.map().layer_by_name("Countries").unwrap().renderer.base_symbol();
    let rgb = |c: rustgis_map::Rgba| (c.r(), c.g(), c.b());
    // An outline that was never set stays as it was too.
    let default_stroke = symbol(&s).stroke;
    s.run("layer.color", &json!({"layer": "Countries", "color": "#336699"})).unwrap();
    assert_eq!(symbol(&s).stroke, default_stroke, "a default outline is not re-derived from the new fill");
    // A chosen outline survives a fill change.
    s.run("layer.outline", &json!({"layer": "Countries", "color": "#00FF00"})).unwrap();
    let before = symbol(&s).stroke;
    s.run("layer.color", &json!({"layer": "Countries", "color": "#996633"})).unwrap();
    assert_eq!(symbol(&s).stroke, before, "changing the fill must not change a chosen outline");
    assert_eq!(rgb(symbol(&s).fill), (0x99, 0x66, 0x33), "changing the fill must set the requested RGB");
}

#[test]
fn buffer_point_adds_only_the_buffer() {
    let mut s = world();
    let before = s.map().layers.len();
    let v =
        s.run("gp.bufferPoint", &json!({"lon": 2.35, "lat": 48.85, "distance": 1, "units": "Kilometers", "name": "Buffer 1 km"})).unwrap();
    assert_eq!(s.map().layers.len(), before + 1, "the clicked point isn't added");
    let l = s.map().layer(v["layer"].as_u64().unwrap() as u32).unwrap();
    assert_eq!(l.name, "Buffer 1 km");
    let e = l.dataset.extent();
    assert!((e.max_y - e.min_y - 2.0 / 111.2).abs() < 0.002, "a 1 km radius: {e:?}");
    assert!(s.run("gp.bufferPoint", &json!({"lon": 200, "lat": 0, "distance": 1})).is_err());
    assert!(s.run("gp.bufferPoint", &json!({"lon": 0, "lat": 0})).is_err());
}

#[test]
fn raster_stretch_custom_bounds_per_band() {
    use rustgis_map::Stretch;
    let mut s = with_raster();
    let layer = |s: &Session, i: usize| s.map().layers.get(i).and_then(|l| l.raster.as_ref()).unwrap().clone();
    let stretch = |s: &Session| layer(s, s.map().layers.len() - 1).style.stretch;
    // Three bands with ranges of their own.
    let ranges = [[0.0, 100.0], [0.0, 200.0], [10.0, 50.0]];
    {
        let rl = s.map_mut().layers.last_mut().and_then(|l| l.raster.as_mut()).unwrap();
        let mut d = (**rl.data.as_ref().unwrap()).clone();
        assert!(d.bands.len() >= 3);
        for (b, [lo, hi]) in d.bands.iter_mut().zip(ranges) {
            b.stats.min = lo;
            b.stats.max = hi;
        }
        rl.data = Some(std::sync::Arc::new(d));
    }
    s.run("raster.symbology", &json!({"type": "rgb"})).unwrap();
    s.run("raster.stretch", &json!({"type": "minMax"})).unwrap();
    // Custom starts from the range each band is drawn over now.
    s.run("raster.stretch", &json!({"type": "custom"})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom_bands(ranges));
    // A bound for each band; null keeps that band's current one.
    s.run("raster.stretch", &json!({"type": "custom", "min": [null, 5, null], "max": [90, null, null]})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom_bands([[0.0, 90.0], [5.0, 200.0], [10.0, 50.0]]));
    // A number is for every band.
    s.run("raster.stretch", &json!({"type": "custom", "min": 1, "max": 99})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom(1.0, 99.0));
    s.run("raster.stretch", &json!({"type": "custom", "min": 1})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom_bands([[1.0, 99.0]; 3]), "the other bound is the range drawn now");
    // Bad bounds are refused, naming the band, and leave the stretch alone.
    let before = stretch(&s);
    for p in [
        json!({"type": "custom", "min": [300, null, null]}),
        json!({"type": "custom", "max": [null, null, 1]}),
        json!({"type": "custom", "min": [1, 2]}),
        json!({"type": "custom", "min": [1, "x", null]}),
        json!({"type": "custom", "min": [1, 2, 3, 4]}),
    ] {
        assert!(s.run("raster.stretch", &p).is_err(), "{p}");
        assert_eq!(stretch(&s), before, "{p}");
    }
    let e = s.run("raster.stretch", &json!({"type": "custom", "max": [null, null, 1]})).unwrap_err();
    assert!(e.to_string().contains("blue"), "{e}");
    // A project round trip keeps them.
    let json = serde_json::to_string(&stretch(&s)).unwrap();
    assert_eq!(serde_json::from_str::<Stretch>(&json).unwrap(), stretch(&s));
    // A channel moved to another band is redrawn over that band's range, the others keep theirs.
    s.run("raster.stretch", &json!({"type": "custom", "min": [2, 3, 12], "max": [80, 150, 40]})).unwrap();
    s.run("raster.bands", &json!({"red": 2})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom_bands([[0.0, 200.0], [3.0, 150.0], [12.0, 40.0]]));
    s.run("raster.bands", &json!({"red": 2})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom_bands([[0.0, 200.0], [3.0, 150.0], [12.0, 40.0]]), "same bands, same bounds");
    // Another stretch type leaves bands alone.
    s.run("raster.stretch", &json!({"type": "minMax"})).unwrap();
    s.run("raster.bands", &json!({"red": 3})).unwrap();
    assert_eq!(stretch(&s), Stretch::MinMax);
    // One band takes one pair only.
    s.run("raster.symbology", &json!({"type": "stretch", "band": 1})).unwrap();
    assert!(s.run("raster.stretch", &json!({"type": "custom", "min": [1, 2, 3]})).is_err());
    s.run("raster.stretch", &json!({"type": "custom", "min": 3, "max": 7})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom(3.0, 7.0));
}

#[test]
fn raster_stretch_like_copies_each_band() {
    use rustgis_map::Stretch;
    let mut s = with_raster();
    s.run("raster.symbology", &json!({"type": "rgb"})).unwrap();
    s.run("raster.stretch", &json!({"type": "custom", "min": [1, 2, 3], "max": [10, 20, 30]})).unwrap();
    s.run("layer.addDataBytes", &json!({"fileName": "scene.tif", "data": utm_tiff_b64()})).unwrap();
    let (from, to) = ("scene", "scene (2)");
    s.run("raster.symbology", &json!({"layer": to, "type": "rgb"})).unwrap();
    s.run("raster.stretch", &json!({"layer": to, "like": from})).unwrap();
    let st = s.map().layer_by_name(to).and_then(|l| l.raster.as_ref()).unwrap().style.stretch;
    assert_eq!(st, Stretch::custom_bands([[1.0, 10.0], [2.0, 20.0], [3.0, 30.0]]));
}

#[test]
fn raster_stretch_custom_bounds() {
    use rustgis_map::Stretch;
    let mut s = with_raster();
    // One band: a missing bound is that band's own statistic (on RGB it is the range drawn now).
    s.run("raster.symbology", &json!({"type": "stretch", "band": 1})).unwrap();
    let stretch = |s: &Session| s.map().layers.last().unwrap().raster.as_ref().unwrap().style.stretch;
    let stats = s.map().layers.last().unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().bands[0].stats.clone();
    s.run("raster.stretch", &json!({"type": "custom", "min": 10, "max": 200})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom(10.0, 200.0));
    // A missing bound is the layer's own statistic.
    s.run("raster.stretch", &json!({"type": "custom", "min": 10})).unwrap();
    assert_eq!(stretch(&s), Stretch::custom(10.0, stats.max));
    // The bounds survive DRA and a project round trip.
    s.run("raster.dra", &json!({"on": true})).unwrap();
    let kept = serde_json::to_string(&stretch(&s)).unwrap();
    assert_eq!(serde_json::from_str::<Stretch>(&kept).unwrap(), stretch(&s));
    // Bad bounds are refused and leave the stretch alone.
    let before = stretch(&s);
    assert!(s.run("raster.stretch", &json!({"type": "custom", "min": 5, "max": 5})).is_err());
    assert!(s.run("raster.stretch", &json!({"type": "custom", "min": 9, "max": 1})).is_err());
    assert!(s.run("raster.stretch", &json!({"type": "custom", "min": 1e308})).is_err());
    // A bound that was given but isn't a number is refused, not replaced by the layer's own.
    assert!(s.run("raster.stretch", &json!({"type": "custom", "min": "NaN", "max": 100})).is_err());
    assert!(s.run("raster.stretch", &json!({"type": "custom", "min": "abc"})).is_err());
    assert_eq!(stretch(&s), before);
    // A band with one value still gets a range to edit from.
    let mut flat = with_raster();
    flat.run("raster.symbology", &json!({"type": "stretch", "band": 1})).unwrap();
    {
        let rl = flat.map_mut().layers.last_mut().and_then(|l| l.raster.as_mut()).unwrap();
        let mut d = (**rl.data.as_ref().unwrap()).clone();
        d.bands[0].stats.min = 7.0;
        d.bands[0].stats.max = 7.0;
        rl.data = Some(std::sync::Arc::new(d));
    }
    flat.run("raster.stretch", &json!({"type": "custom"})).unwrap();
    assert_eq!(stretch(&flat), Stretch::custom(6.5, 7.5));
    // The padding follows the value's size, and statistics that aren't numbers are refused.
    let set_stats = |s: &mut Session, v: f64| {
        let rl = s.map_mut().layers.last_mut().and_then(|l| l.raster.as_mut()).unwrap();
        let mut d = (**rl.data.as_ref().unwrap()).clone();
        d.bands[0].stats.min = v;
        d.bands[0].stats.max = v;
        rl.data = Some(std::sync::Arc::new(d));
    };
    set_stats(&mut flat, 9007199254740992.0);
    flat.run("raster.stretch", &json!({"type": "custom"})).unwrap();
    assert!(matches!(stretch(&flat), Stretch::Custom { min, max, .. } if min < 9007199254740992.0 && max > 9007199254740992.0));
    let before = stretch(&flat);
    for v in [f64::NAN, f64::INFINITY] {
        set_stats(&mut flat, v);
        assert!(flat.run("raster.stretch", &json!({"type": "custom"})).is_err(), "{v}");
    }
    assert_eq!(stretch(&flat), before);
    // Projects saved before the custom stretch still load.
    assert_eq!(serde_json::from_str::<Stretch>("\"MinMax\"").unwrap(), Stretch::MinMax);
}

#[test]
fn default_raster_style_applies_to_new_rasters() {
    use rustgis_map::{RasterRenderer, Resampling, Stretch};
    let mut s = world();
    s.dirty = false;
    let undo = s.undo_label();
    // The layer just added (rasters go in under the vectors, the newest on top of them).
    let style = |s: &Session| s.map().layer(s.active_layer.unwrap()).unwrap().raster.as_ref().unwrap().style.clone();
    // Without settings: the inverted grey stretch.
    let shown = s.run("raster.setDefaultStyle", &json!({})).unwrap();
    assert_eq!(shown["ramp"], "Greys");
    assert_eq!(shown["invert"], true);

    let shown = s
        .run(
            "raster.setDefaultStyle",
            &json!({"ramp": "viridis", "invert": false, "stretch": "standardDeviation", "n": 2.5, "resampling": "bilinear"}),
        )
        .unwrap();
    assert_eq!(shown, json!({"ramp": "Viridis", "invert": false, "stretch": "standardDeviation", "n": 2.5, "resampling": "Bilinear"}));
    assert!(!s.dirty && s.undo_label() == undo, "an app setting, not a project change");
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let st = style(&s);
    assert_eq!(st.renderer, RasterRenderer::Stretch { band: 0, ramp: rustgis_map::Ramp::Viridis, invert: false });
    assert_eq!(st.stretch, Stretch::StandardDeviation { n: 2.5 });
    assert_eq!(st.resampling, Resampling::Bilinear);
    // Colour images keep their colours.
    s.run("layer.addDataBytes", &json!({"fileName": "scene.tif", "data": two_class_b64()})).unwrap();
    assert!(matches!(style(&s).renderer, RasterRenderer::Rgb { .. }), "{:?}", style(&s).renderer);
    assert_eq!(style(&s).resampling, Resampling::Nearest);

    // From a layer: its ramp, inversion, stretch and resampling.
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64(), "name": "dem2"})).unwrap();
    s.run("raster.symbology", &json!({"type": "stretch", "ramp": "Batlow", "invert": true})).unwrap();
    s.run("raster.stretch", &json!({"type": "minMax"})).unwrap();
    s.run("raster.resampling", &json!({"type": "nearest"})).unwrap();
    let shown = s.run("raster.setDefaultStyle", &json!({"fromLayer": true})).unwrap();
    assert_eq!(shown, json!({"ramp": "Batlow", "invert": true, "stretch": "minMax", "resampling": "Nearest"}));
    // A custom stretch is in one raster's units: refused either way, the settings unchanged.
    s.run("raster.stretch", &json!({"type": "custom", "min": 1, "max": 2})).unwrap();
    assert!(s.run("raster.setDefaultStyle", &json!({"fromLayer": true})).is_err());
    assert!(s.run("raster.setDefaultStyle", &json!({"stretch": "custom"})).is_err());
    assert!(s.run("raster.setDefaultStyle", &json!({"ramp": "no such ramp"})).is_err());
    assert!(s.run("raster.setDefaultStyle", &json!({"resampling": "cubic-ish"})).is_err());
    s.run("raster.symbology", &json!({"type": "classify"})).unwrap();
    assert!(s.run("raster.setDefaultStyle", &json!({"fromLayer": true})).is_err(), "only a stretch");
    assert_eq!(s.raster_defaults.ramp, rustgis_map::Ramp::Batlow);

    // Reset: back to the grey stretch, and new rasters are drawn as before.
    s.run("raster.setDefaultStyle", &json!({"reset": true})).unwrap();
    assert_eq!(s.raster_defaults, rustgis_map::RasterDefaults::default());
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64(), "name": "dem3"})).unwrap();
    assert_eq!(style(&s).renderer, RasterRenderer::Stretch { band: 0, ramp: rustgis_map::Ramp::Greys, invert: true });
}

#[test]
fn layer_groups_hold_related_layers_together() {
    use crate::render::View;
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    let depth = s.undo_depth();
    let r = s.run("group.add", &json!({"name": "Places", "layers": ["Lakes", "Cities"]})).unwrap();
    let g = r["group"].as_u64().unwrap() as u32;
    assert_eq!(s.undo_depth(), depth + 1);
    // Gathered at the topmost, in their order.
    assert_eq!(names(&s), ["Cities", "Lakes", "Rivers", "Countries"]);
    let id = |s: &Session, n: &str| s.map().layer_by_name(n).unwrap().id;
    assert_eq!(s.map().group(g).unwrap().children, [id(&s, "Cities"), id(&s, "Lakes")]);
    assert!(s.map().layer(g).is_none(), "a group's id is no layer's");
    // Hiding the group hides its layers on the map (their own boxes stay ticked).
    s.run("map.goToXY", &json!({"lon": 10.0, "lat": 48.0, "scale": 20_000_000})).unwrap();
    let png = |s: &mut Session| {
        let v = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
        crate::render::render(s.map_mut(), &v).unwrap().pixmap.data().to_vec()
    };
    let shown = png(&mut s);
    for n in ["Cities", "Lakes"] {
        s.run("layer.visibility", &json!({"layer": n, "visible": false})).unwrap();
    }
    let without = png(&mut s);
    assert_ne!(shown, without);
    for n in ["Cities", "Lakes"] {
        s.run("layer.visibility", &json!({"layer": n, "visible": true})).unwrap();
    }
    s.run("group.set", &json!({"group": "places", "visible": false})).unwrap();
    assert!(s.map().layer_by_name("Cities").unwrap().visible);
    assert!(!s.map().shows(s.map().layer_by_name("Cities").unwrap()));
    assert_eq!(png(&mut s), without);
    // Toggled back with no other property.
    assert_eq!(s.run("group.set", &json!({"group": g})).unwrap()["visible"], true);
    assert_eq!(png(&mut s), shown);
    // Its scale range applies on top of the layers' own: out beyond 1:1,000,000 at 1:20,000,000.
    s.run("group.scaleRange", &json!({"group": g, "min": 1_000_000})).unwrap();
    assert_eq!(png(&mut s), without);
    s.run("group.scaleRange", &json!({"group": g})).unwrap();
    // Fully see-through draws nothing; half see-through draws something in between.
    s.run("group.set", &json!({"group": g, "transparency": 100})).unwrap();
    assert_eq!(png(&mut s), without);
    s.run("group.set", &json!({"group": g, "transparency": 50})).unwrap();
    let half = png(&mut s);
    assert!(half != shown && half != without);
    s.run("group.set", &json!({"group": g, "transparency": 0, "name": "Water and towns"})).unwrap();
    // Folding is a view change: no undo step. A visibility that isn't a bool is an error, not a toggle.
    let before = s.undo_depth();
    s.run("group.expand", &json!({"group": g})).unwrap();
    assert_eq!(s.undo_depth(), before);
    assert!(!s.map().group(g).unwrap().expanded);
    assert!(s.run("group.set", &json!({"group": g, "visible": "no"})).is_err());
    assert!(s.run("group.set", &json!({"group": g, "name": 5})).is_err());
    assert!(s.map().group(g).unwrap().visible);
    // Identify and the map's selection skip a hidden group's layers.
    s.run("group.set", &json!({"group": g, "visible": false})).unwrap();
    let held = s.map().held_by_group(s.map().scale());
    assert!(held.contains(&id(&s, "Cities")) && !held.contains(&id(&s, "Rivers")));
    s.run("group.set", &json!({"group": g, "visible": true})).unwrap();
    // Listed for scripts and the MCP server.
    let info = s.run("info.project", &Value::Null).unwrap();
    assert_eq!(info["groups"][0]["name"], "Water and towns");
    assert_eq!(info["groups"][0]["layers"].as_array().unwrap().len(), 2);
    // A layer moves in next to the others, and out again.
    s.run("layer.group", &json!({"layer": "Countries", "group": g})).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes", "Countries", "Rivers"]);
    s.run("layer.group", &json!({"layer": "Countries", "group": null})).unwrap();
    assert_eq!(s.map().group(g).unwrap().children.len(), 2);
    assert!(s.run("layer.group", &json!({"layer": "Countries", "group": "nope"})).is_err());
    assert!(s.run("layer.group", &json!({"layer": "Countries"})).is_err());
    // A group's layer moves only within it; another layer moves past the group.
    assert!(s.run("layer.moveDown", &json!({"layer": "Lakes"})).is_err());
    s.run("layer.moveUp", &json!({"layer": "Lakes"})).unwrap();
    assert_eq!(names(&s), ["Lakes", "Cities", "Countries", "Rivers"]);
    assert!(s.run("layer.move", &json!({"layer": "Cities", "to": 3})).is_err());
    s.run("layer.moveUp", &json!({"layer": "Countries"})).unwrap();
    assert_eq!(names(&s), ["Countries", "Lakes", "Cities", "Rivers"]);
    s.run("layer.move", &json!({"layer": "Countries", "to": 1})).unwrap();
    assert_eq!(names(&s), ["Lakes", "Cities", "Countries", "Rivers"]);
    // The group moves as a whole.
    s.run("group.move", &json!({"group": g, "to": 1})).unwrap();
    assert_eq!(names(&s), ["Countries", "Lakes", "Cities", "Rivers"]);
    s.run("group.move", &json!({"group": g, "to": 1e9})).unwrap();
    assert_eq!(names(&s), ["Countries", "Rivers", "Lakes", "Cities"]);
    s.run("group.move", &json!({"group": g, "to": 0})).unwrap();
    assert!(s.run("group.move", &json!({"group": g})).is_err());
    // Saved and opened again, with no dataset of its own.
    let dir = std::env::temp_dir().join(format!("rustgis-groups-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("groups.rgis");
    s.run("project.saveAs", &json!({"path": path.display().to_string()})).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
    let tg = t.map().group(g).unwrap();
    assert_eq!((tg.name.as_str(), tg.expanded, tg.children.len()), ("Water and towns", false, 2));
    std::fs::remove_dir_all(&dir).ok();
    // Removing a layer takes it out of its group.
    s.run("layer.remove", &json!({"layer": "Lakes"})).unwrap();
    assert_eq!(s.map().group(g).unwrap().children, [id(&s, "Cities")]);
    // Ungrouping keeps the layers; removing the group removes them.
    s.run("group.ungroup", &json!({"group": g})).unwrap();
    assert!(s.map().groups.is_empty() && s.map().layer_by_name("Cities").is_some());
    let g = s.run("group.add", &json!({"layers": ["Cities", "Rivers"]})).unwrap()["group"].as_u64().unwrap();
    assert_eq!(s.map().groups[0].name, "New Group");
    s.run("group.remove", &json!({"group": g})).unwrap();
    assert_eq!(names(&s), ["Countries"]);
    // Undo brings everything back.
    while s.undo_depth() > depth {
        s.run("edit.undo", &Value::Null).unwrap();
    }
    assert!(s.map().groups.is_empty());
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
}

#[test]
fn hand_edited_layer_groups_open_bounded_and_unambiguous() {
    let mut s = world();
    s.run("group.add", &json!({"name": "A", "layers": ["Cities"]})).unwrap();
    let mut v: Value = serde_json::from_str(&serde_json::to_string(&s.project).unwrap()).unwrap();
    let map = &mut v["maps"][0];
    let cities = s.map().layer_by_name("Cities").unwrap().id;
    let lakes = s.map().layer_by_name("Lakes").unwrap().id;
    // A group with no dataset parses; a child twice, a missing child, a layer's id, a zero id,
    // junk numbers and a group without children.
    map["groups"] = json!([
        {"id": 900, "name": "Water", "children": [lakes, cities, 12345], "transparency": 1e9, "min_scale": -5.0},
        {"id": 901, "name": "Again", "children": [lakes]},
        {"id": cities, "name": "Clash", "children": []},
        {"id": 0, "name": "Zero", "children": []},
        {"id": 902, "name": "Empty"}
    ]);
    let p = Project::from_json(&v.to_string()).unwrap();
    let m = p.maps.first().unwrap();
    let names: Vec<&str> = m.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Water", "Again", "Empty"]);
    let water = &m.groups[0];
    assert_eq!(water.children, [lakes, cities]);
    assert_eq!((water.transparency, water.min_scale, water.visible), (100.0, None, true));
    assert!(m.groups[1].children.is_empty());
    // Gathered: Lakes moved up under Cities, past Rivers.
    let order: Vec<&str> = m.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(order, ["Cities", "Lakes", "Rivers", "Countries"]);
}

#[test]
fn a_new_layer_goes_above_a_group_rather_than_into_it() {
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    // Rivers (lines) over Countries (polygons) in one group: a new polygon layer ranks just above
    // Countries, which would split the group.
    s.run("group.add", &json!({"name": "Base", "layers": ["Rivers", "Countries"]})).unwrap();
    assert_eq!(names(&s), ["Cities", "Rivers", "Countries", "Lakes"]);
    let square = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}]}"#;
    s.run("layer.addDataText", &json!({"fileName": "p.geojson", "text": square, "name": "Square"})).unwrap();
    assert_eq!(names(&s), ["Cities", "Square", "Rivers", "Countries", "Lakes"]);
}

#[test]
fn a_layer_drops_into_and_out_of_a_group_at_a_place() {
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    let g = s.run("group.add", &json!({"name": "Water", "layers": ["Rivers", "Lakes"]})).unwrap()["group"].clone();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    let members = |s: &Session| {
        let g = &s.map().groups[0];
        s.map().layers.iter().filter(|l| g.children.contains(&l.id)).map(|l| l.name.clone()).collect::<Vec<_>>()
    };
    let depth = s.undo_depth();
    // Into the group, between its layers: one step.
    s.run("layer.group", &json!({"layer": "Countries", "group": g, "before": "Lakes"})).unwrap();
    assert_eq!(names(&s), ["Cities", "Rivers", "Countries", "Lakes"]);
    assert_eq!(members(&s), ["Rivers", "Countries", "Lakes"]);
    assert_eq!(s.undo_depth(), depth + 1);
    // Out of it, to the top.
    s.run("layer.group", &json!({"layer": "Rivers", "group": null, "before": "Cities"})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Cities", "Countries", "Lakes"]);
    assert_eq!(members(&s), ["Countries", "Lakes"]);
    // Out of a group to a place inside another group's run: above that group.
    s.run("layer.group", &json!({"layer": "Cities", "group": g, "before": "Countries"})).unwrap();
    s.run("layer.group", &json!({"layer": "Cities", "group": null, "before": "Lakes"})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Cities", "Countries", "Lakes"]);
    // Out of a group, staying where it is, when no place is given.
    s.run("layer.group", &json!({"layer": "Lakes", "group": null})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Cities", "Countries", "Lakes"]);
    // A place in the group must be one of its layers, and not the layer itself.
    assert!(s.run("layer.group", &json!({"layer": "Rivers", "group": g, "before": "Cities"})).is_err());
    assert!(s.run("layer.group", &json!({"layer": "Rivers", "group": null, "before": "Rivers"})).is_err());
    assert_eq!(members(&s), ["Countries"]);
}

#[test]
fn a_moved_group_never_lands_inside_another() {
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    s.run("group.add", &json!({"name": "A", "layers": ["Cities"]})).unwrap();
    s.run("group.add", &json!({"name": "B", "layers": ["Lakes", "Countries"]})).unwrap();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    // Position 2 among the others (Rivers, Lakes, Countries) is between Lakes and Countries.
    s.run("group.move", &json!({"group": "A", "to": 2})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Cities", "Lakes", "Countries"]);
}

#[test]
fn a_layer_leaving_a_group_never_splits_it() {
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    s.run("group.add", &json!({"name": "A", "layers": ["Rivers", "Lakes", "Countries"]})).unwrap();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    // Out of the middle: above the group.
    s.run("layer.group", &json!({"layer": "Lakes", "group": null})).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes", "Rivers", "Countries"]);
    s.run("layer.group", &json!({"layer": "Lakes", "group": "A", "before": "Countries"})).unwrap();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    // Into the group it is in: nothing moves.
    s.run("layer.group", &json!({"layer": "Lakes", "group": "A"})).unwrap();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    // A bad scale is an error, not a cleared bound.
    s.run("group.scaleRange", &json!({"group": "A", "min": 1e6, "max": 1e3})).unwrap();
    for bad in [json!({"group": "A", "min": -4}), json!({"group": "A", "max": "big"})] {
        assert!(s.run("group.scaleRange", &bad).is_err());
    }
    assert_eq!(s.map().groups[0].min_scale, Some(1e6));
    // Into an empty group, and into a new group: the same.
    s.run("group.add", &json!({"name": "Empty"})).unwrap();
    s.run("layer.group", &json!({"layer": "Lakes", "group": "Empty"})).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes", "Rivers", "Countries"]);
    s.run("layer.group", &json!({"layer": "Lakes", "group": "A", "before": "Countries"})).unwrap();
    s.run("group.add", &json!({"name": "B", "layers": ["Lakes"]})).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes", "Rivers", "Countries"]);
}

#[test]
fn hostile_group_params_never_panic() {
    let junk = [
        json!({"group": 1, "to": 1e30, "before": 99999, "min": "x", "max": -1, "transparency": 1e300}),
        json!({"group": "A", "to": -1, "before": "A", "layers": [1, 2, 2, "nope"], "visible": 3, "on": "yes"}),
        json!({"group": "A", "layer": "Cities", "before": "Cities", "transparency": "half", "min": 1e308, "max": 1e-308}),
        json!({"group": null, "layer": "Rivers", "before": "Lakes", "name": "", "to": 0}),
        json!({"group": [1], "layer": {"x": 1}, "layers": "Cities", "to": 2.5}),
        json!({"group": "A", "to": 1e9, "name": "x".repeat(10_000)}),
    ];
    let ids = ["group.add", "group.set", "group.expand", "group.scaleRange", "group.move", "group.ungroup", "group.remove", "layer.group"];
    for id in ids {
        for p in &junk {
            let mut s = world();
            s.run("group.add", &json!({"name": "A", "layers": ["Rivers", "Lakes"]})).unwrap();
            s.run("group.add", &json!({"name": "B"})).unwrap();
            let _ = s.run(id, p);
            // The groups stay whole: each group's layers sit together in the drawing order.
            let m = s.map();
            for g in &m.groups {
                let at: Vec<usize> = m.layers.iter().enumerate().filter(|(_, l)| g.children.contains(&l.id)).map(|(i, _)| i).collect();
                assert!(at.windows(2).all(|w| w[1] == w[0] + 1), "{id} {p}: {at:?}");
                assert!(g.children.iter().all(|c| m.layer(*c).is_some()), "{id} {p}");
                assert!(g.name.chars().count() <= 200 && (0.0..=100.0).contains(&g.transparency));
            }
        }
    }
}

/// Large elevation models come as tiles in a projected coordinate system (#155): mosaic them on
/// their own grid, clip by a polygon layer, share one stretch, and say when a raster is an overview.
#[test]
fn dem_tiles_mosaic_clip_by_polygon_share_a_stretch_and_report_overviews() {
    use base64::Engine;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let utm = Crs::from_epsg(25832).unwrap();
    // 10 × 10 tiles of 100 m cells: values 100 + column in the west, 200 + column in the east.
    // Each tile has one no-data cell, `hole` cells from its top-left.
    let tile = |name: &str, x0: f64, base: f32, cell: f64, hole: usize| {
        let n = (1000.0 / cell) as u32;
        let mut z: Vec<f32> = (0..n * n).map(|i| base + (i % n) as f32).collect();
        if let Some(v) = z.get_mut(hole) {
            *v = -9999.0;
        }
        let gt = GeoTransform { origin_x: x0, origin_y: 5_700_000.0, pixel_width: cell, pixel_height: -cell };
        Raster::new(name, n, n, vec![z], gt, utm, Some(-9999.0), ColorHint::Gray, "Float32").unwrap()
    };
    let mut s = world();
    for (name, r) in [
        ("west", tile("west", 360_000.0, 100.0, 100.0, 99)),
        ("east", tile("east", 361_000.0, 200.0, 100.0, 99)),
        ("over", tile("over", 360_000.0, 900.0, 100.0, 0)),
        ("coarse", tile("coarse", 362_000.0, 0.0, 200.0, 0)),
        // Half a cell off the others' grid.
        ("shifted", tile("shifted", 362_050.0, 0.0, 100.0, 0)),
    ] {
        let data = base64::engine::general_purpose::STANDARD.encode(geotiff::write(&r, 0).unwrap());
        s.run("layer.addDataBytes", &json!({"fileName": format!("{name}.tif"), "data": data})).unwrap();
    }
    // Tiny cells far apart: more columns between them than a raster can have.
    for (name, x0) in [("near", 0.0), ("far", 100.0)] {
        let gt = GeoTransform { origin_x: x0, origin_y: 1e-6, pixel_width: 1e-9, pixel_height: -1e-9 };
        let r = Raster::new(name, 10, 10, vec![vec![1.0; 100]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let bytes = geotiff::write(&r, 0).unwrap();
        crate::cmd::raster::add_raster(&mut s, name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
    }
    let raster =
        |s: &Session, v: &Value| s.map().layer(v["layer"].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();

    // Mosaic: one grid in the tiles' own CRS and cell size; the first input wins where they overlap.
    let dir = std::env::temp_dir().join(format!("rustgis-mosaic-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("dem.tif").display().to_string();
    let v = s.run("raster.mosaic", &json!({"inputs": ["west", "over", "east"], "name": "dem", "path": path})).unwrap();
    assert_eq!(v["path"], json!(path));
    let m = raster(&s, &v);
    assert_eq!((m.width, m.height, m.crs, m.cell_size()), (20, 10, utm, (100.0, 100.0)));
    assert_eq!((m.transform.origin_x, m.transform.origin_y), (360_000.0, 5_700_000.0));
    assert_eq!((m.value(0, 3, 0), m.value(0, 15, 2)), (Some(103.0), Some(205.0)));
    // West's no-data corner is filled from the overlapping tile; east's stays no-data.
    assert_eq!((m.value(0, 0, 0), m.value(0, 9, 9), m.value(0, 19, 9)), (Some(100.0), Some(909.0), None));
    let written = rustgis_data::raster::read_path(std::path::Path::new(&path)).unwrap();
    assert_eq!((written.width, written.height, written.crs), (20, 10, utm));
    std::fs::remove_dir_all(&dir).ok();
    // Bare file names read files too: tasks and notebooks from elsewhere may not run it.
    assert!(crate::cmd::items::touches_files("raster.mosaic", &json!({"inputs": ["a.tif", "b.tif"]})));
    for (bad, why) in [
        (json!({"inputs": ["west"]}), "two or more"),
        (json!({"inputs": ["west", "coarse"]}), "cell size"),
        (json!({"inputs": ["west", "shifted"]}), "line up"),
        (json!({"inputs": ["near", "far"]}), "too far"),
        (json!({"inputs": ["west", "Countries"]}), "no raster layer"),
        (json!({"inputs": ["west", 3]}), "file path"),
    ] {
        let e = s.run("raster.mosaic", &bad).unwrap_err().to_string();
        assert!(e.contains(why), "{bad}: {e}");
    }

    // Clip by a polygon layer: the window around it, cells outside it no-data.
    let tr = m.transformer();
    let ll = |x: f64, y: f64| {
        let c = tr.to_lonlat(rustgis_geom::Coord::new(x, y)).unwrap();
        json!([c.x, c.y])
    };
    s.run("layer.template", &json!({"template": "polygons", "name": "City"})).unwrap();
    // A triangle, so the mask isn't just the window.
    let ring = [ll(360_500.0, 5_700_000.0), ll(361_500.0, 5_700_000.0), ll(361_500.0, 5_699_500.0), ll(360_500.0, 5_700_000.0)];
    s.run("edit.addFeature", &json!({"layer": "City", "geometry": {"type": "Polygon", "coordinates": [ring]}})).unwrap();
    let v = s.run("raster.process", &json!({"layer": "dem", "operation": "clip", "polygon": "City"})).unwrap();
    let c = raster(&s, &v);
    // The diagonal runs from column 5 at row 0 to column 15 at row 5: rows of 9, 7, 5, 3 and 1
    // cell centres inside, in a window trimmed to columns 6–14.
    assert_eq!((c.width, c.height, c.crs), (9, 5, utm));
    assert_eq!(c.bands[0].stats.count, 25);
    assert_eq!((c.value(0, 8, 0), c.value(0, 0, 0), c.value(0, 0, 4)), (Some(204.0), Some(106.0), None));
    s.run("layer.template", &json!({"template": "polygons", "name": "Elsewhere"})).unwrap();
    let far = json!([[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]]);
    s.run("edit.addFeature", &json!({"layer": "Elsewhere", "geometry": {"type": "Polygon", "coordinates": far}})).unwrap();
    let e = s.run("raster.process", &json!({"layer": "dem", "operation": "clip", "polygon": "Elsewhere"})).unwrap_err();
    assert!(e.to_string().contains("no cell"), "{e}");
    assert!(s.run("raster.process", &json!({"layer": "dem", "operation": "clip", "polygon": "west"})).is_err());

    // One stretch shared across tiles, as fixed bounds.
    s.run("raster.stretch", &json!({"layer": "west", "type": "custom", "min": 100, "max": 300})).unwrap();
    s.run("raster.stretch", &json!({"layer": "east", "like": "west"})).unwrap();
    let st = |s: &Session, n: &str| s.map().layer_by_name(n).unwrap().raster.as_ref().unwrap().style.stretch;
    assert_eq!(st(&s, "east"), rustgis_map::Stretch::custom(100.0, 300.0));
    s.run("raster.stretch", &json!({"layer": "west", "type": "minMax"})).unwrap();
    s.run("raster.stretch", &json!({"layer": "east", "like": "west"})).unwrap();
    assert_eq!(st(&s, "east"), rustgis_map::Stretch::custom(100.0, 109.0));
    // A mask on the source narrows its range, and the shared one with it.
    s.run("raster.mask", &json!({"layer": "west", "values": [100]})).unwrap();
    s.run("raster.stretch", &json!({"layer": "east", "like": "west"})).unwrap();
    assert_eq!(st(&s, "east"), rustgis_map::Stretch::custom(101.0, 109.0));
    assert!(s.run("raster.stretch", &json!({"layer": "east", "like": "Countries"})).is_err());

    // A raster opened from an overview says so in its results, not only in a notice.
    let mut ov = tile("big", 360_000.0, 100.0, 100.0, 99);
    ov.overview = Some((20, 20));
    let bytes = geotiff::write(&ov, 0).unwrap();
    let v = crate::cmd::raster::add_raster(&mut s, "big", rustgis_map::RasterLayer::from_bytes(ov, "big.tif", bytes)).unwrap();
    assert_eq!(v["overviewOf"], json!([20, 20]));
    let out = std::env::temp_dir().join(format!("rustgis-overview-{}.tif", std::process::id()));
    std::fs::remove_file(&out).ok();
    let v = s.run("share.exportRaster", &json!({"layer": "big", "path": out.display().to_string()})).unwrap();
    assert_eq!(v["overviewOf"], json!([20, 20]));
    std::fs::remove_file(&out).ok();
    let e = s.run("raster.mosaic", &json!({"inputs": ["big", "east"]})).unwrap_err();
    assert!(e.to_string().contains("overview"), "{e}");
}

/// ModelBuilder graphs: validated, run in flow order, intermediate results dropped, outputs named.
fn model_graphs(s: &mut Session) {
    let graph = json!({
        "nodes": [
            {"id": "i1", "kind": "input", "x": 0, "y": 0, "layer": "Cities"},
            {"id": "t1", "kind": "tool", "x": 240, "y": 0, "tool": "buffer", "params": {"distance": 20}},
            {"id": "t2", "kind": "tool", "x": 480, "y": 0, "tool": "dissolve"},
            {"id": "o1", "kind": "output", "x": 720, "y": 0, "name": "City zones"}
        ],
        "edges": [
            {"from": "i1", "fromPort": "out", "to": "t1", "toPort": "input"},
            {"from": "t1", "fromPort": "out", "to": "t2", "toPort": "input"},
            {"from": "t2", "fromPort": "out", "to": "o1", "toPort": "in"}
        ]
    });
    s.run("model.save", &json!({"name": "Zones", "graph": graph})).unwrap();
    let m = s.project.models.iter().find(|m| m.name == "Zones").unwrap();
    assert_eq!(m.steps.len(), 2, "steps follow the graph");
    assert_eq!(m.steps[1].params["input"], "%1");
    assert_eq!(s.run("model.validate", &json!({"name": "Zones"})).unwrap()["ok"], true);
    let layers = s.map().layers.len();
    let r = s.run("model.run", &json!({"name": "Zones"})).unwrap();
    assert_eq!(r["ran"].as_array().unwrap().len(), 2);
    assert_eq!(r["outputs"], json!(["City zones"]));
    assert!(s.map().layer_by_name("City zones").is_some());
    assert_eq!(s.map().layers.len(), layers + 1, "the buffer, only feeding the dissolve, is removed");
    let report = s.model_run.clone().unwrap();
    assert!(report.ok && report.nodes.iter().all(|n| n.status == "done"));
    // Run again: the output name stays unique.
    let r = s.run("model.run", &json!({"name": "Zones"})).unwrap();
    assert_eq!(r["outputs"], json!(["City zones 2"]));

    // A result kept by an output and fed on to another tool: the tool reads it by its new name,
    // whichever runs first.
    let both = json!({"nodes": [
        {"id": "i1", "kind": "input", "x": 0, "y": 0, "layer": "Cities"},
        {"id": "t1", "kind": "tool", "x": 200, "y": 0, "tool": "buffer", "params": {"distance": 20}},
        {"id": "o1", "kind": "output", "x": 300, "y": 0, "name": "Buffers"},
        {"id": "t2", "kind": "tool", "x": 400, "y": 0, "tool": "dissolve"}],
        "edges": [{"from": "i1", "fromPort": "out", "to": "t1", "toPort": "input"},
                  {"from": "t1", "fromPort": "out", "to": "o1", "toPort": "in"},
                  {"from": "t1", "fromPort": "out", "to": "t2", "toPort": "input"}]});
    let r = s.run("model.run", &json!({"graph": both})).unwrap();
    assert_eq!(r["outputs"][0], "Buffers");
    // Saving over a model from a file keeps it one.
    s.run("model.save", &json!({"name": "From disk", "graph": both, "imported": true})).unwrap();
    assert!(!s.trusted_models.contains("from disk"));
    s.run("model.save", &json!({"name": "From disk", "graph": both, "rename": "Renamed"})).unwrap();
    assert!(!s.trusted_models.contains("renamed") && !s.trusted_models.contains("from disk"));
    s.run("model.remove", &json!({"name": "Renamed"})).unwrap();

    // Problems: a missing layer, an unconnected required input, a loop.
    let mut bad = graph.clone();
    bad["nodes"][0]["layer"] = json!("Nowhere");
    bad["edges"].as_array_mut().unwrap().push(json!({"from": "t2", "fromPort": "out", "to": "t1", "toPort": "input"}));
    let v = s.run("model.validate", &json!({"graph": bad})).unwrap();
    let codes: Vec<&str> = v["problems"].as_array().unwrap().iter().map(|p| p["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"missing-layer") && codes.contains(&"duplicate-input") && codes.contains(&"cycle"), "{codes:?}");
    let e = s.run("model.run", &json!({"graph": bad})).unwrap_err();
    assert!(!e.is_empty());
    assert!(!s.model_run.as_ref().unwrap().ok);
    // A failing tool stops the run, names its node and leaves the project as it was.
    let layers = s.map().layers.len();
    let fails = json!({"nodes": [
        {"id": "i1", "kind": "input", "layer": "Cities"},
        {"id": "t1", "kind": "tool", "tool": "buffer", "params": {"distance": 20}},
        {"id": "t2", "kind": "tool", "tool": "dissolve", "params": {"field": "no_such_field"}}],
        "edges": [{"from": "i1", "fromPort": "out", "to": "t1", "toPort": "input"},
                  {"from": "t1", "fromPort": "out", "to": "t2", "toPort": "input"}]});
    s.run("model.run", &json!({"graph": fails})).unwrap_err();
    let rep = s.model_run.clone().unwrap();
    assert_eq!(rep.nodes.last().unwrap().status, "error");
    assert_eq!(s.map().layers.len(), layers, "a failed run is undone");
    // Two outputs on one result are refused.
    let mut twice = graph.clone();
    twice["nodes"].as_array_mut().unwrap().push(json!({"id": "o2", "kind": "output", "name": "Again"}));
    twice["edges"].as_array_mut().unwrap().push(json!({"from": "t2", "fromPort": "out", "to": "o2", "toPort": "in"}));
    let v = s.run("model.validate", &json!({"graph": twice})).unwrap();
    assert!(v["problems"].as_array().unwrap().iter().any(|p| p["code"] == "shared-output"));
    // An unsaved graph exports as passed.
    let t = s.run("model.export", &json!({"name": "Unsaved", "graph": twice})).unwrap();
    assert!(t["text"].as_str().unwrap().contains("Again"));
    // Hostile graphs: repeated ids, unknown kinds and ports, edges to nowhere, huge positions.
    for junk in [
        json!({"nodes": [{"id": "a", "kind": "tool", "tool": "buffer", "x": 1e308}, {"id": "a", "kind": "weird"}, {"id": "b", "kind": "output"}, {"id": "", "kind": "input"}, 7],
               "edges": [{"from": "a", "fromPort": "zz", "to": "b", "toPort": "nope"}, {"from": "b", "to": "a", "toPort": "input"}, {"from": "a", "to": "a", "toPort": "input"}, {"from": "q", "to": "a", "toPort": "input"}, "x"]}),
        json!({"nodes": "x"}),
        json!({"nodes": [{"id": "t", "kind": "tool", "tool": "buffer", "params": [1, 2]}], "edges": {}}),
    ] {
        let _ = s.run("model.validate", &json!({"graph": junk}));
        let _ = s.run("model.run", &json!({"graph": junk}));
        let _ = s.run("model.export", &json!({"graph": junk}));
        let _ = s.run("model.save", &json!({"name": "Junk", "graph": junk}));
    }
    let _ = s.run("model.remove", &json!({"name": "Junk"}));

    // A graph without positions is laid out along the flow.
    let g = rustgis_engine_graph(&fails);
    assert!(g.nodes[0].x < g.nodes[1].x && g.nodes[1].x < g.nodes[2].x);
    // Models saved as steps still run as steps: `%n` anywhere, and a clear error for one too far.
    s.run(
        "model.new",
        &json!({"name": "Steps", "steps": [
        {"tool": "buffer", "params": {"input": "Cities", "distance": 5}},
        {"tool": "dissolve", "params": {"input": "%3"}}]}),
    )
    .unwrap();
    assert!(s.project.models.iter().any(|m| m.name == "Steps" && m.graph.is_none()));
    let e = s.run("model.run", &json!({"name": "Steps"})).unwrap_err();
    assert!(e.contains("uses %3"), "{e}");
    s.run("model.remove", &json!({"name": "Steps"})).unwrap();
    // Old step models open as graphs.
    let steps = vec![
        crate::project::ModelStep { tool: "buffer".into(), params: json!({"input": "Cities", "distance": 5}) },
        crate::project::ModelStep { tool: "dissolve".into(), params: json!({"input": "%1"}) },
    ];
    let g = crate::model::steps_to_graph(&steps);
    assert_eq!(g.nodes.len(), 3);
    assert_eq!(g.edges.len(), 2);
    assert_eq!(crate::model::graph_to_steps(&g), steps);
    // Export and import as a model file; an imported model may not touch files.
    let text = s.run("model.export", &json!({"name": "Zones"})).unwrap()["text"].as_str().unwrap().to_string();
    assert!(text.contains(crate::model::SCHEMA));
    let m = s.run("model.import", &json!({"text": text})).unwrap();
    let imported = m["model"].as_str().unwrap().to_string();
    assert_ne!(imported, "Zones");
    assert!(s.project.models.iter().any(|m| m.name == imported && m.graph.as_ref().is_some_and(|g| g.nodes.len() == 4)));
    let mut writes = graph.clone();
    writes["nodes"][1]["params"]["output"] = json!("/tmp/zones.shp");
    s.run("model.new", &json!({"name": "Writes", "graph": writes, "imported": true})).unwrap();
    let e = s.run("model.run", &json!({"name": "Writes"})).unwrap_err();
    assert!(e.contains("from a file"), "{e}");
    for n in ["Zones", "Writes", imported.as_str()] {
        s.run("model.remove", &json!({"name": n})).unwrap();
    }
}

fn rustgis_engine_graph(v: &Value) -> crate::project::ModelGraph {
    crate::model::graph_from_json(v).unwrap()
}

/// Project info holds everything an agent needs about the open project in one call: the view's
/// extent, the active layer, each layer's group, source, extent, definition query and labels,
/// and the layouts with their map frames; sources lose their passwords, and the options filter
/// layers by name and leave fields out.
#[test]
fn project_info_describes_the_whole_project() {
    let mut s = world();
    s.view_size = (1000.0, 600.0);
    s.run("layer.definitionQuery", &json!({"layer": "Cities", "where": "pop_max > 1000000"})).unwrap();
    s.run("layout.new", &json!({"name": "Wall map"})).unwrap();
    if let Some(l) = s.map_mut().layers.iter_mut().find(|l| l.name == "Rivers") {
        l.dataset.source = Some("postgresql://mapper:hunter2@db.example.com/gis".into());
    }
    let info = s.run("info.project", &json!({})).unwrap();
    assert_eq!(info["unsaved"], true, "{info}");
    let e = info["extent"].as_array().unwrap();
    assert!(e.len() == 4 && e[0].as_f64() < e[2].as_f64() && e[1].as_f64() < e[3].as_f64(), "{info}");
    assert_eq!(info["activeLayer"]["name"], "Countries", "{info}");
    let layer = |name: &str| info["layers"].as_array().unwrap().iter().find(|l| l["name"] == name).cloned().unwrap();
    assert_eq!(layer("Cities")["definitionQuery"], "pop_max > 1000000");
    assert_eq!(layer("Countries")["labels"], "name");
    assert_eq!(layer("Countries")["extent"][0], -180.0);
    assert!(layer("Countries")["fields"].as_array().is_some_and(|f| !f.is_empty()));
    let source = layer("Rivers")["source"].as_str().unwrap().to_string();
    assert!(source.contains("db.example.com") && !source.contains("hunter2"), "{source}");
    let layouts = info["layouts"].as_array().unwrap();
    assert_eq!(layouts.len(), 1, "{info}");
    assert_eq!(layouts[0]["name"], "Wall map");
    assert_eq!(layouts[0]["mapFrames"][0]["map"], "World", "{info}");

    let some = s.run("info.project", &json!({"nameFilter": "CIT", "includeFields": false})).unwrap();
    let layers = some["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 1, "{some}");
    assert_eq!(layers[0]["name"], "Cities");
    assert!(layers[0].get("fields").is_none(), "{some}");
}

/// A script refuses scripts, notebooks and `ui.*` commands, by the one rule the Script pane marks
/// its lines with (#653).
#[test]
fn scripts_refuse_scripts_notebooks_and_ui_commands() {
    use crate::cmd::workflows::{refused_in_script, script_commands};
    for id in ["script.run", "ui.pane.labeling", "notebook.run"] {
        assert!(refused_in_script(id), "{id}");
        let e = script_commands(&format!("map.zoomFull\n{id} {{}}")).unwrap_err();
        assert!(e.contains("line 2") && e.contains("can't run in a script"), "{e}");
    }
    assert!(!refused_in_script("map.zoomFull") && !refused_in_script("notebook.new"));
}

/// A script can't edit a layer read in place from its file, as a command run on its own can't:
/// a later line loading the layer would otherwise lose the edit without a word.
#[test]
fn scripts_refuse_edits_to_streamed_layers() {
    let path = grid_fgb("script-streamed", 20);
    let mut s = with_streamed(&path);
    let before = s.map().layer_by_name("Streamed").map(|l| l.dataset.features.len());
    let script = "edit.enable {\"on\": true}\nedit.addFeature {\"layer\": \"Streamed\", \"geometry\": {\"type\": \"Point\", \"coordinates\": [1, 2]}}\nlayer.loadAll {\"layer\": \"Streamed\"}";
    let e = s.run("script.run", &json!({"script": script})).unwrap_err();
    assert!(e.starts_with("line 2:") && e.contains("read in place"), "{e}");
    let l = s.map().layer_by_name("Streamed").unwrap();
    assert!(l.is_streamed(), "the failed script kept nothing");
    assert_eq!(Some(l.dataset.features.len()), before);
    let _ = std::fs::remove_file(path);
}

/// A map on Mars (#223): the body switcher puts a Mars basemap under it, its measurements and
/// scale are Mars', and the project keeps the body; back on the Earth the Mars basemap goes.
#[test]
fn celestial_bodies_set_the_basemap_and_the_measurements() {
    let mut s = Session::new(Project::default());
    let earth = |s: &mut Session| {
        s.run("raster.measure", &json!({"kind": "distance", "points": [[0, 0], [90, 0]]})).unwrap()["metres"].as_f64().unwrap()
    };
    let on_earth = earth(&mut s);
    let earth_scale = s.map().scale();
    s.run("map.addTiles", &json!({"url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "id": "osm"})).unwrap();
    let v = s.run("map.body", &json!({"name": "Mars"})).unwrap();
    assert_eq!(v["body"], "mars");
    assert_eq!(s.map().body().id, "mars");
    // Mars' basemap replaces the Earth's.
    let tiles = s.run("map.tiles", &Value::Null).unwrap();
    let ids: Vec<&str> = tiles.as_array().unwrap().iter().filter_map(|t| t["id"].as_str()).collect();
    assert_eq!(ids, ["planet-mars-viking-mdim21"]);
    assert!(s.map().tiles.first().is_some_and(|t| t.tms));
    // The built-in basemap's Earth land isn't drawn on Mars: a plain background under its tiles.
    let (background, land, ..) = s.map().palette();
    assert_eq!(background, land);
    // A quarter of Mars' equator, and the scale of a smaller world.
    let on_mars = earth(&mut s);
    let k = rustgis_geom::Body::get("mars").ratio();
    assert!((on_mars / on_earth - k).abs() < 1e-9 && (on_mars - std::f64::consts::FRAC_PI_2 * 3_389_526.7).abs() < 1.0, "{on_mars}");
    assert!((s.map().scale() / earth_scale - k).abs() < 1e-9);
    let area = s.run("raster.measure", &json!({"kind": "area", "points": [[0, 0], [1, 0], [1, 1]]})).unwrap();
    // Leaving Mars without a new basemap still takes Mars' off.
    s.run("map.body", &json!({"name": "earth", "basemap": false})).unwrap();
    assert!(s.map().tiles.is_empty());
    let earth_area = s.run("raster.measure", &json!({"kind": "area", "points": [[0, 0], [1, 0], [1, 1]]})).unwrap();
    let ratio = area["squareMetres"].as_f64().unwrap() / earth_area["squareMetres"].as_f64().unwrap();
    assert!((ratio - k * k).abs() < 1e-9, "{ratio}");
    // Another of Mars' basemaps, named alone; saved and opened again, the map is still on Mars.
    s.run("map.body", &json!({"basemap": "mars-hillshade"})).unwrap();
    assert_eq!(s.map().body().id, "mars");
    assert_eq!(s.map().tiles.len(), 1);
    let text = serde_json::to_string(&s.project).unwrap();
    let p = Project::from_json(&text).unwrap();
    assert_eq!(p.maps.first().map(|m| m.body().id), Some("mars"));
    s.run("map.addTiles", &json!({"url": rustgis_map::planets::EARTH_BASEMAP.url, "id": "overlay", "overlay": true, "opacity": 0.5}))
        .unwrap();
    // Back on the Earth the planetary basemap becomes USGS Imagery, keeping overlays;
    // an Earth map saves no body.
    let earth_view = s.run("map.body", &json!({"name": "earth"})).unwrap();
    assert_eq!(earth_view["basemap"], "usgs-imagery");
    assert!(s.map().body().is_earth());
    let tiles = &s.map().tiles;
    assert_eq!(tiles.len(), 2);
    assert!(tiles.iter().any(|t| t.id == "overlay" && t.overlay && t.opacity == 0.5));
    let imagery = tiles.iter().find(|t| !t.overlay).unwrap();
    assert_eq!(imagery.id, rustgis_map::planets::EARTH_BASEMAP.id);
    assert_eq!(imagery.url, rustgis_map::planets::EARTH_BASEMAP.url);
    assert!(imagery.visible && !imagery.overlay && !imagery.tms);
    assert_eq!(imagery.max_zoom, 16);
    assert!(!serde_json::to_string(&s.project).unwrap().contains("\"body\""));
    // Undo goes back to Mars.
    assert!(s.undo());
    assert_eq!(s.map().body().id, "mars");
    assert!(s.map().tiles.iter().any(|t| t.id == "planet-mars-hillshade"));
    assert!(s.map().tiles.iter().all(|t| t.id != "usgs-imagery"));
    // Bad names and mismatches are refused, and nothing changes.
    assert!(s.run("map.body", &json!({"name": "vulcan"})).is_err());
    assert!(s.run("map.body", &json!({"name": "moon", "basemap": "mars-hillshade"})).is_err());
    assert!(s.run("map.body", &json!({})).is_err());
    assert_eq!(s.map().body().id, "mars");
    let bodies = s.run("map.bodies", &Value::Null).unwrap();
    assert_eq!(bodies["body"], "mars");
    assert_eq!(bodies["bodies"].as_array().map(Vec::len), Some(rustgis_geom::BODIES.len()));
    // A basemap the map has no room for (overlays fill it) fails, and the map stays as it was:
    // on its body, with its tile layers.
    let mut full = Session::new(Project::default());
    for i in 0..rustgis_map::tiles::MAX_TILE_LAYERS {
        full.run("map.addTiles", &json!({"url": format!("https://o{i}.example.com/{{z}}/{{x}}/{{y}}.png"), "overlay": true})).unwrap();
    }
    assert!(full.run("map.body", &json!({"name": "mars"})).is_err());
    assert!(full.map().body().is_earth());
    assert_eq!(full.map().tiles.len(), rustgis_map::tiles::MAX_TILE_LAYERS);
    // The Earth's new default is also atomic when there is no room for its imagery.
    full.run("map.body", &json!({"name": "moon", "basemap": false})).unwrap();
    assert!(full.run("map.body", &json!({"name": "earth"})).is_err());
    assert_eq!(full.map().body().id, "moon");
    assert_eq!(full.map().tiles.len(), rustgis_map::tiles::MAX_TILE_LAYERS);
    // A project naming an unknown body is on the Earth.
    let odd = text.replace("\"body\":\"mars\"", "\"body\":\"vulcan\"");
    assert_ne!(odd, text);
    let p = Project::from_json(&odd).unwrap();
    assert!(p.maps.first().is_some_and(|m| m.body().is_earth()));
    // Saved again, it keeps the body's id (a newer version's, say).
    assert!(serde_json::to_string(&p).unwrap().contains("\"body\":\"vulcan\""));
}

/// Text elements take italic, alignment and letter spacing (#213, point c): set when added or
/// later, kept through a save, plain by default and not written until changed; a bad value is an
/// error that changes nothing.
#[test]
fn text_elements_take_italic_alignment_and_spacing() {
    use rustgis_map::layout::{ElementKind, MAX_LETTER_SPACING, TextAlign};
    let mut s = Session::new(sample::world_project());
    s.run("layout.new", &json!({"name": "Type"})).unwrap();
    let style = |s: &Session, id: u32| match s.project.layouts[0].element(id).map(|e| &e.kind) {
        Some(ElementKind::Text { italic, align, letter_spacing, .. }) => (*italic, *align, *letter_spacing),
        _ => panic!("a text element"),
    };
    let plain = s.run("layout.addElement", &json!({"kind": "text", "text": "Brasil"})).unwrap()["element"].as_u64().unwrap() as u32;
    assert_eq!(style(&s, plain), (false, TextAlign::Left, 0.0));
    let saved = serde_json::to_string(&s.project.layouts[0]).unwrap();
    assert!(!saved.contains("italic") && !saved.contains("letter_spacing") && !saved.contains("align"), "defaults aren't written");
    let ocean = s
        .run(
            "layout.addElement",
            &json!({"kind": "text", "text": "OCEANO ATLÂNTICO", "italic": true, "align": "center", "letterSpacing": 4}),
        )
        .unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    assert_eq!(style(&s, ocean), (true, TextAlign::Center, 4.0));
    let dynamic =
        s.run("layout.addElement", &json!({"kind": "dynamicText", "align": "Right"})).unwrap()["element"].as_u64().unwrap() as u32;
    assert_eq!(style(&s, dynamic).1, TextAlign::Right);
    // One value changed keeps the others; spacing is clamped.
    s.run("layout.update", &json!({"element": ocean, "letterSpacing": 999})).unwrap();
    assert_eq!(style(&s, ocean), (true, TextAlign::Center, MAX_LETTER_SPACING));
    s.run("layout.update", &json!({"element": ocean, "italic": false, "align": "centre"})).unwrap();
    assert_eq!(style(&s, ocean), (false, TextAlign::Center, MAX_LETTER_SPACING));
    let back: rustgis_map::layout::Layout = serde_json::from_str(&serde_json::to_string(&s.project.layouts[0]).unwrap()).unwrap();
    assert!(back.element(ocean).is_some_and(
        |e| matches!(&e.kind, ElementKind::Text { align: TextAlign::Center, letter_spacing, .. } if *letter_spacing == MAX_LETTER_SPACING)
    ));
    // A bad value names itself and changes nothing.
    for (bad, field) in [
        (json!({"align": "justify"}), "unknown alignment"),
        (json!({"align": 3}), "align"),
        (json!({"italic": "very"}), "italic"),
        (json!({"letterSpacing": "wide"}), "letterSpacing"),
    ] {
        let mut p = bad.clone();
        p["element"] = json!(ocean);
        let err = s.run("layout.update", &p).unwrap_err();
        assert!(err.starts_with(field), "{err}");
        let mut add = bad;
        add["kind"] = json!("text");
        assert!(s.run("layout.addElement", &add).is_err());
    }
    assert_eq!(style(&s, ocean), (false, TextAlign::Center, MAX_LETTER_SPACING));
}

/// Text elements take a font family (#517): sans by default and not written then, serif when
/// added or later, kept through a save, sans again on request; a bad value is an error that
/// changes nothing. Where no serif is installed the text is drawn in the sans, and the command
/// says so.
#[test]
fn text_elements_take_a_font_family() {
    use rustgis_map::layout::{ElementKind, FontFamily};
    let mut s = Session::new(sample::world_project());
    s.run("layout.new", &json!({"name": "Type"})).unwrap();
    let font = |s: &Session, id: u32| match s.project.layouts[0].element(id).map(|e| &e.kind) {
        Some(ElementKind::Text { font, .. }) => *font,
        _ => panic!("a text element"),
    };
    let plain = s.run("layout.addElement", &json!({"kind": "text", "text": "Brasil"})).unwrap()["element"].as_u64().unwrap() as u32;
    assert_eq!(font(&s, plain), FontFamily::Sans);
    assert!(!serde_json::to_string(&s.project.layouts[0]).unwrap().contains("font"), "the sans isn't written");
    let r = s.run("layout.addElement", &json!({"kind": "dynamicText", "font": "Serif"})).unwrap();
    let title = r["element"].as_u64().unwrap() as u32;
    assert_eq!(font(&s, title), FontFamily::Serif);
    // Said only where the text can't be drawn as asked.
    let installed = rustgis_render::fonts::serif_available();
    assert_eq!(r.get("warning").is_some(), !installed, "{r}");
    let r = s.run("layout.update", &json!({"element": plain, "font": "serif"})).unwrap();
    assert_eq!(font(&s, plain), FontFamily::Serif);
    assert_eq!(r.get("warning").is_some(), !installed, "{r}");
    // Another change leaves the family, and says nothing of it.
    let r = s.run("layout.update", &json!({"element": plain, "italic": true})).unwrap();
    assert_eq!((font(&s, plain), r.get("warning")), (FontFamily::Serif, None));
    let back: rustgis_map::layout::Layout = serde_json::from_str(&serde_json::to_string(&s.project.layouts[0]).unwrap()).unwrap();
    assert!(back.element(plain).is_some_and(|e| matches!(&e.kind, ElementKind::Text { font: FontFamily::Serif, .. })));
    s.run("layout.update", &json!({"element": plain, "font": "sans-serif"})).unwrap();
    assert_eq!(font(&s, plain), FontFamily::Sans);
    // Saved as the other layout enums are; the command's lowercase names read too (a layout
    // written by hand).
    assert!(serde_json::to_string(&s.project.layouts[0]).unwrap().contains(r#""font":"Serif""#));
    for (name, f) in [("\"serif\"", FontFamily::Serif), ("\"Serif\"", FontFamily::Serif), ("\"sans\"", FontFamily::Sans)] {
        assert_eq!(serde_json::from_str::<FontFamily>(name).unwrap(), f);
    }
    for bad in [json!("comic"), json!(2)] {
        let err = s.run("layout.update", &json!({"element": title, "font": bad})).unwrap_err();
        assert!(err.contains("font"), "{err}");
        assert!(s.run("layout.addElement", &json!({"kind": "text", "font": bad})).is_err());
    }
    assert_eq!(font(&s, title), FontFamily::Serif);
    // Only a text takes a family: another element refuses it rather than ignore it.
    let arrow = s.run("layout.addElement", &json!({"kind": "northArrow"})).unwrap()["element"].clone();
    let err = s.run("layout.update", &json!({"element": arrow, "font": "serif"})).unwrap_err();
    assert!(err.contains("only text elements"), "{err}");
    assert!(s.run("layout.addElement", &json!({"kind": "northArrow", "font": "serif"})).unwrap_err().contains("only text elements"));
    // The page draws either way.
    assert!(s.run("layout.update", &json!({"element": title, "text": "Mapa do Brasil", "bold": true, "italic": true})).is_ok());
    assert!(!crate::cmd::layout::layout_bytes(&mut s, 0, false, 72.0, false).unwrap().is_empty());
}

/// Each undo step remembers who made it, through undo and redo: the user (no actor) or the
/// agent the MCP relay named.
#[test]
fn undo_steps_remember_who_made_them() {
    let mut s = world();
    assert_eq!(s.undo_actor(), None);
    s.run("layer.rename", &json!({"layer": "Rivers", "name": "Streams"})).unwrap();
    assert_eq!(s.undo_actor(), Some(None));
    s.actor = Some("agent 1".into());
    s.run("layer.rename", &json!({"layer": "Lakes", "name": "Ponds"})).unwrap();
    s.actor = None;
    assert_eq!(s.undo_actor(), Some(Some("agent 1")));
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!((s.undo_actor(), s.redo_actor()), (Some(None), Some(Some("agent 1"))));
    s.run("edit.redo", &json!({})).unwrap();
    assert_eq!((s.undo_actor(), s.redo_actor()), (Some(Some("agent 1")), None));
}

/// A legend can leave layers out though the map draws them: set when added or later, checked
/// against its frame's map, kept through a save (not written when empty), and following a
/// renamed layer.
#[test]
fn legends_leave_layers_out() {
    use rustgis_map::layout::ElementKind;
    let mut s = Session::new(sample::world_project());
    s.run("layout.new", &json!({"name": "Legends"})).unwrap();
    let left_out = |s: &Session, id: u32| match s.project.layouts[0].element(id).map(|e| &e.kind) {
        Some(ElementKind::Legend { exclude, .. }) => exclude.clone(),
        _ => panic!("a legend"),
    };
    let plain = s.run("layout.addElement", &json!({"kind": "legend"})).unwrap()["element"].as_u64().unwrap() as u32;
    assert!(left_out(&s, plain).is_empty());
    let saved = serde_json::to_string(&s.project.layouts[0]).unwrap();
    assert!(!saved.contains("exclude"), "an empty list isn't written");
    // Names as the map spells them, once each.
    let lakes = s.run("layout.addElement", &json!({"kind": "legend", "exclude": ["lakes", "LAKES", "Rivers"]})).unwrap()["element"]
        .as_u64()
        .unwrap() as u32;
    assert_eq!(left_out(&s, lakes), ["Lakes", "Rivers"]);
    s.run("layout.update", &json!({"element": lakes, "exclude": ["Rivers"]})).unwrap();
    assert_eq!(left_out(&s, lakes), ["Rivers"]);
    // A layer the map hasn't, or not a list of names, is an error that changes nothing; so is
    // `exclude` on another kind of element.
    for bad in [json!(["Atlantis"]), json!("Rivers"), json!([3]), json!([null]), json!([""]), json!([{"x": 1}]), json!(["Rivers", 7])] {
        assert!(s.run("layout.update", &json!({"element": lakes, "exclude": bad})).is_err(), "{bad}");
        assert!(s.run("layout.addElement", &json!({"kind": "legend", "exclude": bad})).is_err(), "{bad}");
    }
    assert_eq!(left_out(&s, lakes), ["Rivers"]);
    let title = s.project.layouts[0].elements.iter().find(|e| e.name == "Title").unwrap().id;
    assert!(s.run("layout.update", &json!({"element": title, "exclude": ["Rivers"]})).unwrap_err().contains("only legends"));
    assert!(s.run("layout.addElement", &json!({"kind": "northArrow", "exclude": ["Rivers"]})).unwrap_err().contains("only legends"));
    // Renaming the layer keeps it left out.
    s.run("layer.rename", &json!({"layer": "Rivers", "name": "Rios"})).unwrap();
    assert_eq!(left_out(&s, lakes), ["Rios"]);
    let back: rustgis_map::layout::Layout = serde_json::from_str(&serde_json::to_string(&s.project.layouts[0]).unwrap()).unwrap();
    assert!(back.element(lakes).is_some_and(|e| matches!(&e.kind, ElementKind::Legend { exclude, .. } if exclude == &["Rios"])));
    // Names may repeat. Renaming Lakes onto a name already left out keeps it once; with two
    // layers named Rios, removing one keeps the name left out for the other.
    s.run("layout.update", &json!({"element": lakes, "exclude": ["Rios", "Lakes"]})).unwrap();
    s.run("layer.rename", &json!({"layer": "Lakes", "name": "Rios"})).unwrap();
    assert_eq!(left_out(&s, lakes), ["Rios"], "once each");
    assert_eq!(s.map().layers.iter().filter(|l| l.name == "Rios").count(), 2);
    let first = s.map().layers.iter().find(|l| l.name == "Rios").unwrap().id;
    s.run("layer.remove", &json!({"layer": first})).unwrap();
    assert_eq!(left_out(&s, lakes), ["Rios"], "the other Rios is still left out");
    // Deleting the layer drops it from the list, so a later layer of that name is listed.
    s.run("layer.remove", &json!({"layer": "Rios"})).unwrap();
    assert!(left_out(&s, lakes).is_empty());
    // An empty list lists every layer again.
    s.run("layout.update", &json!({"element": lakes, "exclude": ["Countries"]})).unwrap();
    s.run("layout.update", &json!({"element": lakes, "exclude": []})).unwrap();
    assert!(left_out(&s, lakes).is_empty());
    // Removing a group with its layers drops them too.
    s.run("layout.update", &json!({"element": lakes, "exclude": ["Cities", "Countries"]})).unwrap();
    s.run("group.add", &json!({"name": "Places", "layers": ["Cities"]})).unwrap();
    s.run("group.remove", &json!({"group": "Places"})).unwrap();
    assert_eq!(left_out(&s, lakes), ["Countries"]);
    // A list past the cap is refused rather than cut short.
    let many = vec!["Countries"; rustgis_map::layout::MAX_LEGEND_EXCLUDED + 1];
    assert!(s.run("layout.update", &json!({"element": lakes, "exclude": many})).unwrap_err().contains("at most"));
    assert_eq!(left_out(&s, lakes), ["Countries"]);
    // A frame naming its map in another case (as an imported layout may) still takes a list and
    // follows renames, as the renderer finds the map.
    let lower = s.map().name.to_lowercase();
    assert_ne!(lower, s.map().name);
    for e in &mut s.project.layouts[0].elements {
        if let ElementKind::MapFrame { map, .. } = &mut e.kind {
            map.clone_from(&lower);
        }
    }
    s.run("layout.update", &json!({"element": lakes, "exclude": ["countries"]})).unwrap();
    s.run("layer.rename", &json!({"layer": "Countries", "name": "Nations"})).unwrap();
    assert_eq!(left_out(&s, lakes), ["Nations"]);
    // Renamed onto the name of a layer the legend lists, a layer left out is listed with it.
    s.run("layer.addSample", &json!({"name": "lakes"})).unwrap();
    s.run("layout.update", &json!({"element": lakes, "exclude": ["Lakes"]})).unwrap();
    s.run("layer.rename", &json!({"layer": "Nations", "name": "Countries"})).unwrap();
    s.run("layer.rename", &json!({"layer": "Lakes", "name": "Countries"})).unwrap();
    assert!(left_out(&s, lakes).is_empty(), "the other Countries isn't taken out");
}

/// A point layer of events, one a month through 2015 (two in March), with their dates as text,
/// epoch milliseconds and years.
fn timed_events() -> Session {
    isolate_plugins();
    let mut feats = Vec::new();
    let dates = ["2015-01-10", "2015-02-03", "2015-03-01", "2015-03-30", "2015-04-15", "2015-06-20", "2015-09-09", "2015-12-25"];
    for (i, d) in dates.iter().enumerate() {
        let ms = rustgis_data::time::parse_text(d).unwrap();
        feats.push(json!({"type": "Feature", "geometry": {"type": "Point", "coordinates": [i as f64, 0.0]},
            "properties": {"name": format!("e{i}"), "when": d, "ms": ms, "year": 2010 + i}}));
    }
    let text = json!({"type": "FeatureCollection", "features": feats}).to_string();
    let (ds, _) = rustgis_data::io::read_geojson(&text, "Events").unwrap();
    let mut s = Session::new(Project::default());
    let id = s.map_mut().add_dataset(ds);
    s.active_layer = Some(id);
    s
}

fn shown(s: &Session) -> Vec<String> {
    let l = s.map().layer_by_name("Events").unwrap();
    let fi = l.dataset.field_index("name").unwrap();
    l.drawn_rows(s.scale()).iter().enumerate().filter(|(_, k)| **k).map(|(r, _)| l.dataset.value(r, fi).to_string()).collect()
}

#[test]
fn time_slider_shows_the_features_of_its_window() {
    let mut s = timed_events();
    // The date fields, best first: text dates and epoch milliseconds all read; years too.
    let fields = s.run("time.fields", &Value::Null).unwrap();
    let names: Vec<&str> = fields["fields"].as_array().unwrap().iter().map(|f| f["field"].as_str().unwrap()).collect();
    assert!(names.contains(&"when") && names.contains(&"ms") && names.contains(&"year"), "{names:?}");
    assert!(!names.contains(&"name"));
    // Bound by its text dates: a slider by month over the layer's year, at January.
    let r = s.run("time.bind", &json!({"field": "when"})).unwrap();
    assert_eq!((r["unit"].as_str(), r["steps"].as_u64(), r["date"].as_str()), (Some("month"), Some(12), Some("2015-01")), "{r}");
    assert_eq!(shown(&s), ["e0"]);
    s.run("time.goTo", &json!({"date": "2015-03-15"})).unwrap();
    assert_eq!(shown(&s), ["e2", "e3"]);
    // Stepping: May is empty, wrapping goes back to January.
    s.run("time.step", &json!({"by": 2})).unwrap();
    assert!(shown(&s).is_empty());
    s.run("time.step", &json!({"by": 8, "wrap": true})).unwrap();
    assert_eq!(s.run("time.info", &Value::Null).unwrap()["date"], "2015-01");
    // Wider and cumulative windows.
    s.run("time.set", &json!({"date": "2015-04", "show": "wide"})).unwrap();
    assert_eq!(shown(&s), ["e2", "e3", "e4"]);
    s.run("time.set", &json!({"show": "cumulative"})).unwrap();
    assert_eq!(shown(&s), ["e0", "e1", "e2", "e3", "e4"]);
    // Epoch milliseconds read the same, and the layer's legend follows the window.
    s.run("time.set", &json!({"show": "step", "date": "2015-12"})).unwrap();
    s.run("time.bind", &json!({"field": "ms"})).unwrap();
    assert_eq!(shown(&s), ["e7"]);
    // A quarter a step.
    s.run("time.set", &json!({"date": "2015-01", "interval": 3})).unwrap();
    assert_eq!(s.run("time.info", &Value::Null).unwrap()["steps"], 4);
    assert_eq!(shown(&s), ["e0", "e1", "e2", "e3"]);
    // Saved and opened again, the slider and the binding hold.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::from_json(&text).unwrap());
    assert_eq!(shown(&t), ["e0", "e1", "e2", "e3"]);
    t.run("time.step", &Value::Null).unwrap();
    assert_eq!(shown(&t), ["e4", "e5"]);
    // Bound by year numbers (2010–2017), a step a year.
    t.run("time.set", &json!({"off": true})).unwrap();
    assert_eq!(shown(&t).len(), 8);
    t.run("time.bind", &json!({"layer": "Events", "field": "year"})).unwrap();
    assert_eq!(t.run("time.info", &Value::Null).unwrap()["unit"], "year");
    t.run("time.goTo", &json!({"date": 2012})).unwrap();
    assert_eq!(shown(&t), ["e2"]);
    // Unbound, everything shows again; undo binds it again.
    t.run("time.unbind", &json!({"layer": "Events"})).unwrap();
    assert_eq!(shown(&t).len(), 8);
    t.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(shown(&t), ["e2"]);
    // Irregular dates: steps only at those, each window up to the next.
    t.run("time.bind", &json!({"layer": "Events", "field": "when"})).unwrap();
    t.run("time.set", &json!({"dates": ["2015-01-01", "2015-03-01", "2015-09-01"], "date": "2015-03-01"})).unwrap();
    assert_eq!(t.run("time.info", &Value::Null).unwrap()["steps"], 3);
    assert_eq!(shown(&t), ["e2", "e3", "e4", "e5"]);
    for bad in [json!({"layer": "Events", "field": "nope"}), json!({"layer": "Events", "field": "name"})] {
        assert!(t.run("time.bind", &bad).is_err(), "{bad}");
    }
    for bad in [json!({"unit": "fortnight"}), json!({"start": "soon"}), json!({"show": "all"}), json!({"dates": "2015"})] {
        assert!(t.run("time.set", &bad).is_err(), "{bad}");
    }
}

#[test]
fn time_slider_survives_saving_cumulative_edits_and_bad_parameters() {
    let mut s = timed_events();
    // Text and epoch dates come before whole numbers that may only look like years.
    let fields = s.run("time.fields", &Value::Null).unwrap();
    assert_eq!(fields["fields"].as_array().unwrap().last().unwrap()["field"], "year");
    // A bad parameter leaves nothing behind: no slider, no binding.
    assert!(s.run("time.bind", &json!({"field": "when", "unit": "fortnight"})).is_err());
    assert!(s.map().time.is_none() && s.map().layers[0].time.is_none());
    // Settings without a span don't turn an off slider on.
    assert!(s.run("time.set", &json!({"loop": false})).is_err());
    assert!(s.map().time.is_none());
    s.run("time.bind", &json!({"field": "when"})).unwrap();
    s.run("time.set", &json!({"date": "2015-03", "show": "cumulative"})).unwrap();
    assert_eq!(shown(&s), ["e0", "e1", "e2", "e3"]);
    // Saved with its unbounded window, and opened again.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::from_json(&text).unwrap());
    assert_eq!(shown(&t), ["e0", "e1", "e2", "e3"]);
    // An edited date moves its feature in or out of the window at once.
    t.active_layer = t.map().layer_by_name("Events").map(|l| l.id);
    t.run("edit.setValue", &json!({"row": 7, "field": "when", "value": "2015-02-14"})).unwrap();
    assert_eq!(shown(&t), ["e0", "e1", "e2", "e3", "e7"]);
    // Undo takes the edit back, not the slider's moves since.
    t.run("time.goTo", &json!({"date": "2015-04"})).unwrap();
    t.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(t.run("time.info", &Value::Null).unwrap()["date"], "2015-04");
    assert_eq!(shown(&t), ["e0", "e1", "e2", "e3", "e4"]);
    t.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(t.run("time.info", &Value::Null).unwrap()["date"], "2015-04");
    t.run("time.goTo", &json!({"date": "2015-03"})).unwrap();
    // Whole numbers that look like years (heights here) aren't taken for dates unasked; a layer
    // with real dates is.
    assert!(rustgis_map::time::has_dates(&t.map().layers[0].dataset));
    let text = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":null,"properties":{"elev":1500}},{"type":"Feature","geometry":null,"properties":{"elev":2210}}]}"#;
    let (heights, _) = rustgis_data::io::read_geojson(text, "Peaks").unwrap();
    assert!(!rustgis_map::time::has_dates(&heights));
    assert_eq!(rustgis_map::time::time_fields(&heights).len(), 1, "still offered when asked");
    // Pasted into a map without a slider, the copy shows every feature (no window comes along).
    t.run("layer.copy", &json!({"layer": "Events"})).unwrap();
    t.run("map.new", &json!({"name": "Plain"})).unwrap();
    t.run("layer.paste", &Value::Null).unwrap();
    assert!(t.map().time.is_none());
    assert_eq!(shown(&t).len(), 8);
    t.run("map.activate", &json!({"name": "Map"})).unwrap();
    // Features changed outside a command (as a streamed layer reads another view), with as many
    // rows: the window is worked out again for them, not the old rows' mask reused.
    {
        let l = t.map_mut().layers.iter_mut().find(|l| l.name == "Events").unwrap();
        let fi = l.dataset.field_index("when").unwrap();
        l.dataset.features[0].attrs[fi] = rustgis_data::Value::Text("2015-12-01".into());
    }
    assert_eq!(shown(&t), ["e1", "e2", "e3", "e7"]);
    // A field that is gone filters nothing.
    t.run("edit.deleteField", &json!({"field": "when"})).unwrap();
    assert_eq!(shown(&t).len(), 8);
}

#[test]
fn time_slider_fills_dated_tile_addresses_and_exports_its_steps() {
    let mut s = timed_events();
    let r = s
        .run(
            "map.addTiles",
            &json!({"url": "https://example.com/{date:YYYY-MM-DD}/{z}/{x}/{y}.png", "start": "2023-08-01", "end": "2023-08-31"}),
        )
        .unwrap();
    assert_eq!(r["url"], "https://example.com/2023-08-01/{z}/{x}/{y}.png");
    let i = s.run("time.info", &Value::Null).unwrap();
    assert_eq!((i["unit"].as_str(), i["steps"].as_u64()), (Some("day"), Some(31)), "{i}");
    // Only tile kinds that fill dates take a dated address.
    let e = s.run("map.addTiles", &json!({"url": "https://example.com/{YYYY}.pmtiles", "start": 2000})).unwrap_err();
    assert!(e.contains("dated addresses"), "{e}");
    // A COG's look (its bands, its range) stays when the date changes.
    let mut look = timed_events();
    look.run("map.addTiles", &json!({"url": "https://example.com/{YYYY}.tif", "kind": "cog", "start": 2000, "end": 2002})).unwrap();
    look.run("map.setTiles", &json!({"index": 0, "bands": [3, 2, 1], "range": [0, 3000]})).unwrap();
    let shown_look = look.map().tiles[0].url.split_once('#').map(|(_, l)| l.to_string()).unwrap_or_default();
    assert!(!shown_look.is_empty(), "{}", look.map().tiles[0].url);
    look.run("time.goTo", &json!({"date": 2002})).unwrap();
    let url = look.map().tiles[0].url.clone();
    assert!(url.starts_with("https://example.com/2002.tif#") && url.ends_with(&shown_look), "{url}");
    // A scheme in capitals is still a web address that follows the slider.
    let mut caps = timed_events();
    caps.run("map.addTiles", &json!({"url": "HTTPS://example.com/{date:YYYY}/{z}/{x}/{y}.png", "start": 2000, "end": 2002})).unwrap();
    caps.run("time.goTo", &json!({"date": 2002})).unwrap();
    assert!(caps.map().tiles[0].url.contains("/2002/"), "{}", caps.map().tiles[0].url);
    // A dated address with the slider off needs its span.
    let mut off = timed_events();
    assert!(off.run("map.addTiles", &json!({"url": "https://example.com/{date:YYYY}/{z}/{x}/{y}.png"})).is_err());
    assert!(off.map().tiles.is_empty() && off.map().time.is_none());
    // Dated addresses only for GeoTIFFs on the web, refused before anything is fetched.
    for url in ["https://example.com/{YYYY}.parquet", "s3://bucket/{YYYY}.tif"] {
        let e = off.run("layer.addUrl", &json!({"url": url, "start": 2000})).unwrap_err();
        assert!(e.contains("dated addresses"), "{e}");
    }
    // A later check failing (here the blend) takes back the slider the span turned on: the
    // session restores the project when a command fails.
    assert!(
        off.run("map.addTiles", &json!({"url": "https://example.com/{YYYY}/{z}/{x}/{y}.png", "start": 2000, "blend": "nope"})).is_err()
    );
    assert!(off.map().tiles.is_empty() && off.map().time.is_none());
    // Three days of daily imagery step by the day the address names, not by the hour.
    let mut short = timed_events();
    short
        .run(
            "map.addTiles",
            &json!({"url": "https://example.com/{date:YYYY-MM-DD}/{z}/{x}/{y}.png", "start": "2023-08-28", "end": "2023-08-30"}),
        )
        .unwrap();
    let i = short.run("time.info", &Value::Null).unwrap();
    assert_eq!((i["unit"].as_str(), i["steps"].as_u64()), (Some("day"), Some(3)), "{i}");
    short.run("time.goTo", &json!({"step": 99})).unwrap();
    assert_eq!(short.map().tiles[0].url, "https://example.com/2023-08-30/{z}/{x}/{y}.png");
    short.run("time.goTo", &json!({"step": 1})).unwrap();
    assert_eq!(short.map().tiles[0].url, "https://example.com/2023-08-29/{z}/{x}/{y}.png");
    s.run("time.goTo", &json!({"date": "2023-08-30"})).unwrap();
    assert_eq!(s.map().tiles[0].url, "https://example.com/2023-08-30/{z}/{x}/{y}.png");
    // Kept in the project as its template, filled again when opened.
    let text = serde_json::to_string(&s.project).unwrap();
    let t = Project::from_json(&text).unwrap();
    assert_eq!(t.maps[0].tiles[0].url, "https://example.com/2023-08-30/{z}/{x}/{y}.png");
    // A template from a file that isn't a web address is dropped, its layer left as it was.
    let mut p: Value = serde_json::from_str(&text).unwrap();
    p["maps"][0]["tiles"][0]["timeUrl"] = json!("file:///etc/{date:YYYY}");
    let t = Project::from_json(&p.to_string()).unwrap();
    assert_eq!(t.maps[0].tiles[0].time_url, None);
    // Exported: a frame a step, the slider back where it was.
    s.run("time.set", &json!({"start": "2015-01-01", "end": "2015-12-31", "unit": "month"})).unwrap();
    s.run("map.removeTiles", &Value::Null).unwrap();
    s.run("time.bind", &json!({"field": "when"})).unwrap();
    s.run("time.goTo", &json!({"date": "2015-06"})).unwrap();
    let dir = std::env::temp_dir().join(format!("rustgis-time-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gif = dir.join("events.gif");
    let r = s
        .run("time.export", &json!({"path": gif.display().to_string(), "width": 96, "height": 64, "from": "2015-02", "to": "2015-05"}))
        .unwrap();
    assert_eq!(r["frames"], 4);
    let bytes = std::fs::read(&gif).unwrap();
    assert!(bytes.starts_with(b"GIF89a"));
    let zip = dir.join("events.zip");
    assert_eq!(s.run("time.export", &json!({"path": zip.display().to_string(), "width": 96, "height": 64})).unwrap()["frames"], 12);
    assert_eq!(s.run("time.info", &Value::Null).unwrap()["date"], "2015-06");
    assert!(s.run("time.export", &json!({"path": zip.display().to_string(), "from": "2016"})).is_err());
    // Only GIFs and zips, so a file is what its name says.
    assert!(s.run("time.export", &json!({"path": dir.join("events.png").display().to_string()})).is_err());
    // A huge step number is the last step.
    s.run("time.goTo", &json!({"step": u64::MAX})).unwrap();
    assert_eq!(s.run("time.info", &Value::Null).unwrap()["date"], "2015-12");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn zoom_level_matches_the_map_link() {
    let mut s = Session::new(Project::default());
    s.run("map.goToLink", &json!({"url": "#7.5/48/10"})).unwrap();
    let z = s.zoom().unwrap();
    assert!((z - 7.5).abs() < 0.01, "{z}");
}

/// A 2×2 grey TIFF with no GeoTIFF tags, written by hand (little-endian, one strip).
fn tagless_tiff() -> Vec<u8> {
    // (tag, type 3 = short / 4 = long, value)
    let entries: [(u16, u16, u32); 8] =
        [(256, 3, 2), (257, 3, 2), (258, 3, 8), (262, 3, 1), (273, 4, 110), (277, 3, 1), (278, 3, 2), (279, 4, 4)];
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((entries.len() as u16).to_le_bytes());
    for (tag, kind, value) in entries {
        out.extend(tag.to_le_bytes());
        out.extend(kind.to_le_bytes());
        out.extend(1u32.to_le_bytes());
        out.extend(value.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend([10, 20, 30, 40]);
    out
}

/// A raster with no coordinates (a TIFF without tags, a PNG, a JPEG) opens as a layer drawn over
/// the view and marked as not georeferenced, from a path or from bytes; a project keeps where it
/// was laid; a photo with a GPS position is still a photo.
#[test]
fn rasters_without_coordinates_open_laid_over_the_view() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-scans-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let encode = |format| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(40, 20, |x, y| image::Rgb([x as u8, y as u8, 9])))
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    };
    let files = [
        ("map.tif", tagless_tiff(), (2, 2, 1)),
        ("plate.png", encode(image::ImageFormat::Png), (40, 20, 3)),
        ("print.jpg", encode(image::ImageFormat::Jpeg), (40, 20, 3)),
    ];
    let mut s = world();
    let view = crate::cmd::workspace::view_extent(&s);
    for (name, bytes, (w, h, bands)) in &files {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        // A JPEG is a photo by default (without a GPS position it is named and left out).
        let jpeg = name.ends_with(".jpg");
        let mut q = json!({"path": path.display().to_string()});
        if jpeg {
            assert!(s.run("layer.addData", &q).is_err(), "a photo without a position is not added");
            assert!(s.map().layer_by_name("print").is_none());
            q["as"] = json!("scan");
        }
        let r = s.run("layer.addData", &q).unwrap();
        let stem = name.split('.').next().unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["bands"].as_u64()), (Some(*w), Some(*h), Some(*bands)), "{r}");
        assert_eq!(r["georeferenced"], false, "{r}");
        assert!(r["note"].as_str().is_some_and(|t| t.contains("no coordinates")), "{r}");
        assert!(s.notices.iter().any(|n| n.text.contains(stem) && n.text.contains("no coordinates")));
        let rl = s.map().layer_by_name(stem).unwrap().raster.clone().unwrap();
        let data = rl.data.clone().unwrap();
        assert!(!data.georeferenced);
        let e = data.extent();
        assert!(e.min_x >= view.min_x && e.max_x <= view.max_x && e.min_y >= view.min_y && e.max_y <= view.max_y, "{e:?} in {view:?}");
        assert!(rl.placement.is_some());
        // The web app's way: the bytes.
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut q = json!({"fileName": name, "data": b64, "name": format!("{stem} upload")});
        if jpeg {
            q["as"] = json!("scan");
        }
        let r = s.run("layer.addDataBytes", &q).unwrap();
        assert_eq!((r["georeferenced"].clone(), r["width"].as_u64()), (json!(false), Some(*w)), "{r}");
    }
    // The project lays a reopened scan where it was.
    let before = s.map().layer_by_name("plate").unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().extent();
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("plate").unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert!(!again.georeferenced);
    let (a, b) = (again.extent(), before);
    assert!((a.min_x - b.min_x).abs() < 1e-9 && (a.max_y - b.max_y).abs() < 1e-9, "{a:?} vs {b:?}");
    // Georeferencing it gives it coordinates for real.
    let l = s.run("raster.georeference", &json!({"layer": "plate", "extent": [10.0, 40.0, 12.0, 41.0]})).unwrap();
    assert!(l["extent"].is_array());
    assert!(s.map().layer_by_name("plate").unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().georeferenced);
    // Damaged ones are errors, and add nothing.
    let count = s.map().layers.len();
    for (name, bytes, _) in &files {
        let cut = base64::engine::general_purpose::STANDARD.encode(&bytes[..bytes.len() / 2]);
        assert!(s.run("layer.addDataBytes", &json!({"fileName": name, "data": cut, "as": "scan"})).is_err(), "{name}");
        std::fs::write(dir.join("cut"), &bytes[..bytes.len() / 2]).unwrap();
        let cut_path = dir.join(format!("cut.{}", name.split('.').nth(1).unwrap()));
        std::fs::rename(dir.join("cut"), &cut_path).unwrap();
        assert!(s.run("layer.addData", &json!({"path": cut_path.display().to_string(), "as": "scan"})).is_err(), "{name}");
    }
    assert_eq!(s.map().layers.len(), count);
    assert!(s.run("layer.addData", &json!({"path": dir.join("map.tif").display().to_string(), "as": "pdf"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scan never leaves with coordinates it doesn't have: it can't be exported, and the tools that
/// would give their result a place on the ground refuse it; a GeoTIFF opened `as` a scan is one
/// too, in the project as well.
#[test]
fn scans_have_no_coordinates_to_give_away() {
    let mut s = Session::new(Project::default());
    let info = |s: &mut Session| s.run("raster.info", &json!({"layer": "utm"})).unwrap();
    let r = s.run("layer.addDataBytes", &json!({"fileName": "utm.tif", "data": utm_tiff_b64()})).unwrap();
    assert!(r.get("georeferenced").is_none());
    assert_eq!(info(&mut s)["georeferenced"], true);
    s.run("raster.editPixels", &json!({"layer": "utm", "polygon": [[-81.5, 41.5], [-80.5, 41.5], [-81.0, 42.5]], "value": 1})).unwrap();
    let r = s.run("layer.addDataBytes", &json!({"fileName": "utm.tif", "data": utm_tiff_b64(), "as": "scan", "name": "flat"})).unwrap();
    assert_eq!(r["georeferenced"], false, "{r}");
    let flat = |s: &mut Session| s.run("raster.info", &json!({"layer": "flat"})).unwrap();
    assert_eq!(flat(&mut s)["georeferenced"], false);
    let dir = std::env::temp_dir().join(format!("rustgis-scan-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    s.active_layer = s.map().layer_by_name("flat").map(|l| l.id);
    let out = dir.join("flat.tif");
    // A file left by an earlier, interrupted run would pass for one written now.
    let _ = std::fs::remove_file(&out);
    let e = s.run("share.exportRaster", &json!({"path": out.display().to_string()})).unwrap_err();
    assert!(e.contains("georeference it first") && !out.exists(), "{e}");
    let e = s.run("share.exportRaster", &json!({"path": out.display().to_string(), "cog": true})).unwrap_err();
    assert!(e.contains("georeference it first") && !out.exists(), "as a COG: {e}");
    let before = s.map().layer_by_name("flat").unwrap().raster.clone().unwrap().placement;
    let tools: [(&str, Value); 14] = [
        ("raster.function", json!({"layer": "flat", "function": "hillshade"})),
        ("raster.editPixels", json!({"layer": "flat", "polygon": [[0.0, 0.0], [0.1, 0.0], [0.0, 0.1]], "value": 1})),
        ("raster.process", json!({"layer": "flat", "operation": "resample", "factor": 2})),
        ("raster.process", json!({"layer": "flat", "operation": "clip"})),
        // Refused before the polygons are looked for, let alone placed on it.
        ("raster.process", json!({"layer": "flat", "operation": "clip", "polygon": "Nowhere"})),
        ("raster.mosaic", json!({"inputs": ["flat", "utm"]})),
        ("raster.change", json!({"before": "flat", "after": "flat"})),
        ("raster.classify", json!({"layer": "flat", "classes": 2})),
        ("raster.classifySupervised", json!({"layer": "flat", "training": "Nowhere", "classField": "c"})),
        ("raster.index", json!({"layer": "flat", "index": "ndvi", "red": 1, "nir": 1})),
        ("raster.measure", json!({"layer": "flat", "kind": "distance", "points": [[0.0, 0.0], [0.1, 0.1]]})),
        ("imagery.mesh", json!({"layer": "flat"})),
        ("dl.classify", json!({"layer": "flat", "training": "Nowhere", "classField": "c"})),
        ("dl.train", json!({"layer": "flat", "training": "Nowhere", "classField": "c"})),
    ];
    let missed: Vec<String> = tools
        .iter()
        .filter_map(|(cmd, q)| match s.run(cmd, q) {
            Err(e) if e.contains("no coordinates") => None,
            other => Some(format!("{cmd}: {other:?}")),
        })
        .collect();
    assert!(missed.is_empty(), "{missed:#?}");
    let l = s.map().layer_by_name("flat").unwrap().raster.clone().unwrap();
    assert_eq!(l.placement, before);
    assert!(!l.data.as_ref().unwrap().georeferenced, "still a scan");
    // The project opens it as a scan, laid where it was, not as the GeoTIFF it came from.
    let text = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let again = t.map().layer_by_name("flat").unwrap().raster.clone().unwrap().data.unwrap();
    assert!(!again.georeferenced);
    // Georeferencing it gives it coordinates, and then it exports.
    s.run("raster.georeference", &json!({"layer": "flat", "extent": [10.0, 40.0, 12.0, 41.0]})).unwrap();
    assert!(s.run("share.exportRaster", &json!({"path": out.display().to_string()})).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn layer_selection_replaces_toggles_and_extends() {
    let mut s = world();
    let name = |s: &Session, id: u32| s.map().layer(id).map(|l| l.name.clone()).unwrap_or_default();
    let picked = |s: &Session| s.selected_layers().into_iter().map(|id| name(s, id)).collect::<Vec<_>>();
    let id = |s: &Session, n: &str| s.map().layer_by_name(n).map(|l| l.id).unwrap();
    let depth = s.undo_depth();
    s.run("layer.select", &json!({"layer": "Cities"})).unwrap();
    assert_eq!(picked(&s), ["Cities"]);
    // Toggled in, the layer becomes active; toggled out, another of the selection does.
    s.run("layer.select", &json!({"layer": "Lakes", "mode": "toggle"})).unwrap();
    assert_eq!(picked(&s), ["Cities", "Lakes"]);
    assert_eq!(s.active_layer, Some(id(&s, "Lakes")));
    s.run("layer.select", &json!({"layer": "Lakes", "mode": "toggle"})).unwrap();
    assert_eq!(picked(&s), ["Cities"]);
    assert_eq!(s.active_layer, Some(id(&s, "Cities")));
    // Extended by a list; the last of it is active. A plain select starts over.
    s.run("layer.select", &json!({"layer": ["Rivers", "Countries", "Rivers"], "mode": "extend"})).unwrap();
    assert_eq!(picked(&s), ["Cities", "Rivers", "Countries"]);
    assert_eq!(s.active_layer, Some(id(&s, "Countries")));
    s.run("layer.select", &json!({"layer": "Rivers"})).unwrap();
    assert_eq!(picked(&s), ["Rivers"]);
    // Choosing layers is not an undo step. A bad layer, mode or list changes nothing.
    assert_eq!(s.undo_depth(), depth);
    s.run("layer.select", &json!({"layer": ["Cities", "Lakes"]})).unwrap();
    for p in [
        json!({"layer": ["Cities", "Nope"]}),
        json!({"layer": [], "mode": "toggle"}),
        json!({"layer": [null]}),
        json!({"layer": [[1]]}),
        json!({"layer": "Cities", "mode": "sideways"}),
    ] {
        assert!(s.run("layer.select", &p).is_err(), "{p}");
    }
    assert_eq!(picked(&s), ["Cities", "Lakes"]);
    // Setting the active layer on its own (adding data, a removal) leaves the selection behind.
    s.active_layer = Some(id(&s, "Rivers"));
    assert_eq!(picked(&s), ["Rivers"]);
}

#[test]
fn layer_commands_act_on_the_whole_layer_selection() {
    let mut s = world();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&s), ["Cities", "Rivers", "Lakes", "Countries"]);
    // Hidden and shown layers swap to one state: the opposite of the active layer's (the last
    // one listed, Rivers, hidden: all are shown).
    s.run("layer.visibility", &json!({"layer": "Rivers", "visible": false})).unwrap();
    s.run("layer.select", &json!({"layer": ["Countries", "Rivers"]})).unwrap();
    s.run("layer.visibility", &Value::Null).unwrap();
    let shown = |s: &Session, n: &str| s.map().layer_by_name(n).unwrap().visible;
    assert!(shown(&s, "Rivers") && shown(&s, "Countries"));
    s.run("layer.visibility", &json!({"visible": false})).unwrap();
    assert!(!shown(&s, "Rivers") && !shown(&s, "Countries") && shown(&s, "Cities"));
    // To the top and to the bottom, keeping their order, each one undo step.
    s.run("layer.move", &json!({"to": 0})).unwrap();
    assert_eq!(names(&s), ["Rivers", "Countries", "Cities", "Lakes"]);
    s.run("layer.move", &json!({"to": 1e9})).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes", "Rivers", "Countries"]);
    assert!(s.run("layer.move", &json!({"to": 1})).is_err(), "several layers can't go to the middle");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(names(&s), ["Rivers", "Countries", "Cities", "Lakes"]);
    // Selectable on all of them.
    s.run("layer.selectable", &json!({"on": false})).unwrap();
    assert!(!s.map().layer_by_name("Rivers").unwrap().selectable && !s.map().layer_by_name("Countries").unwrap().selectable);
    assert!(s.map().layer_by_name("Cities").unwrap().selectable);
    // Zoom to both: a view command, no undo step.
    let depth = s.undo_depth();
    s.run("layer.zoomTo", &Value::Null).unwrap();
    assert_eq!(s.undo_depth(), depth);
    // Removing: one undo step; the layer that takes the topmost one's place is active alone.
    s.run("layer.remove", &Value::Null).unwrap();
    assert_eq!(names(&s), ["Cities", "Lakes"]);
    assert_eq!(s.undo_depth(), depth + 1);
    assert_eq!(s.selected_layers(), vec![s.active_layer.unwrap()]);
    assert_eq!(s.active_layer, s.map().layer_by_name("Cities").map(|l| l.id));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(names(&s), ["Rivers", "Countries", "Cities", "Lakes"]);
    // A list with an unknown layer removes nothing.
    assert!(s.run("layer.remove", &json!({"layer": ["Rivers", "Nope"]})).is_err());
    assert_eq!(names(&s).len(), 4);
}

#[test]
fn layer_selection_survives_a_failed_project_open() {
    let mut s = world();
    s.run("layer.select", &json!({"layer": ["Cities", "Lakes"]})).unwrap();
    let before = s.selected_layers();
    // Another project replaces this one and fails: the selection comes back with it.
    let _ = s.run("project.openText", &json!({"text": "not a project"}));
    assert_eq!(s.selected_layers(), before);
}
