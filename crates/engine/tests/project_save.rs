use rustgis_engine::{Project, Session, cmd::design::EditCounts};
use serde_json::{Value, json};

/// Failed Save As preserves session state and history, and a later save can still succeed.
#[test]
fn failed_save_as_preserves_project_state_and_a_later_save_succeeds() -> Result<(), String> {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let root = std::env::temp_dir().join(format!("rustgis-save-failure-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    let original = root.join("original.rgis");
    let occupied = root.join("occupied.rgis");
    std::fs::create_dir(&occupied).map_err(|e| e.to_string())?;
    let marker = occupied.join("keep.txt");
    std::fs::write(&marker, b"keep destination contents").map_err(|e| e.to_string())?;

    for saved_before in [false, true] {
        for dirty in [false, true] {
            let mut s = Session::new(Project::new("original"));
            if saved_before {
                s.run("project.saveAs", &json!({"path": original}))?;
            }
            // Keep a real redo step, as well as undo history when the project is dirty.
            s.run("project.rename", &json!({"name": "pending"}))?;
            s.run("project.rename", &json!({"name": "redo name"}))?;
            s.run("edit.undo", &Value::Null)?;
            if dirty {
                s.mark_unsaved()
            } else {
                s.mark_saved()
            }
            s.edit_counts.insert(1, EditCounts { added: 2, modified: 3, deleted: 1 });
            let before = serde_json::to_value(&s.project).map_err(|e| e.to_string())?;
            let path = s.path.clone();
            let depth = s.undo_depth();
            let counts = s.edit_counts.clone();
            let history = (s.undo_label(), s.redo_label());
            assert!(s.can_undo() && s.can_redo());
            for target in [root.join("missing/changed.rgis"), occupied.clone()] {
                assert!(s.run("project.saveAs", &json!({"path": target})).is_err());
                assert_eq!(serde_json::to_value(&s.project).map_err(|e| e.to_string())?, before);
                assert_eq!(s.path, path);
                assert_eq!(s.dirty(), dirty);
                assert_eq!(s.undo_depth(), depth);
                assert_eq!(s.edit_counts, counts);
                assert_eq!((s.undo_label(), s.redo_label()), history);
                assert!(s.can_undo() && s.can_redo());
                assert_eq!(std::fs::read(&marker).map_err(|e| e.to_string())?, b"keep destination contents");
                if saved_before {
                    assert_eq!(Project::load(&original)?.name, "original");
                }
            }
            s.run("edit.redo", &Value::Null)?;
            assert_eq!(s.project.name, "redo name");
            s.run("edit.undo", &Value::Null)?;
            assert_eq!(serde_json::to_value(&s.project).map_err(|e| e.to_string())?, before);

            s.edit_counts = counts;
            let saved = root.join("renamed");
            let expected = saved.with_extension("rgis");
            s.run("project.saveAs", &json!({"path": saved}))?;
            assert_eq!(s.project.name, "renamed");
            assert_eq!(Project::load(&expected)?.name, "renamed");
            assert_eq!(s.path.as_ref(), Some(&expected));
            assert!(!s.dirty());
            assert!(s.edit_counts.is_empty());
            assert_eq!(s.undo_depth(), depth);
            assert!(s.can_redo());
        }
    }
    std::fs::remove_dir_all(root).map_err(|e| e.to_string())
}

#[test]
fn project_from_newer_rustgis_reports_update_message() {
    let json = serde_json::json!({
        "format": 9999,
        "name": "future project"
    })
    .to_string();
    let err = Project::from_json(&json).unwrap_err();
    assert!(err.contains("saved by a newer RustGIS"));
    assert!(err.contains("Update RustGIS to open it"));
}

#[test]
fn project_with_unknown_variant_suggests_newer_version() {
    let json = serde_json::json!({
        "format": 1,
        "name": "project with unknown enum variant",
        "maps": [{
            "name": "Map",
            "projection": "FutureProjectionVariant"
        }]
    })
    .to_string();
    let err = Project::from_json(&json).unwrap_err();
    assert!(err.contains("may have been saved by a newer RustGIS") || err.contains("Update RustGIS to open it"));
}

/// Future metadata wins over incompatible payloads through direct, alternate and package imports.
#[test]
fn file_compatibility_rejects_future_formats_before_importing() -> Result<(), String> {
    use base64::Engine;
    for format in [u64::from(rustgis_engine::project::FORMAT) + 1, u64::from(u32::MAX) + 1] {
        let text = json!({"format": format, "name": "Future", "maps": "new payload", "layers": "new payload"}).to_string();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&text);
        let mut s = Session::new(Project::new("Original"));
        s.run("project.rename", &json!({"name": "Keep"}))?;
        s.path = Some("original.rgis".into());
        s.mark_saved();
        let before = serde_json::to_value(&s.project).map_err(|e| e.to_string())?;
        let history = (s.undo_depth(), s.undo_label(), s.redo_label());
        let commands = ["map.import", "style.import", "layer.addLayerFile", "style.add", "project.fromTemplate"];
        for (command, params) in commands
            .iter()
            .flat_map(|command| [(*command, json!({"text": text})), (*command, json!({"data": encoded}))])
            .chain([("project.openText", json!({"text": text})), ("project.openText", json!({"text": text, "package": true}))])
        {
            let error = s.run(command, &params).unwrap_err();
            assert!(error.contains("Update RustGIS to open it") && error.contains(&format!("format {format}")), "{command}: {error}");
            assert_eq!(serde_json::to_value(&s.project).map_err(|e| e.to_string())?, before, "{command}");
            assert_eq!((s.undo_depth(), s.undo_label(), s.redo_label()), history, "{command}");
            assert_eq!(s.path.as_deref(), Some(std::path::Path::new("original.rgis")));
            assert!(!s.dirty(), "{command}");
        }
        for file in ["future.rgis", "future.rgismap"] {
            let package = rustgis_engine::data::zip::write(&[(file.into(), text.as_bytes().to_vec())])?;
            let data = base64::engine::general_purpose::STANDARD.encode(package);
            for command in ["map.import", "style.import"] {
                let error = s.run(command, &json!({"data": data})).unwrap_err();
                assert!(error.contains("Update RustGIS to open it"), "{file}, {command}: {error}");
                assert_eq!(serde_json::to_value(&s.project).map_err(|e| e.to_string())?, before);
            }
        }
    }
    Ok(())
}

/// Existing writers keep bare payloads, while readers accept legacy and explicitly supported metadata.
#[test]
fn file_compatibility_keeps_current_and_legacy_companion_files() -> Result<(), String> {
    use rustgis_engine::{data::Dataset, data::GeometryType, map::Layer};
    let mut original = Session::new(Project::new("Original"));
    original.map_mut().add_layer(Layer::new(1, Dataset::new("Points", GeometryType::Point, Vec::new())));
    original.run("style.new", &json!({"layer": "Points"}))?;
    for (kind, command, params) in
        [("map", "map.import", json!({})), ("layer", "layer.addLayerFile", json!({"layer": "Points"})), ("styles", "style.add", json!({}))]
    {
        let (_, bytes) = rustgis_engine::cmd::items::item_bytes(&mut original, kind, &params)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        assert!(value.get("format").is_none(), "current writer unchanged: {kind}");
        for declared in [false, true] {
            if declared && kind == "styles" {
                continue; // Style writers still emit the existing array, with no wrapper.
            }
            let mut payload = value.clone();
            if declared {
                payload["format"] = json!(rustgis_engine::project::FORMAT);
            }
            let mut loaded = Session::new(Project::new("Loaded"));
            loaded.run(command, &json!({"text": payload.to_string()}))?;
            let loaded_layer = match kind {
                "map" | "layer" => Some(loaded.map().layer_by_name("Points").ok_or("missing imported layer")?),
                _ => None,
            };
            if let Some(layer) = loaded_layer {
                assert_eq!(layer.renderer, original.map().layers[0].renderer);
                assert_eq!(layer.labels, original.map().layers[0].labels);
                loaded.run("style.import", &json!({"text": payload.to_string()}))?;
            }
            assert_eq!(loaded.project.styles, original.project.styles);
            assert!(loaded.notices.iter().all(|n| !n.error));
        }
    }
    Ok(())
}

/// Invalid version metadata cannot be ignored by permissive companion deserializers.
#[test]
fn file_compatibility_rejects_malformed_metadata() -> Result<(), String> {
    let mut map = serde_json::to_value(rustgis_engine::map::Map::new("Test")).map_err(|e| e.to_string())?;
    for format in [json!(null), json!(-1), json!(1.5), json!("2"), json!({})] {
        map["format"] = format;
        for command in ["map.import", "style.import"] {
            let mut s = Session::new(Project::new("Original"));
            let error = s.run(command, &json!({"text": map.to_string()})).unwrap_err();
            assert!(error.contains("not a RustGIS"), "{command}: {error}");
            assert_eq!(s.project.maps.len(), 1);
            assert!(s.project.styles.is_empty());
        }
    }
    // A supported project with an unknown enum retains its update advice on alternate routes.
    let mut project = serde_json::to_value(Project::new("Unknown")).map_err(|e| e.to_string())?;
    project["maps"][0]["projection"] = json!("FutureProjection");
    for command in ["map.import", "style.import"] {
        let error = Session::new(Project::default()).run(command, &json!({"text": project.to_string()})).unwrap_err();
        assert!(error.contains("Update RustGIS to open it"), "{command}: {error}");
    }
    Ok(())
}

/// Resaving a supported older project must identify today's writer in every project container.
#[test]
fn file_compatibility_resaves_declare_the_current_writer() -> Result<(), String> {
    let mut legacy = serde_json::to_value(Project::new("Legacy")).map_err(|e| e.to_string())?;
    legacy["format"] = json!(0);
    let mut s = Session::new(Project::from_json(&legacy.to_string())?);
    assert_eq!(s.project.format, 0, "the loaded version is available independently");
    let (text, _) = s.project.saved_text(None)?;
    let saved: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    assert_eq!(saved["format"], json!(rustgis_engine::project::FORMAT));
    for kind in ["template", "projectPackage"] {
        let (_, bytes) = rustgis_engine::cmd::items::item_bytes(&mut s, kind, &json!({}))?;
        let text = if kind == "projectPackage" {
            rustgis_engine::cmd::items::package_text(&bytes)?
        } else {
            String::from_utf8(bytes).map_err(|e| e.to_string())?
        };
        let saved: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        assert_eq!(saved["format"], json!(rustgis_engine::project::FORMAT), "{kind}");
    }
    Ok(())
}

/// A number that isn't finite (NaN, infinity) can't make a project unopenable: Set Value refuses
/// one, and one a reader put in a layer is saved as null with a count and dataset location.
/// A project of finite numbers saves and opens without a loss notice.
#[test]
fn numbers_that_are_not_finite_never_make_a_project_unopenable() -> Result<(), String> {
    use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
    let mut s = Session::new(Project::default());
    let id = s.map_mut().add_dataset(Dataset::new("Readings", GeometryType::None, vec![Field::new("level", FieldType::Double)]));
    let ds = &mut s.map_mut().layer_mut(id).ok_or("no layer")?.dataset;
    ds.push(None, vec![V::Float(1.5)]);
    ds.push(None, vec![V::Float(2.5)]);
    for typed in ["NaN", " inf ", "-infinity", "1e400"] {
        let e = s.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "level", "value": typed})).err().ok_or(typed)?;
        assert!(e.contains("isn't a finite number"), "{typed}: {e}");
    }
    assert_eq!(s.map().layer(id).ok_or("no layer")?.dataset.value(0, 0), &V::Float(1.5), "a refused value changes nothing");
    let losses = |notices: Vec<String>| notices.into_iter().filter(|n| n.contains("finite numbers")).collect::<Vec<_>>();

    // Clean: no notice, and the numbers open as they were.
    let (text, report) = s.project.saved_text(None)?;
    assert!(losses(report.notices()).is_empty(), "{:?}", report.notices());
    assert_eq!(Project::from_json(&text)?.maps[0].layers[0].dataset.value(1, 0), &V::Float(2.5));

    // As a reader leaves them (a NaN REAL in a GeoPackage): saved as null, and counted.
    let ds = &mut s.map_mut().layer_mut(id).ok_or("no layer")?.dataset;
    ds.features[0].attrs[0] = V::Float(f64::NAN);
    ds.features[1].attrs[0] = V::Float(f64::NEG_INFINITY);
    let (text, report) = s.project.saved_text(None)?;
    assert_eq!(
        losses(report.notices()),
        ["2 attribute value(s) that aren't finite numbers (NaN or infinity) were written as null; affected datasets include 'Readings'"]
    );
    let mark = s.notice_mark();
    report.notify(&mut s);
    let notice = s.notices_since(mark).last().ok_or("no counted output notice")?;
    assert_eq!(
        notice
            .translated(|key, args| {
                assert!(key.starts_with("{count} attribute value(s)"));
                assert!(args.contains(&("places", "'Readings'")));
                "translated loss notice".to_string()
            })
            .as_deref(),
        Some("translated loss notice")
    );
    let opened = Project::from_json(&text)?;
    assert_eq!(opened.maps[0].layers[0].dataset.value(0, 0), &V::Null);
    assert_eq!(opened.maps[0].layers[0].dataset.value(1, 0), &V::Null);

    // Opened clean: no notice.
    let mark = s.notice_mark();
    s.run("project.openText", &json!({"text": Project::from_json(&text).and_then(|mut p| p.saved_text(None))?.0}))?;
    assert!(losses(s.notices_since(mark).iter().map(|n| n.text.clone()).collect()).is_empty());
    // Other interactive ingress either rejects a non-finite value or reports its conversion.
    let before = s.map().layers[0].dataset.features.len();
    assert!(
        s.run(
            "edit.addFeature",
            &json!({"layer": "Readings", "geometry": {"type": "Point", "coordinates": [0, 0]}, "attributes": {"level": "NaN"}})
        )
        .is_err()
    );
    assert_eq!(s.map().layers[0].dataset.features.len(), before);
    let mark = s.notice_mark();
    s.run("edit.calculateField", &json!({"layer": "Readings", "field": "level", "expression": "'NaN'"}))?;
    assert_eq!(s.notices_since(mark).iter().filter(|n| n.text.contains("'level' is null for 2 results that aren't numbers")).count(), 1);

    // A declared Double in MIF/MID retains its non-finite source number until counted output.
    // CSV inference deliberately keeps a mixed NaN/numeric column as lossless text.
    let mif = "Version 300\nCoordSys Earth Projection 1, 104\nColumns 1\n level Float\nData\nPoint 0 0\nPoint 1 1\n";
    let (dataset, _) = rustgis_engine::data::mapinfo::read(mif, Some("NaN\n1.5\n"), "Imported").map_err(|e| e.to_string())?;
    assert!(matches!(dataset.value(0, 0), V::Float(f) if f.is_nan()));
    s.map_mut().add_dataset(dataset);
    let (_, report) = s.project.saved_text(None)?;
    assert_eq!(
        losses(report.notices()),
        ["1 attribute value(s) that aren't finite numbers (NaN or infinity) were written as null; affected datasets include 'Imported'"]
    );
    Ok(())
}

