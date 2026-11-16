//! Committed files and their losses remain visible after a workflow rolls its project back.

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Cell};
use serde_json::{Value, json};

use crate::{Project, Session};

struct Files(std::path::PathBuf);

impl Files {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("rustgis-write-notices-{}-{time}-{serial}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn places() -> Session {
    let mut s = Session::new(Project::default());
    let mut ds = Dataset::new("Places", GeometryType::Point, vec![Field::new("long_attribute_name", FieldType::Text)]);
    ds.crs = rustgis_data::raster::Crs::from_epsg(4326);
    ds.push(Some(Geometry::Point(rustgis_data::Coord::new(1.0, 2.0))), vec![Cell::Text("kept".into())]);
    s.map_mut().add_dataset(ds);
    s.map_mut().editing.settings.enabled = true;
    s
}

#[test]
fn write_notices_survive_nested_script_task_errors_and_panics() {
    for mode in ["success", "script", "nested", "task", "panic"] {
        let dir = Files::new();
        let path = dir.path().join("export.shp");
        let mut s = places();
        let export = json!({"layer":"Places","path":path});
        let edit = json!({"layer":"Places","field":"long_attribute_name"});
        let fail = json!({"layer":"not-here"});
        let body = format!("share.exportLayer {export}\nedit.deleteField {edit}\nlayer.remove {fail}");
        if matches!(mode, "task" | "nested") {
            let steps = if mode == "nested" {
                json!([{"title":"Script","command":"script.run","params":{"script":body}}])
            } else {
                json!([
                    {"title":"Write","command":"share.exportLayer","params":export},
                    {"title":"Edit","command":"edit.deleteField","params":edit},
                    {"title":"Fail","command":"layer.remove","params":fail}
                ])
            };
            s.run("task.new", &json!({"name":"Write then fail","steps":steps})).unwrap();
        }
        let mark = s.notice_mark();
        let depth = s.undo_depth();
        let result = match mode {
            "success" => s.run("share.exportLayer", &export),
            "task" | "nested" => s.run("task.run", &json!({"task":"Write then fail"})),
            "panic" => s.guarded("Panic after write", true, |s| {
                s.run_part("share.exportLayer", &export)?;
                s.run_part("edit.deleteField", &edit)?;
                panic!("after write");
            }),
            _ => s.run("script.run", &json!({"script":body})),
        };
        assert_eq!(result.is_ok(), mode == "success", "{mode}: {result:?}");
        assert!(path.is_file(), "{mode}: {result:?}");
        let (back, _) = rustgis_data::io::read_path(&path).unwrap();
        assert_eq!(back.fields[0].name, "long_attri");
        assert_eq!(back.features.len(), 1);
        let notices = s.notices_since(mark);
        let loss: Vec<_> = notices.iter().filter(|n| n.text.contains("1 field name was changed")).collect();
        assert_eq!(loss.len(), 1, "{mode}: {notices:?}");
        assert!(loss[0].text.contains("long_attribute_name") && loss[0].text.contains("long_attri"));
        assert_eq!(notices.iter().filter(|n| n.text == format!("Exported {}", path.display())).count(), 1, "{mode}: {notices:?}");
        if mode != "success" {
            assert!(s.map().layer_by_name("Places").unwrap().dataset.field_index("long_attribute_name").is_some());
            assert!(notices.iter().filter(|n| !n.error).all(|n| !n.text.contains("Deleted")), "rolled-back edit: {notices:?}");
            assert_eq!(notices.iter().filter(|n| !n.error).count(), 2, "{mode}: {notices:?}");
            assert_eq!(s.undo_depth(), depth);
        }
    }
}

#[test]
fn write_notices_keep_tool_losses_from_a_failed_model() {
    let dir = Files::new();
    let path = dir.path().join("places.pmtiles");
    let mut s = places();
    s.map_mut().layers.iter_mut().find(|l| l.name == "Places").unwrap().dataset.features[0].geometry =
        Some(Geometry::Point(rustgis_data::Coord::new(1.0, 89.0)));
    let params = json!({"input":"Places","minZoom":0,"maxZoom":0,"output":path});
    s.run(
        "model.new",
        &json!({"name":"Write then fail","steps":[
            {"tool":"vectorToPmtiles","params":params},
            {"tool":"buffer","params":{"input":"missing","distance":1}}
        ]}),
    )
    .unwrap();
    let mark = s.notice_mark();
    let result = s.run("model.run", &json!({"name":"Write then fail"}));
    assert!(result.is_err(), "{result:?}");
    assert!(path.is_file(), "{result:?}");
    assert!(std::fs::read(&path).unwrap().starts_with(b"PMTiles\x03"));
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said.iter().filter(|n| n.contains("Clamped 1 coordinates")).count(), 1, "{said:?}");
}

