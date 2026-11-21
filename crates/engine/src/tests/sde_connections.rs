//! `connection.readSde`: what an ArcGIS Pro `.sde` connection file says, read without saving
//! anything and without its password.

use base64::Engine;

use super::*;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/testdata/sde").join(name)
}

fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `KEY=value;` pairs, as text.
fn read(s: &mut Session, pairs: &str) -> Result<Value, String> {
    s.run("connection.readSde", &json!({"data": base64(pairs.as_bytes())}))
}

#[test]
fn an_sde_file_is_read_without_saving_a_connection() {
    let mut s = Session::new(Project::default());
    let saved_before = s.connections.to_json();
    let path = fixture("sqlserver-os.sde");
    let out = s.run("connection.readSde", &json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(out["kind"], "sqlserver");
    assert_eq!(out["server"], "gisdb.example.invalid");
    assert_eq!(out["database"], "gis");
    assert_eq!(out["user"], Value::Null);
    assert_eq!(out["authentication"], "os");
    assert_eq!(out["address"], "mssql://gisdb.example.invalid/gis");
    assert_eq!(s.connections.to_json(), saved_before, "reading a file saves nothing");

    // The same file as bytes, as the web app and a caller without the file system pass it.
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(s.run("connection.readSde", &json!({"data": base64(&bytes)})).unwrap(), out);

    // The answer makes a connection when the caller passes it on.
    let saved = s.run("connection.new", &json!({"kind": "sqlserver", "address": out["address"], "name": "City"})).unwrap();
    assert_eq!(saved["address"], "mssql://gisdb.example.invalid/gis");
}

#[test]
fn an_sde_file_with_a_database_login_never_shows_its_password() {
    let mut s = Session::new(Project::default());
    let out = read(
        &mut s,
        "ENCRYPTED_PASSWORD=00ffRGSYNTHa1;SERVER=gis-db;INSTANCE=sde:sqlserver:gis-db;DATABASE=utility;USER=editor;PASSWORD=RGSYNTHb2;VERSION=sde.DEFAULT",
    )
    .unwrap();
    assert_eq!(out["user"], "editor");
    assert_eq!(out["authentication"], "database");
    assert_eq!(out["address"], "mssql://editor@gis-db/utility");
    let said: String = s.notices.iter().map(|n| n.text.as_str()).collect();
    for text in [out.to_string(), said] {
        assert!(!text.contains("RGSYNTH") && !text.to_lowercase().contains("password"), "{text}");
    }
}

#[test]
fn a_version_other_than_default_is_said_once_read() {
    let mut s = Session::new(Project::default());
    let out = read(&mut s, "SERVER=gis-db;INSTANCE=sde:sqlserver:gis-db;DATABASE=utility;AUTHENTICATION_MODE=OSA;VERSION=editor.Design 2")
        .unwrap();
    assert_eq!(out["version"], "editor.Design 2");
    let notice = s.notices.last().map(|n| n.text.clone()).unwrap_or_default();
    assert!(notice.contains("editor.Design 2") && notice.contains("Default version"), "{notice}");

    // Default is what is read: nothing to say.
    let before = s.notices.len();
    let out =
        read(&mut s, "SERVER=gis-db;INSTANCE=sde:sqlserver:gis-db;DATABASE=utility;AUTHENTICATION_MODE=OSA;VERSION=sde.DEFAULT").unwrap();
    assert_eq!(out["version"], "sde.DEFAULT");
    assert_eq!(s.notices.len(), before);
}

#[test]
fn an_sde_file_for_another_platform_is_refused_by_name() {
    let mut s = Session::new(Project::default());
    let e = s.run("connection.readSde", &json!({"path": fixture("postgres-os.sde").to_string_lossy()})).unwrap_err();
    assert!(e.contains("postgres-os.sde") && e.contains("PostgreSQL"), "{e}");
}

#[test]
fn a_broken_sde_file_is_an_error() {
    let mut s = Session::new(Project::default());
    let saved_before = s.connections.to_json();
    for p in [
        json!({}),
        json!({"data": "not base64 !"}),
        json!({"data": base64(&[0xff; 4096])}),
        json!({"data": base64(b"DATABASE=utility;USER=editor")}),
        json!({"data": "A".repeat((1 << 20) / 3 * 4 + 8)}),
        json!({"path": fixture("missing.sde").to_string_lossy()}),
    ] {
        let short: String = p.to_string().chars().take(60).collect();
        assert!(s.run("connection.readSde", &p).is_err(), "{short}");
    }
    assert_eq!(s.connections.to_json(), saved_before);
}

/// `RUSTGIS_TEST_SDE` names a live SQL Server enterprise geodatabase: its listing has feature
/// classes, a class opens as a layer named as the tree shows it (its alias, else its short name),
/// and a registered table opens as a standalone table.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "needs a live SQL Server enterprise geodatabase in RUSTGIS_TEST_SDE"]
fn sde_live_classes_and_tables() {
    let Ok(address) = std::env::var("RUSTGIS_TEST_SDE") else { return };
    let mut s = Session::new(Project::default());
    let listed = s.run("info.databaseTables", &json!({"database": address})).unwrap();
    let items = listed["items"].as_array().expect("a geodatabase lists items");
    let classes: Vec<&Value> = items.iter().filter(|i| i["item"] == "featureClass" && i["column"].is_string()).collect();
    assert!(!classes.is_empty(), "{listed}");
    // A class is named as the tree shows it: by its alias, else by its short name.
    let class = classes.iter().find(|c| c["alias"].is_string()).unwrap_or(&classes[0]);
    let expected = class["display"].as_str();
    let label = class["label"].as_str().unwrap();
    let r = s.run("layer.addDatabase", &json!({"database": address, "layer": label})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert_eq!(Some(s.map().layer(id).unwrap().name.as_str()), expected, "{r}");
    let tables: Vec<&str> = items.iter().filter(|i| i["item"] == "table").filter_map(|i| i["label"].as_str()).collect();
    assert!(!tables.is_empty(), "the test geodatabase registers a table");
    let before = s.project.tables.len();
    let r = s.run("layer.addDatabase", &json!({"database": address, "layer": tables[0]})).unwrap();
    assert_eq!(r["tables"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["layers"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(s.project.tables.len(), before + 1);
    let text = serde_json::to_string(&s.project).unwrap();
    let password = rustgis_data::source::msconn::split_password(&address).and_then(|(_, p)| p);
    assert!(password.is_none_or(|p| !text.contains(&p)), "the project keeps no password");
}
