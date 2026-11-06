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