/// Finite current rows must not hide non-finite values kept in saved design and edit history.
#[test]
fn nonfinite_design_and_archived_values_are_counted_when_output_is_prepared() -> Result<(), String> {
    use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
    let mut s = Session::new(Project::default());
    let mut dataset = Dataset::new("Design", GeometryType::None, vec![Field::new("level", FieldType::Double)]);
    dataset.push(None, vec![V::Float(f64::NAN)]);
    s.map_mut().add_dataset(dataset);
    s.run("data.archive", &json!({"layer": "Design"}))?;
    s.run("edit.setValue", &json!({"layer": "Design", "row": 0, "field": "level", "value": 1}))?;
    s.run("edit.template", &json!({"layer": "Design", "name": "T", "defaults": {"level": 2}}))?;
    for text in ["NaN", "inf", "1e400"] {
        assert!(s.run("edit.template", &json!({"layer": "Design", "name": "T", "defaults": {"level": text}})).is_err());
        assert_eq!(s.map().layers[0].design.templates[0].defaults[0].1, V::Float(2.0));
    }
    // Native callers and imported designs can still hold hostile defaults: output must count them.
    s.map_mut().layers[0].design.templates[0].defaults[0].1 = V::Float(f64::NAN);
    assert_eq!(s.map().layers[0].dataset.unsaved_numbers(), 0);
    assert_eq!(s.map().layers[0].design.unsaved_numbers(), 2);
    for (text, report) in [s.project.saved_text(None)?, s.project.clone().recovery_text_with_report()?] {
        assert_eq!(
            report
                .notices()
                .iter()
                .filter(|notice| notice.contains("2 attribute value(s)") && notice.contains("'Design' (design or archived values)"))
                .count(),
            1
        );
        let opened = Project::from_json(&text)?;
        let design = &opened.maps[0].layers[0].design;
        assert_eq!(design.templates[0].defaults[0].1, V::Null);
        assert_eq!(design.archive.as_ref().ok_or("no archive")?[0].before.as_ref().ok_or("no archived feature")?.attrs[0], V::Null);
    }
    Ok(())
}

