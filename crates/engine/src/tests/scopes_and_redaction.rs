//! Regression tests for file isolation on byte/text imports, S3 session/scope boundaries,
//! DuckDB SQL secret stripping across durable saves/peers, and output URL/capability redaction.
use super::*;
use crate::policy::{AgentAccess, Call, Effect, Effects, Narrow, Outcome, Pending, Revocation, Scope, command_effects};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Live(AtomicBool);
impl Revocation for Live {
    fn allows(&self, _: Effects) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn fixture_marker(tag: &str) -> String {
    format!("fixture-{tag}-marker")
}

fn assistant_scope(id: u64) -> (Scope, Arc<Live>) {
    let live = Arc::new(Live(AtomicBool::new(true)));
    (Scope::assistant(id, live.clone()), live)
}

fn agent_scope(access: AgentAccess, label: &str) -> Scope {
    Scope::agent(access, false, label, None, Arc::new(Live(AtomicBool::new(true))))
}

fn pending(s: &Session, scope: &Scope, call: &Call) -> Pending {
    match s.review(scope, call) {
        Outcome::NeedsApproval(p) => p,
        Outcome::Refused(e) => panic!("refused instead of a review: {e}"),
        Outcome::Allowed => panic!("expected approval requirement"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn add_data_bytes_and_text_strip_paths_and_never_read_local_sidecars() {
    use base64::Engine;

    let dir = std::env::temp_dir().join(format!("rustgis-sidecar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut red = tiny_skia::Pixmap::new(8, 8).unwrap();
    red.fill(tiny_skia::Color::from_rgba8(220, 30, 30, 255));
    std::fs::write(dir.join("red.png"), red.encode_png().unwrap()).unwrap();
    std::fs::create_dir_all(dir.join("folder.png")).unwrap();

    let kml = r#"<kml xmlns="http://www.opengis.net/kml/2.2"><Document>
<Placemark><name>Pin</name><Point><coordinates>10.5,40.5</coordinates></Point></Placemark>
<GroundOverlay><name>SidecarRed</name><Icon><href>red.png</href></Icon><LatLonBox><north>41</north><south>40</south><east>11</east><west>10</west></LatLonBox></GroundOverlay>
<GroundOverlay><name>SidecarFolder</name><Icon><href>folder.png</href></Icon><LatLonBox><north>41</north><south>40</south><east>11</east><west>10</west></LatLonBox></GroundOverlay>
</Document></kml>"#;
    let kml_path = dir.join("doc.kml");
    std::fs::write(&kml_path, kml).unwrap();

    // Baseline: layer.addData reading a local file (Effect::Files) does read red.png beside doc.kml.
    let mut local = Session::new(Project::default());
    local.run("layer.addData", &json!({"path": kml_path.display().to_string()})).unwrap();
    assert!(local.map().layer_by_name("SidecarRed").is_some(), "local file addData should read sidecar");

    let b64 = base64::engine::general_purpose::STANDARD.encode(kml.as_bytes());
    let raw_name = kml_path.display().to_string();

    // Path-like fileName is refused at policy review for scopes without Effect::Files (Plugin(edit),
    // Agent(Refuse), Item), and even when Effect::Files IS held (User, approved Assistant,
    // Plugin(edit+files), Agent(Allow)), layer.addDataBytes strips directory components and NEVER
    // reads or probes local sidecars on disk.
    {
        let mut s = Session::new(Project::default());
        isolate_plugins();
        let params = json!({"fileName": raw_name, "data": b64});
        plugin("bytes-edit-only", &["edit"]);
        assert!(forward(&mut s, "bytes-edit-only", "layer.addDataBytes", params.clone()).is_err());
        // Plain leaf fileName works with just 'edit' and still never reads sidecars.
        forward(&mut s, "bytes-edit-only", "layer.addDataBytes", json!({"fileName": "doc.kml", "data": b64})).unwrap();
        assert!(s.map().layer_by_name("SidecarRed").is_none());
        rustgis_plugins::unload("bytes-edit-only");

        assert!(s.run_scoped(agent_scope(AgentAccess::Refuse, "mcp"), &Call::command("layer.addDataBytes", params.clone())).is_err());
        let item_err = s.narrowed(Narrow::Item { kind: "task", name: "item".into() }, |s| s.run("layer.addDataBytes", &params)).unwrap();
        assert!(item_err.is_err());
    }

    for mode in ["user", "assistant", "plugin-files", "mcp-allow"] {
        let mut s = Session::new(Project::default());
        isolate_plugins();
        let params = json!({"fileName": raw_name, "data": b64});
        let r = match mode {
            "user" => s.run("layer.addDataBytes", &params).unwrap(),
            "assistant" => {
                let (scope, _) = assistant_scope(19001);
                let call = Call::command("layer.addDataBytes", params);
                match s.review(&scope, &call) {
                    Outcome::NeedsApproval(p) => s.run_permitted(scope, p.allow(), &call).unwrap(),
                    Outcome::Allowed => s.run_scoped(scope, &call).unwrap(),
                    Outcome::Refused(e) => panic!("assistant refused: {e}"),
                }
            }
            "plugin-files" => {
                plugin("bytes-files", &["edit", "files"]);
                let out = forward(&mut s, "bytes-files", "layer.addDataBytes", params).unwrap();
                rustgis_plugins::unload("bytes-files");
                out
            }
            "mcp-allow" => {
                let scope = agent_scope(AgentAccess::Allow, "mcp-allow");
                s.run_scoped(scope, &Call::command("layer.addDataBytes", params)).unwrap()
            }
            _ => panic!("unexpected mode: {mode}"),
        };
        assert!(s.map().layer_by_name("SidecarRed").is_none(), "{mode}: sidecar raster must not be read: {r}");
        assert!(
            s.map().layers.iter().all(|l| l.dataset.source.as_deref() != Some(raw_name.as_str())),
            "{mode}: directory path must be stripped from dataset.source"
        );
        // Must not probe folder.png on disk ("isn't a file" only happens when disk metadata is inspected).
        assert!(
            !s.notices.iter().any(|n| n.text.contains("isn't a file")),
            "{mode}: local disk must not be probed for overlays: {:?}",
            s.notices
        );
    }

    // layer.addDataText with a path-shaped fileName also strips directories and never reads sidecars.
    let mut text_session = Session::new(Project::default());
    text_session.run("layer.addDataText", &json!({"fileName": raw_name, "format": "kml", "text": kml})).unwrap();
    assert!(text_session.map().layer_by_name("SidecarRed").is_none());
    assert!(!text_session.notices.iter().any(|n| n.text.contains("isn't a file")));

    // add_named_bytes (used by URL downloads) with a path-shaped name also never reads local sidecars.
    let mut dl_session = Session::new(Project::default());
    crate::cmd::raster::add_named_bytes(&mut dl_session, &json!({}), raw_name, kml.as_bytes().to_vec(), None).unwrap();
    assert!(dl_session.map().layer_by_name("SidecarRed").is_none());

    // Directory-only or empty fileNames are rejected cleanly.
    let mut bad_session = Session::new(Project::default());
    for bad_name in ["", "   ", "/", "nested/dir/", r"C:\temp\dir\"] {
        assert!(
            bad_session.run("layer.addDataBytes", &json!({"fileName": bad_name, "data": b64})).is_err(),
            "expected error for fileName {bad_name:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn jpeg2000_bytes_require_files_authority() {
    isolate_plugins();
    let mut s = Session::new(Project::default());

    for clean in ["scene.tif", "doc.kml", "points.geojson"] {
        let p = json!({"fileName": clean, "data": ""});
        let effects = command_effects(s.registry.get("layer.addDataBytes").unwrap(), &p);
        assert!(!effects.contains(Effect::Files), "{clean} must not carry Effect::Files: {effects:?}");
    }

    for jp2 in ["scene.jp2", "nested/dir/scene.j2k", r"C:\imagery\TILE.JP2"] {
        let p = json!({"fileName": jp2, "data": "AAAA"});
        let effects = command_effects(s.registry.get("layer.addDataBytes").unwrap(), &p);
        assert!(effects.contains(Effect::Files), "{jp2} must require Effect::Files");

        let call = Call::command("layer.addDataBytes", p.clone());
        let (assistant, _) = assistant_scope(19010);
        let _ = pending(&s, &assistant, &call);
        assert!(s.run_scoped(assistant, &call).is_err(), "{jp2}: unapproved assistant must be refused");

        let mcp_refuse = agent_scope(AgentAccess::Refuse, "jp2-mcp");
        assert!(matches!(s.review(&mcp_refuse, &call), Outcome::Refused(_)));
        assert!(s.run_scoped(mcp_refuse, &call).is_err());

        let item_res = s.narrowed(Narrow::Item { kind: "task", name: "jp2-item".into() }, |s| s.run("layer.addDataBytes", &p)).unwrap();
        assert!(item_res.is_err(), "{jp2}: imported item must be refused");

        plugin("jp2-edit", &["edit"]);
        let err = forward(&mut s, "jp2-edit", "layer.addDataBytes", p).unwrap_err();
        rustgis_plugins::unload("jp2-edit");
        assert!(err.contains("'files' permission"), "{jp2}: {err}");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn s3_session_and_scope_isolation_and_plain_urls() {
    if isolate_process_test("tests::scopes_and_redaction::s3_session_and_scope_isolation_and_plain_urls") {
        return;
    }
    use crate::render::rustgis_tiles::s3;
    use rustgis_data::secrets::MemorySecrets;

    let (base, _seen) = serve_s3();
    let mut a = Session::new(Project::default());
    a.database_logins.set_store(Some(Arc::new(MemorySecrets::new())));
    let conn_id = a
        .run(
            "connection.new",
            &json!({
                "kind": "s3",
                "name": "SavedS3",
                "address": base,
                "options": {"credentials": "keys", "buckets": "s3-test-bucket", "region": "us-east-1"}
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.run(
        "connection.login",
        &json!({
            "id": conn_id,
            "accessKeyId": fixture_marker("saved-id"),
            "secretAccessKey": fixture_marker("saved-s3"),
            "remember": true
        }),
    )
    .unwrap();
    let binding = a.s3_binding(&conn_id).unwrap().to_string();

    // Also add an in-memory signed S3 connection in process state through Session `a`.
    a.run(
        "s3.connect",
        &json!({
            "name": "manual-signed-s3",
            "endpoint": base,
            "buckets": ["s3-test-bucket"],
            "accessKeyId": fixture_marker("manual-id"),
            "secretAccessKey": fixture_marker("manual-s3")
        }),
    )
    .unwrap();

    // Another Session `b` does NOT own Session `a`'s saved S3 binding:
    let mut b = Session::new(Project::default());
    let b_conns = b.run("s3.connections", &Value::Null).unwrap();
    let b_text = b_conns.to_string();
    assert!(!b_text.contains("SavedS3") && !b_text.contains(&conn_id) && !b_text.contains(&binding), "{b_text}");
    assert!(b.run("s3.list", &json!({"connection": "SavedS3"})).is_err());
    assert!(b.run("s3.list", &json!({"connection": conn_id, "bucket": "extra-bucket"})).is_err());
    assert!(
        !s3::saved_connection(&binding).unwrap().0.buckets.iter().any(|bk| bk == "extra-bucket"),
        "Session b must not grant extra buckets on Session a's saved binding"
    );
    assert!(b.run("s3.disconnect", &json!({"name": "SavedS3"})).is_err());
    assert!(b.run("s3.disconnect", &json!({"name": conn_id})).is_err());
    assert!(s3::saved_connection(&binding).is_some(), "Session a's saved binding must remain active");

    // Non-User scopes on Session `a`:
    // 1. s3.connections hides saved and signed connections.
    plugin("s3-net", &["edit", "network"]);
    let plug_conns = forward(&mut a, "s3-net", "s3.connections", json!({})).unwrap();
    assert_eq!(plug_conns["connections"], json!([]), "{plug_conns}");

    let (assistant, _) = assistant_scope(19020);
    let asst_conns = a.run_scoped(assistant.clone(), &Call::command("s3.connections", json!({}))).unwrap();
    assert_eq!(asst_conns["connections"], json!([]), "{asst_conns}");

    let mcp_allow = agent_scope(AgentAccess::Allow, "mcp-allow");
    let mcp_conns = a.run_scoped(mcp_allow.clone(), &Call::command("s3.connections", json!({}))).unwrap();
    assert_eq!(mcp_conns["connections"], json!([]), "{mcp_conns}");

    let item_conns =
        a.narrowed(Narrow::Item { kind: "task", name: "item".into() }, |s| s.run("s3.connections", &json!({}))).unwrap().unwrap();
    assert_eq!(item_conns["connections"], json!([]), "{item_conns}");

    // 2. s3.list with "connection" or bucket-less listing requires Effect::Reserved.
    for reserved_params in [json!({"connection": "SavedS3"}), json!({"connection": "SavedS3", "bucket": "extra-bucket"}), json!({})] {
        let effects = command_effects(a.registry.get("s3.list").unwrap(), &reserved_params);
        assert!(effects.contains(Effect::Reserved), "{reserved_params} must carry Effect::Reserved");
        let call = Call::command("s3.list", reserved_params.clone());
        assert!(forward(&mut a, "s3-net", "s3.list", reserved_params.clone()).is_err());
        assert!(a.run_scoped(mcp_allow.clone(), &call).is_err());
        assert!(a.run_scoped(agent_scope(AgentAccess::Refuse, "mcp-refuse"), &call).is_err());
        let item_err = a.narrowed(Narrow::Item { kind: "task", name: "item".into() }, |s| s.run("s3.list", &reserved_params)).unwrap();
        assert!(item_err.is_err());
        let p = pending(&a, &assistant, &call);
        assert!(!p.review().remember, "Reserved S3 connection use must require per-call approval");
        assert!(a.run_scoped(assistant.clone(), &call).is_err());
    }
    assert!(
        !s3::saved_connection(&binding).unwrap().0.buckets.iter().any(|bk| bk == "extra-bucket"),
        "non-User callers must never mutate saved bucket grants"
    );
    assert!(forward(&mut a, "s3-net", "s3.disconnect", json!({"name": "SavedS3"})).is_err());
    rustgis_plugins::unload("s3-net");

    // 3. Authorized User call to s3.list strips rustgis-s3:<binding>@ from objects[].url.
    let listed = a.run("s3.list", &json!({"connection": "SavedS3", "bucket": "s3-test-bucket", "prefix": "data/"})).unwrap();
    let out_text = listed.to_string();
    assert_eq!(listed["objects"][0]["url"], "s3://s3-test-bucket/data/places.geojson");
    assert!(!out_text.contains("rustgis-s3:"), "capability leaked in s3.list: {out_text}");
    assert!(!out_text.contains(&binding), "binding token leaked in s3.list: {out_text}");
    assert!(!out_text.contains("fixture-"), "secret leaked in s3.list: {out_text}");
}

#[test]
fn duckdb_sql_and_add_sql_strip_secrets_across_saves_and_peers() {
    use crate::cmd::notebook::{Cell, CellKind, Notebook};
    use crate::project::{Task, TaskStep};
    use rustgis_data::{Dataset, Field, FieldType, GeometryType};
    use rustgis_map::Layer;
    use rustgis_map::source::{DuckdbOrigin, SourceLayer};

    let s3_val = fixture_marker("duck-s3");
    let s3_sec = s3_val.as_str();
    let set_val = fixture_marker("duck-set");
    let set_sec = set_val.as_str();
    let url_val = fixture_marker("duck-url");
    let url_sec = url_val.as_str();
    let pg_val = fixture_marker("duck-pg");
    let pg_sec = pg_val.as_str();
    let sql_val = fixture_marker("sql-in");
    let sql_pw = sql_val.as_str();
    let db_val = fixture_marker("sql-db");
    let db_url_sec = db_val.as_str();

    let lossy_sql = format!(
        "CREATE OR REPLACE PERSISTENT SECRET s1 (TYPE S3, KEY_ID 'TESTFIXTUREKEYID', SECRET '{s3_sec}'); \
         SET s3_secret_access_key = '{set_sec}'; \
         SELECT * FROM read_parquet('https://data.example.org/p.parquet?api_key={url_sec}&v=1') \
         JOIN postgres_scan('host=db.internal dbname=gis user=reader password={pg_sec}', 'public', 'roads') ON true"
    );

    let make_duck_layer = |name: &str, sql: &str| -> Layer {
        let ds = Dataset::new(name, GeometryType::Point, vec![Field::new("id", FieldType::Integer)]);
        let mut layer = Layer::new(1, ds);
        let mut src = SourceLayer::default();
        src.duckdb = Some(DuckdbOrigin {
            sql: Some(sql.into()),
            database: Some("warehouse.duckdb".into()),
            table: None,
            crs: None,
            family: rustgis_data::source::Family::Points,
        });
        layer.source = Some(src);
        layer
    };

    // Clean SQL produces zero credential notices and survives untouched.
    let clean_sql = "SELECT id, geom FROM roads WHERE kind = 'primary'";
    let mut clean_session = Session::new(Project::default());
    clean_session.map_mut().add_layer(make_duck_layer("CleanDuck", clean_sql));
    let (clean_text, clean_report) = clean_session.project.saved_text(None).unwrap();
    assert_eq!(clean_report.layer_addresses(), 0, "clean DuckDB SQL must not report stripped credentials");
    assert!(clean_text.contains(clean_sql));

    // Lossy SQL strips every secret across .rgis, .rgismap, .rgislyr, .rgispkg, .rgismpk, and peer_text.
    let mut s = Session::new(Project::default());
    s.map_mut().add_layer(make_duck_layer("LossyDuck", &lossy_sql));

    let assert_no_duck_secrets = |label: &str, text: &str| {
        for secret in [s3_sec, set_sec, url_sec, pg_sec, "TESTFIXTUREKEYID"] {
            assert!(!text.contains(secret), "{label} leaked {secret}: {text}");
        }
        assert!(text.contains("https://data.example.org/p.parquet?v=1"), "{label} lost clean URL query: {text}");
    };

    let (rgis_text, rgis_report) = s.project.saved_text(None).unwrap();
    assert!(rgis_report.layer_addresses() > 0, "lossy DuckDB SQL must report stripped credentials");
    assert_no_duck_secrets(".rgis", &rgis_text);

    // Identical-save restore preserves the live session's DuckDB SQL on immediate reopen
    // (Project::from_json), whereas import/cold reopen (Project::from_import_json or altered bytes)
    // and open_peer_text never rehydrate secrets.
    let imported = Project::from_import_json(&rgis_text, None).unwrap();
    let imported_sql =
        imported.map().layer_by_name("LossyDuck").unwrap().source.as_ref().unwrap().duckdb.as_ref().unwrap().sql.as_deref().unwrap();
    assert_no_duck_secrets("import .rgis", imported_sql);

    let cold = Project::from_json(&format!("{rgis_text}\n")).unwrap();
    let cold_sql = cold.map().layer_by_name("LossyDuck").unwrap().source.as_ref().unwrap().duckdb.as_ref().unwrap().sql.as_deref().unwrap();
    assert_no_duck_secrets("cold .rgis", cold_sql);

    let same_process = Project::from_json(&rgis_text).unwrap();
    let restored_sql =
        same_process.map().layer_by_name("LossyDuck").unwrap().source.as_ref().unwrap().duckdb.as_ref().unwrap().sql.as_deref().unwrap();
    assert_eq!(restored_sql, lossy_sql, "identical-save restore keeps in-memory live SQL");

    let (peer_json, peer_out_report) = s.peer_text(crate::Peer::Bridge).unwrap();
    assert!(peer_out_report.layer_addresses() > 0);
    assert_no_duck_secrets("peer_text", &peer_json);
    let mut peer_session = Session::new(Project::default());
    let peer_report = peer_session.open_peer_text(crate::Peer::Bridge, &peer_json).unwrap();
    assert_eq!(peer_report.reconnect, 1, "incoming DuckDB source requires explicit reconnect");
    let peer_sql =
        peer_session.map().layer_by_name("LossyDuck").unwrap().source.as_ref().unwrap().duckdb.as_ref().unwrap().sql.as_deref().unwrap();
    assert_no_duck_secrets("open_peer_text", peer_sql);

    // Replacing a session that held live DuckDB SQL secrets with Embedder peer text counts lost secrets.
    let mut replacing = Session::new(Project::default());
    replacing.map_mut().add_layer(make_duck_layer("LossyDuck", &lossy_sql));
    let embedder_report = replacing.open_peer_text(crate::Peer::Embedder, &peer_json).unwrap();
    assert!(embedder_report.lost() > 0, "replacing live DuckDB SQL secrets via Embedder must report lost");

    // .rgismap, .rgislyr, .rgispkg, and .rgismpk (both regular and mobile) via item_bytes:
    for kind in ["map", "layer", "projectPackage", "mapPackage", "mobilePackage"] {
        s.notices.clear();
        let (_, bytes) = crate::cmd::items::item_bytes(&mut s, kind, &json!({"layer": "LossyDuck"})).unwrap();
        let combined = if kind.ends_with("Package") {
            rustgis_data::zip::read(&bytes)
                .unwrap()
                .into_iter()
                .map(|(_, data)| String::from_utf8_lossy(&data).into_owned())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            String::from_utf8(bytes).unwrap()
        };
        assert_no_duck_secrets(kind, &combined);
        assert!(
            s.notices.iter().any(|n| n.text.contains("without their keys, tokens, passwords")),
            "{kind} must notify about stripped DuckDB SQL credentials: {:?}",
            s.notices
        );
    }

    // layer.addSql parameter roles (sql, query, database, password) strip secrets across tasks,
    // GP history, models, notebooks, and scripts.
    let add_sql_params = json!({
        "database": format!("postgresql://reader:{db_url_sec}@db.internal/gis"),
        "password": sql_pw,
        "sql": lossy_sql,
        "query": format!("SELECT * FROM read_parquet('https://data.example.org/q.parquet?token={url_sec}')")
    });
    assert!(crate::sources::holds_secret(&add_sql_params));
    assert!(!crate::sources::holds_secret(&json!({"database": "warehouse.duckdb", "sql": clean_sql})));

    let mut wf = Session::new(Project::default());
    wf.project.tasks.push(Task {
        name: "DuckTask".into(),
        description: String::new(),
        steps: vec![TaskStep {
            title: "Query".into(),
            instructions: String::new(),
            command: "layer.addSql".into(),
            params: add_sql_params.clone(),
        }],
    });
    wf.project.notebooks.push(Notebook {
        name: "DuckNb".into(),
        jupyter: None,
        cells: vec![Cell {
            kind: CellKind::Code,
            source: format!("layer.addSql {}\n", add_sql_params),
            output: None,
            error: false,
            jupyter: None,
        }],
    });
    wf.push_gp_record(crate::GpRecord {
        tool: "SQL Layer".into(),
        id: String::new(),
        params: Value::Null,
        command: Some(("layer.addSql".into(), add_sql_params)),
        at: 0,
        ok: true,
        messages: Vec::new(),
        millis: 1,
        outputs: Vec::new(),
    });

    let (wf_saved, wf_report) = wf.project.saved_text(None).unwrap();
    assert_ne!(wf_report, crate::durable::Report::default());
    for secret in [s3_sec, set_sec, url_sec, pg_sec, sql_pw, db_url_sec] {
        assert!(!wf_saved.contains(secret), "workflow save leaked {secret}: {wf_saved}");
    }
    let (_, task_bytes) = crate::cmd::items::item_bytes(&mut wf, "task", &json!({"task": "DuckTask"})).unwrap();
    let task_text = String::from_utf8(task_bytes).unwrap();
    for secret in [s3_sec, set_sec, url_sec, pg_sec, sql_pw, db_url_sec] {
        assert!(!task_text.contains(secret), ".rgistask leaked {secret}: {task_text}");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn outputs_redact_urls_capabilities_and_local_paths_across_scopes() {
    if isolate_process_test("tests::scopes_and_redaction::outputs_redact_urls_capabilities_and_local_paths_across_scopes") {
        return;
    }
    use rustgis_data::secrets::MemorySecrets;

    let hdr_val = fixture_marker("hdr");
    let hdr_sec = hdr_val.as_str();
    let q_val = fixture_marker("qry");
    let q_sec = q_val.as_str();
    let basic_val = fixture_marker("bsc");
    let basic_sec = basic_val.as_str();
    let leg_val = fixture_marker("leg");
    let leg_sec = leg_val.as_str();
    let terr_val = fixture_marker("ter");
    let terr_sec = terr_val.as_str();
    let wms_val = fixture_marker("wms");
    let wms_sec = wms_val.as_str();
    let rast_val = fixture_marker("rst");
    let rast_sec = rast_val.as_str();
    let svc_val = fixture_marker("svc");
    let svc_sec = svc_val.as_str();
    let s3_out_val = fixture_marker("s3out");
    let s3_sec = s3_out_val.as_str();

    let mut s = world();
    s.database_logins.set_store(Some(Arc::new(MemorySecrets::new())));

    // Saved web login + saved S3 login so we have real live rustgis-login: and rustgis-s3: wrappers.
    let web_id = s
        .run(
            "connection.new",
            &json!({"kind": "wms", "address": "https://sec-h04.example.org/wms", "auth": "header", "keyName": "X-Api-Key"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.run("connection.login", &json!({"id": web_id, "apiKey": hdr_sec})).unwrap();
    let wrapped_tile = crate::service_logins::request(&mut s, Some(&web_id), "https://sec-h04.example.org/tiles/{z}/{x}/{y}.png").unwrap();
    assert!(wrapped_tile.starts_with("rustgis-login:"));

    let s3_id = s
        .run(
            "connection.new",
            &json!({
                "kind": "s3",
                "name": "SavedBucketS3",
                "address": "https://s3-h04.example.org",
                "options": {"credentials": "keys", "buckets": "priv-bkt", "region": "us-east-1"}
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.run("connection.login", &json!({"id": s3_id, "accessKeyId": fixture_marker("out-id"), "secretAccessKey": s3_sec, "remember": true}))
        .unwrap();
    let s3_binding = s.s3_binding(&s3_id).unwrap().to_string();
    let wrapped_s3_raster = crate::render::rustgis_tiles::s3::bound_url(&s3_binding, "s3://priv-bkt/dem.tif").unwrap();
    assert!(wrapped_s3_raster.starts_with("rustgis-s3:"));

    // 1. Tile layers with query token, basic user:pass, legend token, capability wrapper, and dated time URL.
    let mut t_query = rustgis_map::TileLayer::new(
        "QueryTiles",
        rustgis_map::TileKind::Raster,
        format!("https://user:{basic_sec}@tiles.example.org/{{z}}/{{x}}/{{y}}.png?apikey={q_sec}&style=dark"),
    );
    let legend: rustgis_data::ogc::Legend =
        serde_json::from_value(json!({"url": format!("https://tiles.example.org/legend.png?token={leg_sec}&v=1")})).unwrap();
    t_query.legends.push(legend);
    t_query.time_url = Some(format!("https://tiles.example.org/wmts/{{time}}/{{z}}/{{x}}/{{y}}.png?apikey={q_sec}"));
    s.map_mut().tiles.push(t_query);

    let mut t_cap = rustgis_map::TileLayer::new("CapTiles", rustgis_map::TileKind::Raster, wrapped_tile.clone());
    t_cap.connection = web_id.clone();
    s.map_mut().tiles.push(t_cap);

    // Turn on time slider so time.info includes tiles[].url.
    s.map_mut().time = Some(rustgis_map::time::TimeSlider::spanning(
        rustgis_data::time::parse_input(&json!("2020")).unwrap(),
        rustgis_data::time::parse_input(&json!("2025")).unwrap(),
    ));

    // 2. WMS queryable layer with secret query parameter and capability wrapper.
    let wms_caps = format!(
        r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Sec WMS</Title></Service>
<Capability><Request>
<GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://sec-h04.example.org/wms?token={wms_sec}&amp;"/></Get></HTTP></DCPType></GetMap>
<GetFeatureInfo><Format>application/geo+json</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://sec-h04.example.org/info?token={wms_sec}&amp;"/></Get></HTTP></DCPType></GetFeatureInfo>
</Request>
<Layer queryable="1"><Title>Root</Title><CRS>EPSG:3857</CRS><Layer><Name>roads</Name><Title>SecRoads</Title></Layer></Layer>
</Capability></WMS_Capabilities>"#
    );
    s.run(
        "service.add",
        &json!({"url": format!("https://sec-h04.example.org/wms?token={wms_sec}"), "layer": "roads", "text": wms_caps, "connection": web_id}),
    )
    .unwrap();
    s.run("map.goToXY", &json!({"lon": 8.5, "lat": 47.4, "scale": 10000})).unwrap();

    // 3. Terrain with query api_key.
    s.map_mut().view.terrain = Some(rustgis_map::view::Terrain {
        source: rustgis_map::view::TerrainSource::Tiles {
            url: format!("https://terrain.example.org/{{z}}/{{x}}/{{y}}.png?api_key={terr_sec}&fmt=png"),
            encoding: rustgis_map::view::TerrainEncoding::default(),
        },
        exaggeration: 1.5,
        water: None,
    });

    // 4. Rasters: one local filesystem path, one URL with ?token=..., and one rustgis-s3: wrapper.
    let local_raster_path = "/tmp/private-user-dir/secret_elevation.tif";
    let make_raster_layer = |id: u32, name: &str, src: &str, in_place: bool| -> rustgis_map::Layer {
        let r = rustgis_data::raster::Raster::new(
            name,
            4,
            4,
            vec![vec![1.0; 16]],
            rustgis_data::raster::GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 },
            rustgis_data::raster::Crs::LonLat,
            None,
            rustgis_data::raster::ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let rl = if in_place { rustgis_map::RasterLayer::in_place(r, src) } else { rustgis_map::RasterLayer::from_path(r, src) };
        let mut l = rustgis_map::Layer::new_raster(id, name, rl);
        l.dataset.source = Some(src.to_string());
        l
    };
    s.map_mut().add_layer(make_raster_layer(901, "LocalDem", local_raster_path, false));
    s.map_mut().add_layer(make_raster_layer(902, "UrlDem", &format!("https://rasters.example.org/dem.tif?token={rast_sec}&band=1"), true));
    s.map_mut().add_layer(make_raster_layer(903, "S3Dem", &wrapped_s3_raster, true));

    let assert_clean_output = |label: &str, v: &Value| {
        let text = v.to_string();
        for forbidden in
            [hdr_sec, q_sec, basic_sec, leg_sec, terr_sec, wms_sec, rast_sec, svc_sec, s3_sec, "rustgis-login:", "rustgis-s3:", &s3_binding]
        {
            assert!(!text.contains(forbidden), "{label} leaked {forbidden:?} in {text}");
        }
    };

    // User scope checks: capability wrappers are stripped from map.tiles / map.setTiles / raster.info,
    // and raster.info / time.info / map.terrain redact URL secrets unconditionally, while local raster
    // path remains visible to User.
    let user_tiles = s.run("map.tiles", &Value::Null).unwrap();
    assert!(!user_tiles.to_string().contains("rustgis-login:") && !user_tiles.to_string().contains("rustgis-s3:"));
    let user_local_raster = s.run("raster.info", &json!({"layer": "LocalDem"})).unwrap();
    assert_eq!(user_local_raster["source"], local_raster_path);
    let user_url_raster = s.run("raster.info", &json!({"layer": "UrlDem"})).unwrap();
    assert_clean_output("user raster.info url", &user_url_raster);
    let user_s3_raster = s.run("raster.info", &json!({"layer": "S3Dem"})).unwrap();
    assert_clean_output("user raster.info s3", &user_s3_raster);
    assert_eq!(user_s3_raster["source"], "s3://priv-bkt/dem.tif");
    assert_eq!(crate::sources::redact_output("rustgis-s3:malformed-wrapper"), "[address omitted]");
    assert_eq!(crate::sources::redact_output("rustgis-login:malformed-wrapper"), "[address omitted]");
    assert_clean_output("user time.info", &s.run("time.info", &Value::Null).unwrap());
    assert_clean_output("user map.terrain", &s.run("map.terrain", &json!({"exaggeration": 1.5})).unwrap());

    // Service / catalogue fixtures carrying inline credentials:
    let wcs_caps = format!(
        r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0" xmlns:xlink="http://www.w3.org/1999/xlink">
        <ows:OperationsMetadata><ows:Operation name="GetCoverage"><ows:DCP><ows:HTTP><ows:Get xlink:href="https://wcs.example.org/wcs?api_key={svc_sec}&amp;"/></ows:HTTP></ows:DCP></ows:Operation></ows:OperationsMetadata>
        <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Soil</ows:Title>
          <ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
        </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#
    );
    let wcs_describe = r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
        <lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
    let csw_records = format!(
        r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/">
        <csw:SearchResults numberOfRecordsMatched="1" nextRecord="0"><csw:Record><dc:title>Forests</dc:title>
          <dc:URI protocol="OGC:WMS">https://geo.example.org/wms?token={svc_sec}&amp;v=1</dc:URI>
        </csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#
    );
    let ogc_group_doc = json!({
        "collections": [{
            "id": "roads",
            "title": "Roads",
            "links": [{"rel": "items", "type": "application/geo+json", "href": format!("https://sec-h04.example.org/items?api_key={svc_sec}&limit=10")}]
        }]
    })
    .to_string();

    // Non-User scopes: Plugin, Assistant, Agent(Refuse)/Agent(Allow), and Item.
    plugin("plug", &["edit", "network"]);
    let (assistant, _) = assistant_scope(19040);
    let mcp_refuse = agent_scope(AgentAccess::Refuse, "mcp-refuse");
    let mcp_allow = agent_scope(AgentAccess::Allow, "mcp-allow");

    // Effects::NONE commands across Plugin, Assistant, Agent(Refuse), and Item:
    let none_calls: Vec<(&str, Value)> = vec![
        ("map.tiles", Value::Null),
        ("map.setTiles", json!({"name": "QueryTiles", "opacity": 0.9})),
        ("time.info", Value::Null),
        ("raster.info", json!({"layer": "LocalDem"})),
        ("raster.info", json!({"layer": "UrlDem"})),
        ("raster.info", json!({"layer": "S3Dem"})),
        ("identify", json!({"lon": 8.5, "lat": 47.4, "mode": "visible"})),
    ];
    for (cmd, params) in none_calls {
        let plug_v = forward(&mut s, "plug", cmd, params.clone()).unwrap();
        assert_clean_output(&format!("plugin {cmd}"), &plug_v);
        let asst_v = s.run_scoped(assistant.clone(), &Call::command(cmd, params.clone())).unwrap();
        assert_clean_output(&format!("assistant {cmd}"), &asst_v);
        let mcp_v = s.run_scoped(mcp_refuse.clone(), &Call::command(cmd, params.clone())).unwrap();
        assert_clean_output(&format!("mcp {cmd}"), &mcp_v);
        let item_v = s.narrowed(Narrow::Item { kind: "task", name: "item".into() }, |s| s.run(cmd, &params)).unwrap().unwrap();
        assert_clean_output(&format!("item {cmd}"), &item_v);

        if cmd == "raster.info" && params["layer"] == "LocalDem" {
            for (who, v) in [("plugin", &plug_v), ("assistant", &asst_v), ("mcp", &mcp_v), ("item", &item_v)] {
                assert!(v["source"].is_null(), "{who} raster.info leaked local path: {v}");
                assert!(!v.to_string().contains(local_raster_path), "{who}: {v}");
            }
        }
    }

    // Network-bearing commands across Plugin(["network"]), Assistant (approved), and Agent(Allow):
    let net_calls: Vec<(&str, Value)> = vec![
        ("map.terrain", json!({"exaggeration": 1.5})),
        ("identify.featureInfo", json!({"layer": "SecRoads", "lon": 8.5, "lat": 47.4, "capabilities": wms_caps, "request": true})),
        ("service.layers", json!({"url": format!("https://wcs.example.org/wcs?api_key={svc_sec}"), "text": wcs_caps})),
        (
            "service.coverage",
            json!({"url": format!("https://wcs.example.org/wcs?api_key={svc_sec}"), "layer": "soc", "text": wcs_caps, "describe": wcs_describe, "bbox": "full"}),
        ),
        (
            "catalog.search",
            json!({"url": format!("https://cat.example.org/csw?api_key={svc_sec}"), "query": "forest", "text": csw_records}),
        ),
        (
            "service.groupPlan",
            json!({"url": format!("https://sec-h04.example.org/ogc?api_key={svc_sec}"), "type": "ogcapi", "text": ogc_group_doc}),
        ),
    ];
    for (cmd, params) in net_calls {
        let plug_v = forward(&mut s, "plug", cmd, params.clone()).unwrap();
        assert_clean_output(&format!("plugin {cmd}"), &plug_v);

        let call = Call::command(cmd, params.clone());
        let asst_v = match s.review(&assistant, &call) {
            Outcome::NeedsApproval(p) => s.run_permitted(assistant.clone(), p.allow(), &call).unwrap(),
            Outcome::Allowed => s.run_scoped(assistant.clone(), &call).unwrap(),
            Outcome::Refused(e) => panic!("assistant {cmd} refused: {e}"),
        };
        assert_clean_output(&format!("assistant {cmd}"), &asst_v);

        let mcp_v = s.run_scoped(mcp_allow.clone(), &call).unwrap();
        assert_clean_output(&format!("mcp-allow {cmd}"), &mcp_v);
    }

    rustgis_plugins::unload("plug");
}