#[test]
fn write_notices_keep_model_graph_repairs_only_after_a_file_commit() {
    for mode in ["lossy", "clean", "failed", "inline"] {
        let dir = Files::new();
        let path = if mode == "failed" { dir.path().join("missing").join("model.json") } else { dir.path().join("model.json") };
        let mut s = places();
        let mut nodes = vec![json!({"id":"buffer","kind":"tool","tool":"buffer","params":{"input":"Places","distance":1}})];
        if mode != "clean" {
            nodes.push(json!({"kind":"tool","tool":"buffer"}));
        }
        let mut params = json!({"graph":{"nodes":nodes,"edges":[]},"path":path});
        if mode == "inline" {
            params.as_object_mut().unwrap().remove("path");
        }
        let mark = s.notice_mark();
        let script = format!("model.export {params}\nlayer.remove {{\"layer\":\"missing\"}}");
        assert!(s.run("script.run", &json!({"script":script})).is_err());
        let notices = s.notices_since(mark);
        let losses = notices.iter().filter(|n| n.text.contains("model graph: 1 node(s) left out")).count();
        assert_eq!(losses, usize::from(mode == "lossy"), "{mode}: {notices:?}");
        if matches!(mode, "lossy" | "clean") {
            let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["graph"]["nodes"].as_array().unwrap().len(), 1);
        } else {
            assert!(!path.exists(), "{mode}: no committed output");
        }
    }
}

#[test]
fn write_notices_keep_clean_exports_free_of_loss_warnings() {
    let dir = Files::new();
    let path = dir.path().join("clean.shp");
    let mut s = places();
    s.map_mut().layers.iter_mut().find(|l| l.name == "Places").unwrap().dataset.fields[0].name = "name".into();
    let mark = s.notice_mark();
    s.run("share.exportLayer", &json!({"layer":"Places","path":path})).unwrap();
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said, [format!("Exported {}", path.display())]);
}

#[test]
fn write_notices_keep_committed_losses_when_background_delivery_is_refused() {
    use crate::cmd::exports;
    use crate::policy::{AgentAccess, Effects, Revocation, Scope};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let dir = Files::new();
    let path = dir.path().join("export.shp");
    let mut s = places();
    struct Live;
    impl Revocation for Live {
        fn allows(&self, _: Effects) -> bool {
            true
        }
    }
    let scope = Scope::agent(AgentAccess::Allow, false, "write-notices-export", None, Arc::new(Live));
    let run =
        s.with_scope(scope, |s| exports::export_start(s, "share.exportLayer", &json!({"layer":"Places","path":path}))).unwrap().unwrap();
    let started = run.started();
    let mut wrote = run.run(&crate::analysis::control::Control::stoppable()).unwrap();
    let finalized = Arc::new(AtomicBool::new(false));
    let called = finalized.clone();
    wrote.then = Some(Box::new(move |_, _| {
        called.store(true, Ordering::Relaxed);
        Ok(())
    }));
    s.run("project.new", &json!({})).unwrap();
    let mark = s.notice_mark();
    let error = exports::export_land(&mut s, started, Ok(wrote)).unwrap_err();
    assert!(error.contains("completion was refused"), "{error}");
    assert!(error.contains("1 field name was changed") && error.contains("long_attribute_name"), "{error}");
    assert!(!finalized.load(Ordering::Relaxed), "refused delivery never runs a finalizer");
    assert!(s.map().layers.is_empty());
    assert!(s.notices_since(mark).iter().any(|n| n.error && n.text.contains("1 field name was changed")));
    let (back, _) = rustgis_data::io::read_path(&path).unwrap();
    assert_eq!(back.fields[0].name, "long_attri");
}