/// Both resident and streamed exports report their numeric replacements once, with the path.
#[test]
fn nonfinite_layer_exports_report_nulls() -> Result<(), String> {
    use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
    let root = std::env::temp_dir().join(format!("rustgis-nonfinite-export-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let source = root.join("source.gpkg");
    let mut ds = Dataset::new("Readings", GeometryType::Point, vec![Field::new("level", FieldType::Double)]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![V::Float(1.5)]);
    rustgis_engine::data::io::write_path(&ds, &source).map_err(|e| e.to_string())?;
    for stream in [false, true] {
        for lossy in [false, true] {
            let conn = rusqlite::Connection::open(&source).map_err(|e| e.to_string())?;
            // Attribute UPDATEs also compile the R-tree triggers. This fixture's geometry is
            // always the non-empty point (0, 0), so all five spatial functions return zero.
            let flags = rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC
                | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS;
            for name in ["ST_IsEmpty", "ST_MinX", "ST_MaxX", "ST_MinY", "ST_MaxY"] {
                conn.create_scalar_function(name, 1, flags, |_| Ok(0i64)).map_err(|e| e.to_string())?;
            }
            conn.execute("UPDATE Readings SET level = ?1", [if lossy { "NaN" } else { "1.5" }]).map_err(|e| e.to_string())?;
            drop(conn);
            let mut s = Session::new(Project::default());
            let id = s.run("layer.addData", &json!({"path": source, "stream": stream}))?["layer"].as_u64().ok_or("no id")?;
            assert_eq!(s.map().layer(id as u32).ok_or("no layer")?.is_streamed(), stream);
            for ext in ["geojson", "gpkg"] {
                let out = root.join(format!("out-{stream}-{lossy}.{ext}"));
                if out.exists() {
                    std::fs::remove_file(&out).map_err(|e| e.to_string())?;
                }
                let mark = s.notice_mark();
                s.run("share.exportLayer", &json!({"layer": id, "path": out}))?;
                let losses: Vec<_> = s.notices_since(mark).iter().filter(|n| n.text.contains("finite numbers")).collect();
                assert_eq!(losses.len(), usize::from(lossy), "stream {stream}: {losses:?}");
                if lossy {
                    assert!(losses[0].text.contains("1 attribute value(s)"));
                    assert!(losses[0].text.contains(out.to_str().ok_or("path")?));
                    assert!(losses[0].translated(|key, _| key.to_string()).is_some());
                }
                let read = rustgis_engine::data::io::read_path(&out).map_err(|e| e.to_string())?.0;
                assert_eq!(read.value(0, 0), if lossy { &V::Null } else { &V::Float(1.5) });
            }
        }
    }
    std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
    Ok(())
}

/// Interactive and previously imported subtype defaults cannot introduce a non-finite cell.
#[test]
fn nonfinite_subtype_defaults_are_refused_before_mutation() -> Result<(), String> {
    use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
    let mut s = Session::new(Project::default());
    let id = s.map_mut().add_dataset(Dataset::new(
        "Defaults",
        GeometryType::Point,
        vec![Field::new("kind", FieldType::Integer), Field::new("level", FieldType::Double)],
    ));
    s.run("data.subtypes", &json!({"layer": id, "field": "kind", "subtypes": [{"code": 1, "name": "One", "defaults": {"level": 2.5}}]}))?;
    for text in ["NaN", "inf", "1e400"] {
        assert!(
            s.run(
                "data.subtypes",
                &json!({"layer": id, "field": "kind", "subtypes": [{"code": 1, "name": "One", "defaults": {"level": text}}]})
            )
            .is_err()
        );
        let sub = s.map().layer(id).ok_or("no layer")?.design.subtypes.as_ref().ok_or("no subtypes")?;
        assert_eq!(sub.subtypes[0].defaults[0].1, V::Float(2.5));
    }
    let add = json!({"layer": id, "geometry": {"type": "Point", "coordinates": [0,0]}, "attributes": {"kind": 1}});
    s.run("edit.addFeature", &add)?;
    assert_eq!(s.map().layer(id).ok_or("no layer")?.dataset.value(0, 1), &V::Float(2.5));
    s.map_mut().layer_mut(id).ok_or("no layer")?.design.subtypes.as_mut().ok_or("no subtypes")?.subtypes[0].defaults[0].1 =
        V::Text("NaN".into());
    let count = s.map().layer(id).ok_or("no layer")?.dataset.features.len();
    assert!(s.run("edit.addFeature", &add).is_err());
    assert_eq!(s.map().layer(id).ok_or("no layer")?.dataset.features.len(), count);
    Ok(())
}

/// Set Value keeps its finite coercions, while reporting every lossy one and staying quiet for exact input.
#[test]
fn set_value_reports_numeric_approximations() -> Result<(), String> {
    use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
    let mut s = Session::new(Project::default());
    let mut ds = Dataset::new("Numbers", GeometryType::None, vec![Field::new("n", FieldType::Integer)]);
    ds.push(None, vec![V::Int(1)]);
    let id = s.map_mut().add_dataset(ds);
    for (text, expected, fragment) in [
        ("2.7", V::Int(2), "lost its fraction"),
        ("1e30", V::Int(i64::MAX), "clamped"),
        ("unknown", V::Null, "written as null"),
        ("9007199254740993.0", V::Int(9_007_199_254_740_992), "lost integer precision"),
        ("<NULL>", V::Null, ""),
        ("3", V::Int(3), ""),
    ] {
        let mark = s.notice_mark();
        s.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "n", "value": text}))?;
        assert_eq!(s.map().layer(id).ok_or("no layer")?.dataset.value(0, 0), &expected);
        let losses: Vec<_> = s.notices_since(mark).iter().filter(|n| n.text.contains("1 attribute value")).collect();
        assert_eq!(losses.len(), usize::from(!fragment.is_empty()));
        if !fragment.is_empty() {
            assert!(losses[0].text.contains(fragment));
            assert!(losses[0].text.contains("Numbers, field 'n', row 1"));
        }
    }
    Ok(())
}