#[test]
fn write_notices_keep_geopackage_date_losses_after_a_failed_script() {
    let dir = Files::new();
    let path = dir.path().join("places.gpkg");
    let mut s = places();
    s.run("edit.addField", &json!({"layer":"Places","name":"at","type":"datetime"})).unwrap();
    s.run("edit.setValue", &json!({"layer":"Places","row":0,"field":"at","value":"2026-01-01T00:00:00Z"})).unwrap();
    s.run("share.exportLayer", &json!({"layer":"Places","path":path,"table":"places"})).unwrap();
    s.run("gpkg.addLayer", &json!({"path":path,"table":"places","name":"Editable"})).unwrap();
    s.run("edit.setValue", &json!({"layer":"Editable","row":0,"field":"at","value":"2026-01-01T00:00:00.123456+02:00"})).unwrap();
    let mark = s.notice_mark();
    assert!(
        s.run("script.run", &json!({"script":"gpkg.saveEdits {\"layer\":\"Editable\"}\nlayer.remove {\"layer\":\"missing\"}"})).is_err()
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let at: String = db.query_row("SELECT at FROM places", [], |r| r.get(0)).unwrap();
    assert_eq!(at, "2025-12-31T22:00:00.123Z");
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said.iter().filter(|t| t.contains("rounded to the millisecond")).count(), 1, "{said:?}");
    assert_eq!(said.iter().filter(|t| t.contains("written in UTC")).count(), 1, "{said:?}");
}

#[test]
fn write_notices_do_not_retain_a_failed_exports_preparation_or_prior_edits() {
    let dir = Files::new();
    let path = dir.path().join("missing").join("export.shp");
    let mut s = places();
    let before = s.notice_mark();
    let body = format!(
        "edit.deleteField {{\"layer\":\"Places\",\"field\":\"long_attribute_name\"}}\nshare.exportLayer {}",
        json!({"layer":"Places","path":path})
    );
    assert!(s.run("script.run", &json!({"script":body})).is_err());
    assert!(!path.exists());
    assert!(s.notices_since(before).iter().all(|n| n.error));
    assert!(s.map().layer_by_name("Places").unwrap().dataset.field_index("long_attribute_name").is_some());
}

#[test]
fn write_notices_keep_the_ordinary_queue_bounded_and_marks_consistent() {
    let mut s = places();
    let mark = s.notice_mark();
    assert!(
        s.guarded("Outer", true, |s| {
            s.notify("rolled-back edit", false);
            s.guarded("Inner", true, |s| {
                for i in 0..250 {
                    s.notify_written(format!("committed {i}"), false);
                }
                Err("inner failed".into())
            })?;
            Ok(Value::Null)
        })
        .is_err()
    );
    assert_eq!(s.notices.len(), 200);
    assert!(s.notices_since(mark).iter().filter(|n| !n.error).all(|n| n.text.starts_with("committed ")));
    let mark = s.notice_mark();
    s.notify("next", false);
    assert_eq!(s.notices_since(mark).len(), 1);
    assert_eq!(s.notices_since(mark)[0].text, "next");
}

#[test]
fn write_notices_keep_drawn_losses_when_background_delivery_is_refused() {
    use crate::{
        cmd::exports,
        policy::{AgentAccess, Effects, Revocation, Scope},
    };
    use rustgis_data::diagnostics::Severity;
    use std::sync::Arc;
    let dir = Files::new();
    for refused in [false, true] {
        let path = dir.path().join(if refused { "refused.png" } else { "delivered.png" });
        let mut s = super::with_raster();
        s.run("layer.zoomTo", &json!({"layer":"scene"})).unwrap();
        s.run("raster.symbology", &json!({"type":"shadedRelief","shadow":1})).unwrap();
        let rl = s.map_mut().layers.iter_mut().find_map(|l| l.raster.as_mut()).unwrap();
        rl.in_place = true;
        let mut raster = (**rl.data.as_ref().unwrap()).clone();
        raster.overview = Some((800, 600));
        rl.data = Some(Arc::new(raster));
        struct Live;
        impl Revocation for Live {
            fn allows(&self, _: Effects) -> bool {
                true
            }
        }
        let scope = Scope::agent(AgentAccess::Allow, false, "write-notices-drawing", None, Arc::new(Live));
        let run = s
            .with_scope(scope, |s| exports::export_start(s, "share.exportMap", &json!({"path":path,"width":200,"height":150})))
            .unwrap()
            .unwrap();
        let started = run.started();
        let wrote = run.run(&crate::analysis::control::Control::stoppable()).unwrap();
        assert!(path.is_file());
        assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
        if refused {
            s.run("project.new", &json!({})).unwrap();
        }
        let mark = s.notice_mark();
        let result = exports::export_land(&mut s, started, Ok(wrote));
        let notices = s.notices_since(mark);
        let losses: Vec<_> = notices.iter().filter(|n| n.text.contains("scene: its cast shadows aren't drawn")).collect();
        assert_eq!(losses.len(), 1, "{refused}: {notices:?}");
        if refused {
            let error = result.unwrap_err();
            assert!(error.contains("completion was refused") && error.contains("scene: its cast shadows aren't drawn"), "{error}");
            assert!(s.map().layers.is_empty());
        } else {
            result.unwrap();
            assert_eq!(losses[0].severity, Severity::Warning);
        }
    }
}