#[test]
fn add_feature_reports_numeric_approximations() -> Result<(), String> {
    use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
    let mut s = Session::new(Project::default());
    let ds = Dataset::new("Numbers", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    let id = s.map_mut().add_dataset(ds);
    for (row, (text, expected, fragment)) in [
        ("2.7", V::Int(2), "lost its fraction"),
        ("1e30", V::Int(i64::MAX), "clamped"),
        ("unknown", V::Null, "written as null"),
        ("<NULL>", V::Null, ""),
        ("3", V::Int(3), ""),
    ]
    .into_iter()
    .enumerate()
    {
        let mark = s.notice_mark();
        s.run("edit.addFeature", &json!({"layer": id, "geometry": {"type":"Point", "coordinates":[0,0]}, "attributes": {"n": text}}))?;
        assert_eq!(s.map().layer(id).ok_or("no layer")?.dataset.value(row, 0), &expected);
        let losses: Vec<_> = s.notices_since(mark).iter().filter(|n| n.text.contains("1 attribute value")).collect();
        assert_eq!(losses.len(), usize::from(!fragment.is_empty()));
        if !fragment.is_empty() {
            assert!(losses[0].text.contains(fragment));
            assert!(losses[0].text.contains(&format!("Numbers, field 'n', row {}", row + 1)));
        }
    }
    Ok(())
}

/// Null substitution cannot be persisted with an editable source's non-finite row baseline.
#[test]
fn project_output_refuses_to_manufacture_source_edits() -> Result<(), String> {
    use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
    use rustgis_engine::map::{
        Layer,
        source::{PostgisEdit, PostgisOrigin, RowVersion, row_hash},
    };
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let path = std::env::temp_dir().join(format!("rustgis-editable-nonfinite-{}-{nonce}.rgis", std::process::id()));
    std::fs::write(&path, b"keep destination").map_err(|e| e.to_string())?;
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut s = Session::new(Project::default());
        let mut ds = Dataset::new("Editable", GeometryType::None, vec![Field::new("n", FieldType::Double)]);
        ds.push(None, vec![V::Float(number)]);
        let versions = ds.features.iter().map(|f| (f.id, RowVersion { version: 7, hash: row_hash(f) })).collect();
        let id = s.map_mut().add_dataset(ds);
        let layer = s.map_mut().layer_mut(id).ok_or("no layer")?;
        layer.postgis = Some(PostgisEdit {
            origin: PostgisOrigin { database: "postgres:dbname=example".into(), table: "numbers".into(), family: None, crs: None },
            key: "id".into(),
            srid: 4326,
            writable: true,
            condition: None,
            limit: 0,
            versions,
        });
        // Demonstrate why serializing the value independently is unsafe for editable snapshots.
        let raw = serde_json::to_string(layer).map_err(|e| e.to_string())?;
        let back: Layer = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        assert_eq!(back.edit_versions().ok_or("no baseline")?.changes(&back.dataset.features).updates.len(), 1);
        for error in [s.project.saved_text(None).unwrap_err(), s.project.clone().recovery_text_with_report().unwrap_err()] {
            assert!(error.contains("1 non-finite attribute value(s)"), "{error}");
            assert!(error.contains("editable layer 'Editable'"), "{error}");
        }
        assert!(s.project.save(&path).is_err());
        assert_eq!(std::fs::read(&path).map_err(|e| e.to_string())?, b"keep destination");
        let layer = s.map().layer(id).ok_or("no layer")?;
        assert!(layer.dataset.value(0, 0).is_non_finite());
        assert!(layer.edit_versions().ok_or("no baseline")?.changes(&layer.dataset.features).is_empty());
        // Clearing it intentionally remains a real pending source edit after save and reopen.
        s.run("edit.setValue", &json!({"layer":id,"row":0,"field":"n","value":null}))?;
        let (text, report) = s.project.saved_text(None)?;
        assert!(report.notices().is_empty());
        let reopened = Project::from_json(&text)?;
        let layer = reopened.maps[0].layer(id).ok_or("no reopened layer")?;
        assert_eq!(layer.edit_versions().ok_or("no baseline")?.changes(&layer.dataset.features).updates.len(), 1);
    }
    std::fs::remove_file(path).map_err(|e| e.to_string())?;
    Ok(())
}