#[test]
fn write_notices_withdraw_rolled_back_geopackage_layer_refreshes() {
    for fails in [false, true] {
        let dir = Files::new();
        let path = dir.path().join("tables.gpkg");
        let mut s = super::world();
        s.run("share.exportLayer", &json!({"layer":"Countries","path":path})).unwrap();
        s.run("layer.addData", &json!({"path":path,"layer":"Countries","stream":true,"name":"InPlace"})).unwrap();
        s.map_mut().layers.iter_mut().for_each(|l| {
            l.sync_source(&rustgis_geom::Extent::new(-20.0, -20.0, 20.0, 20.0), true);
        });
        let kind = |s: &Session| s.map().layer_by_name("InPlace").unwrap().source.as_ref().unwrap().info().unwrap().geometry_type;
        assert_eq!(kind(&s), GeometryType::Polygon);
        let mark = s.notice_mark();
        let export = json!({"layer":"Cities","path":path,"mode":"replaceTable","tableName":"Countries"});
        let result = if fails {
            s.run("script.run", &json!({"script":format!("share.exportLayer {export}\nlayer.remove {{\"layer\":\"missing\"}}") }))
        } else {
            s.run("share.exportLayer", &export)
        };
        assert_eq!(result.is_err(), fails, "{result:?}");
        let notices = s.notices_since(mark);
        assert_eq!(notices.iter().filter(|n| n.text.contains("replaced its table Countries, dropping")).count(), 1, "{notices:?}");
        assert_eq!(notices.iter().filter(|n| n.text.contains("in place, now the new table")).count(), usize::from(!fails), "{notices:?}");
        assert_eq!(kind(&s), if fails { GeometryType::Polygon } else { GeometryType::Point });
        assert_eq!(rustgis_data::source::open_path(&path, Some("Countries")).unwrap().info().geometry_type, GeometryType::Point);
    }
}

/// A later script failure rolls the session back, but the committed CSV and its translated
/// omission notice must still describe the rows written to disk.
#[test]
fn write_notices_keep_table_limit_after_a_failed_script() {
    let dir = Files::new();
    let path = dir.path().join("top.csv");
    let mut s = Session::new(Project::default());
    s.run("layer.addDataText", &json!({"fileName":"ranks.csv","text":"number\n1\n2\n"})).unwrap();
    let export = json!({"table":"ranks","path":path,"limit":1});
    let script = format!("share.exportTable {export}\nlayer.remove {{\"layer\":\"missing\"}}");
    let mark = s.notice_mark();
    assert!(s.run("script.run", &json!({"script":script})).is_err());
    let csv = std::fs::read_to_string(&path).unwrap();
    assert_eq!(csv.lines().collect::<Vec<_>>(), ["number", "1"]);
    let notices = s.notices_since(mark);
    assert_eq!(notices.iter().filter(|n| n.text.contains("1 row(s) left out by the requested limit")).count(), 1, "{notices:?}");
    let loss = notices.iter().find(|n| n.text.contains("left out by the requested limit")).unwrap();
    assert!(loss.text.contains(&path.display().to_string()));
    assert!(loss.translated(|key, _| format!("translated:{key}")).unwrap().starts_with("translated:"));
    assert_eq!(s.project.tables.iter().find(|t| t.dataset.name == "ranks").unwrap().dataset.features.len(), 2);
}

#[test]
fn write_notices_keep_table_and_null_losses_in_refused_receipts() {
    use crate::{
        cmd::exports,
        policy::{AgentAccess, Effects, Revocation, Scope},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Live;
    impl Revocation for Live {
        fn allows(&self, _: Effects) -> bool {
            true
        }
    }
    for mode in ["table", "layer", "convert", "limitedTable"] {
        for refused in [false, true] {
            let dir = Files::new();
            let mut s = places();
            let path = dir.path().join(if matches!(mode, "table" | "limitedTable") { "places.gpkg" } else { "places.geojson" });
            let (command, params, loss) = if mode == "limitedTable" {
                s.run("layer.addDataText", &json!({"fileName":"ranks.csv","text":"number\n1\n2\n"})).unwrap();
                let table = s.project.tables.iter_mut().find(|t| t.dataset.name == "ranks").unwrap();
                table.dataset.fields[0].ty = FieldType::Double;
                table.dataset.features[0].attrs[0] = Cell::Float(f64::NAN);
                s.run("share.exportTable", &json!({"table":"ranks","path":path})).unwrap();
                let db = rusqlite::Connection::open(&path).unwrap();
                db.execute("CREATE INDEX old_index ON ranks(number)", []).unwrap();
                (
                    "share.exportTable",
                    json!({"table":"ranks","path":path,"mode":"replaceTable","tableName":"ranks","limit":1}),
                    "replaced its table ranks, dropping",
                )
            } else if mode == "table" {
                s.run("share.exportLayer", &json!({"layer":"Places","path":path})).unwrap();
                let db = rusqlite::Connection::open(&path).unwrap();
                db.execute("CREATE INDEX old_index ON Places(long_attribute_name)", []).unwrap();
                (
                    "share.exportLayer",
                    json!({"layer":"Places","path":path,"mode":"replaceTable","tableName":"Places"}),
                    "replaced its table Places, dropping",
                )
            } else {
                let layer = s.map_mut().layers.iter_mut().find(|l| l.name == "Places").unwrap();
                let field = layer.dataset.ensure_field("number", FieldType::Double);
                layer.dataset.features[0].attrs[field] = Cell::Float(f64::NAN);
                (
                    if mode == "convert" { "data.convert" } else { "share.exportLayer" },
                    json!({"layer":"Places","path":path,"format":"geojson"}),
                    "were written as null",
                )
            };
            let scope = Scope::agent(AgentAccess::Allow, false, "write-notices-receipt", None, Arc::new(Live));
            let run = s.with_scope(scope, |s| exports::export_start(s, command, &params)).unwrap().unwrap();
            let started = run.started();
            let mut wrote = run.run(&crate::analysis::control::Control::stoppable()).unwrap();
            let finalized = Arc::new(AtomicBool::new(false));
            let called = finalized.clone();
            let then = wrote.then.take();
            wrote.then = Some(Box::new(move |s, out| {
                called.store(true, Ordering::Relaxed);
                if let Some(then) = then {
                    then(s, out)?;
                }
                Ok(())
            }));
            if refused {
                s.run("project.new", &json!({})).unwrap();
            }
            let mark = s.notice_mark();
            let result = exports::export_land(&mut s, started, Ok(wrote));
            assert_eq!(result.is_err(), refused, "{mode}: {result:?}");
            let notices = s.notices_since(mark);
            assert_eq!(notices.iter().filter(|n| n.text.contains(loss)).count(), 1, "{mode}/{refused}: {notices:?}");
            assert_eq!(finalized.load(Ordering::Relaxed), !refused, "{mode}: finalizer after refusal");
            if mode == "limitedTable" {
                for extra in ["1 row(s) left out by the requested limit", "1 attribute value(s) that aren't finite numbers"] {
                    assert_eq!(notices.iter().filter(|n| n.text.contains(extra)).count(), 1, "{refused}: {notices:?}");
                    if refused {
                        assert!(result.as_ref().unwrap_err().contains(extra));
                    }
                }
                let db = rusqlite::Connection::open(&path).unwrap();
                let count: i64 = db.query_row("SELECT count(*) FROM ranks WHERE number IS NULL", [], |r| r.get(0)).unwrap();
                assert_eq!(count, 1);
            }
            if refused {
                assert!(result.unwrap_err().contains(loss));
            }
            if matches!(mode, "table" | "limitedTable") {
                let db = rusqlite::Connection::open(&path).unwrap();
                let indexes: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE name='old_index'", [], |r| r.get(0)).unwrap();
                assert_eq!(indexes, 0);
            } else {
                let text = std::fs::read_to_string(&path).unwrap();
                let back: Value = serde_json::from_str(&text).unwrap();
                assert!(back["features"][0]["properties"]["number"].is_null());
                if !refused {
                    let n = notices.iter().find(|n| n.text.contains(loss)).unwrap();
                    assert!(n.translated(|key, _| format!("translated:{key}")).unwrap().starts_with("translated:"));
                }
            }
        }
    }
}
