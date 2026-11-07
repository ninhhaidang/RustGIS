//! Saved S3 connections: keys kept in the credential store only on request, and sources read
//! through the exact login that was chosen for them.
//!
//! Each test uses names, buckets and a server of its own: the S3 login registry is shared by
//! every test of the process.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustgis_data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
use serde_json::{Value, json};

use crate::connections::Connections;
use crate::render::rustgis_tiles::{cache, s3};
use crate::{Project, Session, s3_keys};

/// A credential store that remembers what it was asked and can refuse to save or delete.
#[derive(Default)]
struct Spy {
    inner: MemorySecrets,
    ops: Mutex<Vec<String>>,
    refuse_save: AtomicBool,
    refuse_delete: AtomicBool,
}

impl Spy {
    fn ops(&self) -> Vec<String> {
        self.ops.lock().unwrap().clone()
    }

    fn count(&self, op: &str) -> usize {
        self.ops().iter().filter(|o| o.starts_with(op)).count()
    }

    fn raw(&self, id: &str) -> Option<String> {
        self.inner.load(&crate::connections::account(id)).unwrap().map(|s| s.expose().to_string())
    }
}

impl SecretStore for Spy {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        self.ops.lock().unwrap().push(format!("load {account}"));
        self.inner.load(account)
    }

    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
        self.ops.lock().unwrap().push(format!("save {account}"));
        if self.refuse_save.load(Ordering::Relaxed) {
            return Err(SecretError::Denied);
        }
        self.inner.save(account, value)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.ops.lock().unwrap().push(format!("delete {account}"));
        if self.refuse_delete.load(Ordering::Relaxed) {
            return Err(SecretError::Denied);
        }
        self.inner.delete(account)
    }
}

/// A stand-in S3 endpoint with one bucket (path-style): objects with byte ranges, a listing,
/// redirects, and a 404 that echoes the request's Authorization header back, as a hostile server
/// might. Every request's head is kept.
struct Server {
    base: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn requests(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// The access key ids that signed the requests after the first `from`.
    fn key_ids(&self, from: usize) -> BTreeSet<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .skip(from)
            .filter_map(|h| h.split("Credential=").nth(1).map(|c| c.split('/').next().unwrap_or("").to_string()))
            .collect()
    }

    /// How many requests after the first `from` carried no Authorization header.
    fn unsigned(&self, from: usize) -> usize {
        self.log.lock().unwrap().iter().skip(from).filter(|h| !h.to_ascii_lowercase().contains("authorization:")).count()
    }

    /// Whether any request carried `text`.
    fn saw(&self, text: &str) -> bool {
        self.log.lock().unwrap().iter().any(|h| h.contains(text))
    }
}

fn serve_bucket(bucket: &str, objects: Vec<(&str, Vec<u8>)>, redirects: Vec<(&str, String)>) -> Server {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new((
        bucket.to_string(),
        objects.into_iter().map(|(k, b)| (k.to_string(), b)).collect::<Vec<_>>(),
        redirects.into_iter().map(|(k, l)| (k.to_string(), l)).collect::<Vec<_>>(),
    ));
    let seen = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (state, seen) = (state.clone(), seen.clone());
            std::thread::spawn(move || answer(stream, &state.0, &state.1, &state.2, &seen));
        }
    });
    Server { base, log }
}

type Objects = [(String, Vec<u8>)];

fn answer(mut s: std::net::TcpStream, bucket: &str, objects: &Objects, redirects: &[(String, String)], seen: &Mutex<Vec<String>>) {
    let mut buf = [0u8; 8192];
    let mut head = Vec::new();
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let req = String::from_utf8_lossy(&head).to_string();
    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let lower = req.to_ascii_lowercase();
    let range = lower.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| {
        let (a, b) = r.trim().split_once('-')?;
        Some((a.parse::<usize>().ok(), b.parse::<usize>().ok()))
    });
    seen.lock().unwrap().push(req.clone());
    let bare = path.split('?').next().unwrap_or("");
    let key = bare.strip_prefix(&format!("/{bucket}/")).unwrap_or("");
    let auth = req.lines().find(|l| l.to_ascii_lowercase().starts_with("authorization:")).unwrap_or("").to_string();
    let mut extra = String::new();
    let (status, body): (String, Vec<u8>) = if bare == format!("/{bucket}") || (bare == format!("/{bucket}/") && path.contains("list-type"))
    {
        let contents: String =
            objects.iter().map(|(k, b)| format!("<Contents><Key>{k}</Key><Size>{}</Size></Contents>", b.len())).collect();
        (
            "200 OK".into(),
            format!("<ListBucketResult><Name>{bucket}</Name><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>").into_bytes(),
        )
    } else if bare == "/" {
        (
            "200 OK".into(),
            format!("<ListAllMyBucketsResult><Buckets><Bucket><Name>{bucket}</Name></Bucket></Buckets></ListAllMyBucketsResult>")
                .into_bytes(),
        )
    } else if let Some((_, location)) = redirects.iter().find(|(k, _)| k == key) {
        extra = format!("Location: {location}\r\n");
        ("301 Moved Permanently".into(), Vec::new())
    } else if let Some((_, whole)) = objects.iter().find(|(k, _)| k == key) {
        match range {
            Some((Some(a), Some(b))) if a < whole.len() => ("206 Partial Content".into(), whole[a..=b.min(whole.len() - 1)].to_vec()),
            Some((None, Some(n))) => {
                let n = n.min(whole.len());
                ("206 Partial Content".into(), whole[whole.len() - n..].to_vec())
            }
            Some(_) => ("416 Range Not Satisfiable".into(), Vec::new()),
            None => ("200 OK".into(), whole.clone()),
        }
    } else {
        ("404 Not Found".into(), format!("<Error><Code>NoSuchKey</Code><Message>{auth}</Message></Error>").into_bytes())
    };
    let _ = write!(s, "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = s.write_all(&body);
}

/// A FlatGeobuf of a grid of points (attribute `n`), big enough to be read in place by range.
fn grid_fgb() -> Vec<u8> {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
    let mut ds = Dataset::new("grid", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..200u32 {
        for j in 0..200u32 {
            let c = Coord::new(-179.0 + 1.79 * f64::from(i), -80.0 + 0.8 * f64::from(j));
            ds.push(Some(Geometry::Point(c)), vec![V::Int(i64::from(i * 200 + j))]);
        }
    }
    rustgis_data::source::fgb::write(&ds).unwrap()
}

/// A vector PMTiles archive: one park at zoom 0.
fn parks_pmtiles() -> Vec<u8> {
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
    pmtiles::write(vec![(0, 0, 0, tile)], &new).unwrap()
}

fn session(store: Option<&Arc<Spy>>) -> Session {
    let mut s = Session::new(Project::default());
    s.database_logins.set_store(store.map(|st| st.clone() as Arc<dyn SecretStore>));
    s
}

/// A saved S3 connection at `base` for `bucket`; its id.
fn saved(s: &mut Session, name: &str, base: &str, bucket: &str, credentials: &str) -> String {
    let r = s
        .run(
            "connection.new",
            &json!({"kind": "s3", "name": name, "address": base, "options": {"credentials": credentials, "buckets": bucket}}),
        )
        .unwrap();
    r["id"].as_str().unwrap().to_string()
}

fn login(s: &mut Session, id: &str, key: &str, secret: &str, remember: bool) -> Result<Value, String> {
    s.run("connection.login", &json!({"id": id, "accessKeyId": key, "secretAccessKey": secret, "remember": remember}))
}

fn live_key(name: &str) -> Option<String> {
    s3::connection(name).and_then(|c| c.credentials).map(|c| c.access_key_id)
}

/// Waits for a streamed layer's view to be read; how many features it holds.
fn read_view(s: &mut Session, id: u32) -> usize {
    let view = rustgis_geom::Extent::new(10.0, 10.0, 14.0, 13.0);
    let scale = s.map().scale();
    let mut pending = s.map_mut().sync_sources(&view, scale, false);
    let start = std::time::Instant::now();
    while pending && start.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(5));
        pending = s.map_mut().sync_sources(&view, scale, false);
    }
    s.map().layer(id).map_or(0, |l| l.dataset.features.len())
}

/// The layer whose streamed source reads `name` (the end of its address).
fn streamed(s: &Session, ends: &str) -> u32 {
    s.map().layers.iter().find(|l| l.source.as_ref().and_then(|x| x.path.as_deref()).is_some_and(|p| p.ends_with(ends))).unwrap().id
}

fn bound(s: &Session, id: &str, url: &str) -> String {
    s3::bound_url(s.s3_binding(id).unwrap(), url).unwrap()
}

#[test]
fn s3_saved_keys_are_remembered_only_on_request_and_reused_by_a_fresh_session() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_keys_are_remembered_only_on_request_and_reused_by_a_fresh_session") {
        return;
    }
    let srv = serve_bucket("sv-remember", vec![("grid.fgb", grid_fgb())], vec![]);
    let store = Arc::new(Spy::default());
    let mut a = session(Some(&store));
    let id = saved(&mut a, "sv-remember-conn", &srv.base, "sv-remember", "keys");
    assert_eq!(id, "c1");
    // Logging in is session-only unless asked: nothing is stored.
    let r = login(&mut a, &id, "SAVEDKEYA", "saved-secret-A", false).unwrap();
    assert_eq!((r["loggedIn"].as_bool(), r["savedKeys"].as_bool()), (Some(true), Some(false)), "{r}");
    assert!(store.raw(&id).is_none() && store.count("save") == 0);
    assert!(a.connections.to_json().find("savedKeys").is_none());
    // Remembered on request: both keys and the canonical endpoint, in the store only.
    let r = login(&mut a, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    assert_eq!((r["loggedIn"].as_bool(), r["savedKeys"].as_bool()), (Some(true), Some(true)), "{r}");
    assert!(r.get("warning").is_none() && !r.to_string().contains("saved-secret"), "{r}");
    let stored: Value = serde_json::from_str(&store.raw(&id).unwrap()).unwrap();
    assert_eq!(stored, json!({"endpoint": srv.base, "accessKeyId": "SAVEDKEYA", "secretAccessKey": "saved-secret-A"}));
    // The preference holds the marker, never a key; lists never ask the store.
    let prefs = a.connections.to_json();
    assert!(prefs.contains("\"savedKeys\":true") && !prefs.contains("SAVEDKEYA") && !prefs.contains("saved-secret"), "{prefs}");
    let loads = store.count("load");
    let list = a.run("connection.list", &json!({})).unwrap();
    assert_eq!(list["connections"][0]["savedKeys"], true);
    assert_eq!(store.count("load"), loads);
    // A restart: the definitions and the store survive, the login doesn't.
    s3::disconnect("sv-remember-conn");
    let mut b = session(Some(&store));
    b.connections = Connections::from_json(&prefs);
    assert!(b.connections.find("c1").is_some_and(|c| c.saved_keys));
    let r = b.run("connection.list", &json!({})).unwrap();
    assert_eq!(r["connections"][0]["loggedIn"], false);
    let r = b.run("connection.login", &json!({"id": id})).unwrap();
    assert_eq!((r["loggedIn"].as_bool(), r["savedKeys"].as_bool()), (Some(true), Some(true)), "{r}");
    // Two sessions' ids are alike (`c1`); their runtime tokens are not.
    assert_ne!(a.s3_binding(&id), b.s3_binding(&id));
    assert!(b.s3_binding(&id).is_some());
    // Browsed and added through the login: the server sees the saved access key id.
    let v = b.run("connection.browse", &json!({"id": id, "node": "s3:sv-remember/"})).unwrap();
    let add = v["children"].as_array().unwrap().iter().find(|c| c["name"] == "grid.fgb").unwrap()["add"].clone();
    assert!(add["params"]["url"].as_str().unwrap().starts_with("rustgis-s3:"), "{add}");
    let r = b.run(add["command"].as_str().unwrap(), &add["params"]).unwrap();
    assert_eq!((r["streamed"].as_bool(), r["features"].as_u64()), (Some(true), Some(40_000)), "{r}");
    assert_eq!(srv.key_ids(0), BTreeSet::from(["SAVEDKEYA".to_string()]));
    assert!(!srv.saw("saved-secret-A"), "no secret reaches the server in a header");
    assert_eq!(srv.unsigned(0), 0);
    s3::disconnect("sv-remember-conn");
}

#[test]
fn s3_saved_login_rules_sources_temporary_credentials_and_bad_values() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_login_rules_sources_temporary_credentials_and_bad_values") {
        return;
    }
    let srv = serve_bucket("sv-rules", vec![], vec![]);
    let store = Arc::new(Spy::default());
    let mut s = session(Some(&store));
    let id = saved(&mut s, "sv-rules-keys", &srv.base, "sv-rules", "keys");
    let typed = |extra: Value| {
        let mut p = json!({"id": id, "accessKeyId": "SAVEDKEYA", "secretAccessKey": "saved-secret-A"});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    // A remember that isn't true or false is refused before anything happens.
    for junk in [json!("yes"), json!(1), json!(null), json!([true]), json!({})] {
        let e = s.run("connection.login", &typed(json!({"remember": junk}))).unwrap_err();
        assert_eq!(e, "remember is true or false");
    }
    assert!(s3::connection("sv-rules-keys").is_none() && store.ops().is_empty());
    // It keeps a complete typed pair, nothing else: no half pair, no token alone, no stored fallback.
    let remember_only = s.run("connection.login", &json!({"id": id, "remember": true})).unwrap_err();
    assert!(remember_only.contains("remember needs"), "{remember_only}");
    for p in [json!({"id": id, "accessKeyId": "SAVEDKEYA", "remember": true}), json!({"id": id, "secretAccessKey": "saved-secret-A"})] {
        assert!(s.run("connection.login", &p).unwrap_err().contains("both accessKeyId and secretAccessKey"));
    }
    let token_only = s.run("connection.login", &json!({"id": id, "sessionToken": "tok"})).unwrap_err();
    assert!(token_only.contains("session token needs"), "{token_only}");
    assert!(s3::connection("sv-rules-keys").is_none() && store.ops().is_empty());
    // Not for sources that don't type keys, nor for other kinds.
    let none = saved(&mut s, "sv-rules-none", &srv.base, "sv-rules", "none");
    assert!(s.run("connection.login", &json!({"id": none, "accessKeyId": "K", "secretAccessKey": "S", "remember": true})).is_err());
    let wms = s.run("connection.new", &json!({"kind": "wms", "name": "sv-rules-wms", "address": "http://127.0.0.1:9/wms"})).unwrap();
    assert!(s.run("connection.login", &json!({"id": wms["id"], "remember": true})).is_err());
    assert!(s3::connection("sv-rules-none").is_none() && store.ops().is_empty());
    // A damaged value, and one for another endpoint, are refused in fixed words.
    store.inner.save(&crate::connections::account(&id), &Secret::new("junk with saved-secret-A")).unwrap();
    let e = s.run("connection.login", &json!({"id": id})).unwrap_err();
    assert_eq!(e, s3_keys::UNUSABLE);
    let other = json!({"endpoint": "http://127.0.0.1:9", "accessKeyId": "OTHERKEY", "secretAccessKey": "other-secret"}).to_string();
    store.inner.save(&crate::connections::account(&id), &Secret::new(other)).unwrap();
    let e = s.run("connection.login", &json!({"id": id})).unwrap_err();
    assert_eq!(e, s3_keys::UNUSABLE);
    assert!(!e.contains("OTHERKEY") && s3::connection("sv-rules-keys").is_none());
    // Nothing saved: said so, and a stale marker goes.
    store.inner.delete(&crate::connections::account(&id)).unwrap();
    s.connections.set_saved_keys(&id, true);
    let e = s.run("connection.login", &json!({"id": id})).unwrap_err();
    assert_eq!(e, s3_keys::NONE_SAVED);
    assert!(!s.connections.find(&id).unwrap().saved_keys);
    // Remembered keys, then typed ones for this session only (nothing written, the pair kept).
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    let saves = store.count("save");
    let r = login(&mut s, &id, "SAVEDKEYB", "saved-secret-B", false).unwrap();
    assert_eq!(r["savedKeys"], true);
    assert_eq!(live_key("sv-rules-keys").as_deref(), Some("SAVEDKEYB"));
    assert_eq!(store.count("save"), saves);
    assert!(store.raw(&id).unwrap().contains("SAVEDKEYA") && !store.raw(&id).unwrap().contains("SAVEDKEYB"));
    // ... and the stored pair again with nothing typed.
    s.run("connection.login", &json!({"id": id})).unwrap();
    assert_eq!(live_key("sv-rules-keys").as_deref(), Some("SAVEDKEYA"));
    // A temporary set (with a token) signs this session and writes nothing, not even half; what
    // was saved before stays.
    let before = store.raw(&id);
    let r = s
        .run(
            "connection.login",
            &typed(json!({"accessKeyId": "TEMPKEY", "secretAccessKey": "temp-secret", "sessionToken": "tok-1", "remember": true})),
        )
        .unwrap();
    assert_eq!(r["warning"], s3_keys::TEMPORARY, "{r}");
    assert_eq!((r["loggedIn"].as_bool(), r["savedKeys"].as_bool()), (Some(true), Some(true)));
    assert_eq!(store.raw(&id), before);
    let live = s3::connection("sv-rules-keys").unwrap().credentials.unwrap();
    assert_eq!((live.access_key_id.as_str(), live.session_token.as_deref()), ("TEMPKEY", Some("tok-1")));
    // The same for a connection with nothing saved: still nothing written.
    let fresh = saved(&mut s, "sv-rules-fresh", &srv.base, "sv-rules", "keys");
    let r = s
        .run(
            "connection.login",
            &json!({"id": fresh, "accessKeyId": "TEMPKEY", "secretAccessKey": "temp-secret", "sessionToken": "tok-1", "remember": true}),
        )
        .unwrap();
    assert_eq!((r["warning"].as_str(), r["savedKeys"].as_bool()), (Some(s3_keys::TEMPORARY), Some(false)));
    assert!(store.raw(&fresh).is_none());
    // A profile, the environment and anonymous access never read the store.
    let loads = store.count("load");
    let profile = s
        .run("connection.new", &json!({"kind": "s3", "name": "sv-rules-profile", "address": srv.base, "options": {"credentials": "profile", "profile": "sv-rules-no-such-profile"}}))
        .unwrap();
    let environment = saved(&mut s, "sv-rules-env", &srv.base, "sv-rules", "environment");
    for other in [none.as_str(), profile["id"].as_str().unwrap(), environment.as_str()] {
        let _ = s.run("connection.login", &json!({"id": other}));
    }
    assert_eq!(store.count("load"), loads, "{:?}", store.ops());
    for name in ["sv-rules-keys", "sv-rules-fresh", "sv-rules-none", "sv-rules-env"] {
        s3::disconnect(name);
    }
}

#[test]
fn s3_saved_missing_or_refusing_stores_keep_the_session_login() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_missing_or_refusing_stores_keep_the_session_login") {
        return;
    }
    let srv = serve_bucket("sv-stores", vec![], vec![]);
    // No store at all (the browser, the command line).
    let mut s = session(None);
    let id = saved(&mut s, "sv-stores-none", &srv.base, "sv-stores", "keys");
    let r = login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    assert_eq!((r["loggedIn"].as_bool(), r["savedKeys"].as_bool()), (Some(true), Some(false)), "{r}");
    assert_eq!(r["warning"], s3_keys::NOT_REMEMBERED);
    assert_eq!(live_key("sv-stores-none").as_deref(), Some("SAVEDKEYA"));
    s3::disconnect("sv-stores-none");
    assert_eq!(s.run("connection.login", &json!({"id": id})).unwrap_err(), s3_keys::UNAVAILABLE);
    assert!(s3::connection("sv-stores-none").is_none(), "no login without keys");
    // A store that refuses everything.
    let mut s = session(None);
    s.database_logins.set_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    let id = saved(&mut s, "sv-stores-denied", &srv.base, "sv-stores", "keys");
    let r = login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    assert_eq!((r["warning"].as_str(), r["loggedIn"].as_bool()), (Some(s3_keys::NOT_REMEMBERED), Some(true)));
    assert!(!r.to_string().contains("keychain"), "no backend text: {r}");
    s3::disconnect("sv-stores-denied");
    assert_eq!(s.run("connection.login", &json!({"id": id})).unwrap_err(), s3_keys::UNAVAILABLE);
    assert!(s3::connection("sv-stores-denied").is_none());
    // A save that fails doesn't make the new pair the saved one, nor erase the old.
    let spy = Arc::new(Spy::default());
    let mut s = session(Some(&spy));
    let id = saved(&mut s, "sv-stores-save", &srv.base, "sv-stores", "keys");
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    spy.refuse_save.store(true, Ordering::Relaxed);
    let r = login(&mut s, &id, "SAVEDKEYB", "saved-secret-B", true).unwrap();
    assert_eq!((r["warning"].as_str(), r["savedKeys"].as_bool()), (Some(s3_keys::NOT_REMEMBERED), Some(true)), "{r}");
    assert_eq!(live_key("sv-stores-save").as_deref(), Some("SAVEDKEYB"));
    assert!(spy.raw(&id).unwrap().contains("SAVEDKEYA"));
    s3::disconnect("sv-stores-save");
}

#[test]
fn s3_saved_lifecycle_keeps_deletes_and_forgets_keys() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_lifecycle_keeps_deletes_and_forgets_keys") {
        return;
    }
    let srv = serve_bucket("sv-life", vec![], vec![]);
    let spy = Arc::new(Spy::default());
    let mut s = session(Some(&spy));
    let id = saved(&mut s, "sv-life-conn", &srv.base, "sv-life", "keys");
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    let raw = spy.raw(&id);
    assert!(raw.is_some());
    // Renaming, grouping, buckets and region keep the pair and ask nothing of the store.
    let r = s.run("connection.edit", &json!({"id": id, "name": "sv-life-renamed", "group": "Work"})).unwrap();
    assert_eq!((r["savedKeys"].as_bool(), r["loggedIn"].as_bool()), (Some(true), Some(false)), "{r}");
    let r = s
        .run(
            "connection.edit",
            &json!({"id": id, "options": {"credentials": "keys", "buckets": "sv-life,sv-life-two", "region": "eu-west-1"}}),
        )
        .unwrap();
    assert_eq!(r["savedKeys"], true);
    assert_eq!(spy.raw(&id), raw);
    assert_eq!(spy.count("delete"), 0);
    // A copy has no keys of its own, and no marker.
    let copy = s.run("connection.duplicate", &json!({"id": id, "name": "sv-life-copy"})).unwrap();
    assert_eq!(copy["savedKeys"], false);
    assert!(spy.raw(copy["id"].as_str().unwrap()).is_none());
    // Another spelling of the same endpoint isn't a change.
    let spelled = format!("{}/", srv.base.replace("http://", "HTTP://"));
    let r = s.run("connection.edit", &json!({"id": id, "address": spelled})).unwrap();
    assert_eq!(r["savedKeys"], true);
    assert_eq!((spy.raw(&id), spy.count("delete")), (raw.clone(), 0));
    // Logged in again, a refused delete stops an endpoint edit, a forget and a removal, leaving
    // the definition, its generation, the marker and the login as they were.
    let login_again = |s: &mut Session| s.run("connection.login", &json!({"id": id})).unwrap();
    login_again(&mut s);
    spy.refuse_delete.store(true, Ordering::Relaxed);
    let (generation, address) = (s.connections.generation, s.connections.find(&id).unwrap().address.clone());
    let e = s.run("connection.edit", &json!({"id": id, "address": "http://127.0.0.1:9"})).unwrap_err();
    assert_eq!(e, s3_keys::NOT_FORGOTTEN);
    assert_eq!(s.run("connection.forgetSavedKeys", &json!({"id": id})).unwrap_err(), s3_keys::NOT_FORGOTTEN);
    assert_eq!(s.run("connection.remove", &json!({"id": id})).unwrap_err(), s3_keys::NOT_FORGOTTEN);
    let kept = s.connections.find(&id).unwrap();
    assert_eq!((kept.address.as_str(), kept.saved_keys, s.connections.generation), (address.as_str(), true, generation));
    assert!(s3::connection("sv-life-renamed").is_some(), "the login stays");
    assert_eq!(spy.raw(&id), raw);
    // An edit that is refused for another reason deletes nothing.
    spy.refuse_delete.store(false, Ordering::Relaxed);
    let deletes = spy.count("delete");
    assert!(s.run("connection.edit", &json!({"id": id, "name": "c1", "address": "http://127.0.0.1:9"})).is_err());
    assert!(s.run("connection.edit", &json!({"id": id, "address": "ftp://127.0.0.1:9"})).is_err());
    assert_eq!(spy.count("delete"), deletes);
    assert!(spy.raw(&id).is_some());
    // Forgetting deletes the pair, not the login; again is no error.
    let r = s.run("connection.forgetSavedKeys", &json!({"id": "sv-life-renamed"})).unwrap();
    assert_eq!((r["savedKeys"].as_bool(), r["loggedIn"].as_bool()), (Some(false), Some(true)), "{r}");
    assert!(spy.raw(&id).is_none() && s3::connection("sv-life-renamed").is_some());
    assert!(s.run("connection.forgetSavedKeys", &json!({"id": id})).is_ok());
    assert!(s.run("connection.forgetSavedKeys", &json!({"id": "c99"})).is_err());
    let wms = s.run("connection.new", &json!({"kind": "wms", "name": "sv-life-wms", "address": "http://127.0.0.1:9/wms"})).unwrap();
    assert!(s.run("connection.forgetSavedKeys", &json!({"id": wms["id"]})).is_err());
    // A real endpoint change deletes the old pair before the new definition is stored.
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    let r = s.run("connection.edit", &json!({"id": id, "address": "http://127.0.0.1:9"})).unwrap();
    assert_eq!(r["savedKeys"], false);
    assert!(spy.raw(&id).is_none());
    let last_delete = spy.ops().iter().rposition(|o| o.starts_with("delete")).unwrap();
    assert!(spy.ops().iter().skip(last_delete).all(|o| !o.starts_with("save")));
    // Removal deletes the saved pair and disconnects the login.
    s.run("connection.edit", &json!({"id": id, "address": srv.base})).unwrap();
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    s.run("connection.remove", &json!({"id": id})).unwrap();
    assert!(spy.raw(&id).is_none() && s.connections.find(&id).is_none());
    let copy_id = copy["id"].as_str().unwrap();
    s.run("connection.remove", &json!({"id": copy_id})).unwrap();
    // Without a store, a marker blocks removal and forgetting; without one, removal is as before.
    let mut t = session(None);
    t.connections = Connections::from_json(
        &json!({"next": 5, "connections": [
        {"id": "c3", "name": "sv-life-marked", "kind": "s3", "options": {"credentials": "keys"}, "savedKeys": true},
        {"id": "c4", "name": "sv-life-plain", "kind": "s3", "options": {"credentials": "keys"}}]})
        .to_string(),
    );
    assert_eq!(t.run("connection.remove", &json!({"id": "c3"})).unwrap_err(), s3_keys::NOT_FORGOTTEN);
    assert_eq!(t.run("connection.forgetSavedKeys", &json!({"id": "c3"})).unwrap_err(), s3_keys::NOT_FORGOTTEN);
    assert!(t.connections.find("c3").is_some_and(|c| c.saved_keys));
    t.run("connection.remove", &json!({"id": "c4"})).unwrap();
    s3::disconnect("sv-life-renamed");
}

#[test]
fn s3_saved_a_binding_signs_only_its_own_connection_and_errors_never_echo_the_key() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_a_binding_signs_only_its_own_connection_and_errors_never_echo_the_key")
    {
        return;
    }
    let moved_to = serve_bucket("sv-bind", vec![("elsewhere.fgb", vec![1, 2, 3])], vec![]);
    let srv =
        serve_bucket("sv-bind", vec![("grid.fgb", grid_fgb())], vec![("moved.fgb", format!("{}/sv-bind/elsewhere.fgb", moved_to.base))]);
    let mut s = session(None);
    let (a, b) = (saved(&mut s, "sv-bind-a", &srv.base, "sv-bind", "keys"), saved(&mut s, "sv-bind-b", &srv.base, "sv-bind", "keys"));
    login(&mut s, &b, "SAVEDKEYB", "saved-secret-B", false).unwrap();
    login(&mut s, &a, "SAVEDKEYA", "saved-secret-A", false).unwrap();
    let object = |key: &str| format!("{}/sv-bind/{key}", srv.base);
    // The second connection's address is read with its key, whatever order they logged in.
    let from = srv.requests();
    s.run("layer.addUrl", &json!({"url": bound(&s, &b, &object("grid.fgb")), "name": "by b"})).unwrap();
    assert_eq!(srv.key_ids(from), BTreeSet::from(["SAVEDKEYB".to_string()]));
    let from = srv.requests();
    s.run("layer.addUrl", &json!({"url": bound(&s, &a, &object("grid.fgb")), "name": "by a"})).unwrap();
    assert_eq!(srv.key_ids(from), BTreeSet::from(["SAVEDKEYA".to_string()]));
    // An unrelated address of the same endpoint isn't signed by either.
    let from = srv.requests();
    s.run("layer.addUrl", &json!({"url": object("grid.fgb"), "name": "plain"})).unwrap();
    assert!(srv.requests() > from && srv.key_ids(from).is_empty() && srv.unsigned(from) == srv.requests() - from);
    // Another endpoint, an ungranted bucket, no token and a token nobody has: refused, not
    // read with someone else's key.
    let from = srv.requests();
    let wrap = |token: &str, endpoint: &str, url: &str| rustgis_data::s3_address::bind(token, endpoint, url).unwrap();
    let token = s.s3_binding(&a).unwrap().to_string();
    for (bad, why) in [
        (wrap(&token, &moved_to.base, &format!("{}/sv-bind/grid.fgb", moved_to.base)), s3::MISMATCH),
        (wrap(&token, &srv.base, &format!("{}/sv-ungranted/grid.fgb", srv.base)), s3::MISMATCH),
        (wrap("", &srv.base, &object("grid.fgb")), s3::UNRESOLVED),
        (wrap("00000000000000ff", &srv.base, &object("grid.fgb")), s3::UNRESOLVED),
    ] {
        let e = s.run("layer.addUrl", &json!({"url": bad})).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
    assert_eq!(srv.requests(), from, "refused before anything is sent");
    // A refusal is told by its status: the server's answer echoes the Authorization header.
    let e = s.run("layer.addUrl", &json!({"url": bound(&s, &b, &object("missing.fgb"))})).unwrap_err();
    assert!(e.contains("404"), "{e}");
    for secret in ["SAVEDKEYB", "SAVEDKEYA", "saved-secret", "AWS4-HMAC", "Authorization", "authorization"] {
        assert!(!e.contains(secret), "{secret} in {e}");
    }
    assert!(srv.saw("SAVEDKEYB"), "the request itself was signed");
    // A signed request isn't redirected to another server.
    let e = s.run("layer.addUrl", &json!({"url": bound(&s, &b, &object("moved.fgb"))})).unwrap_err();
    assert!(e.contains("redirect"), "{e}");
    assert_eq!(moved_to.requests(), 0);
    // An ordinary connection of the same name takes the saved login's place: its addresses wait.
    s3::connect(s3::Connection {
        name: "sv-bind-a".into(),
        endpoint: Some(srv.base.clone()),
        region: String::new(),
        buckets: vec![],
        credentials: None,
    })
    .unwrap();
    // (An address already read is kept in the cache, so the check is of one that wasn't.)
    let e = s.run("layer.addUrl", &json!({"url": bound_with(&token, &srv.base, &object("other.fgb"))})).unwrap_err();
    assert!(e.contains(s3::UNRESOLVED), "{e}");
    for name in ["sv-bind-a", "sv-bind-b"] {
        s3::disconnect(name);
    }
}

fn bound_with(token: &str, endpoint: &str, url: &str) -> String {
    rustgis_data::s3_address::bind(token, endpoint, url).unwrap()
}

/// Waits until the server has had no new request for a moment (reads the layers just added
/// started in the background have finished).
fn settle(srv: &Server) {
    let (mut last, mut quiet) = (usize::MAX, 0);
    for _ in 0..200 {
        cache::wait(std::time::Duration::from_millis(200));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = srv.requests();
        quiet = if now == last { quiet + 1 } else { 0 };
        if quiet >= 4 {
            return;
        }
        last = now;
    }
}

/// What a project of three sources through a saved connection looks like to a new session: the
/// project's text, and the connections' preference.
struct Saved {
    srv: Server,
    store: Arc<Spy>,
    text: String,
    prefs: String,
    fgb: String,
}

/// Builds a project with a streamed FlatGeobuf, a COG and a PMTiles archive, all through one
/// saved connection `<bucket>-a` (keys remembered), and a second saved connection `<bucket>-b` of
/// the same endpoint and bucket (keys remembered too).
fn saved_project(bucket: &str) -> Saved {
    let srv = serve_bucket(
        bucket,
        vec![("grid.fgb", grid_fgb()), ("scene.tif", rustgis_data::raster::cog::example(2048, 256)), ("parks.pmtiles", parks_pmtiles())],
        vec![],
    );
    let store = Arc::new(Spy::default());
    let mut s = session(Some(&store));
    let name = format!("{bucket}-a");
    let id = saved(&mut s, &name, &srv.base, bucket, "keys");
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    let second = saved(&mut s, &format!("{bucket}-b"), &srv.base, bucket, "keys");
    login(&mut s, &second, "SAVEDKEYB", "saved-secret-B", true).unwrap();
    // The first one's login is the one the project's sources are added through.
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", false).unwrap();
    let token = s.s3_binding(&id).unwrap().to_string();
    let object = |key: &str| bound_with(&token, &srv.base, &format!("{}/{bucket}/{key}", srv.base));
    let fgb = object("grid.fgb");
    s.run("layer.addUrl", &json!({"url": fgb, "name": "Grid"})).unwrap();
    let cog_url = object("scene.tif");
    s.run("layer.addUrl", &json!({"url": cog_url, "name": "Scene"})).unwrap();
    s.run("layer.addUrl", &json!({"url": object("parks.pmtiles"), "name": "Parks"})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    let prefs = s.connections.to_json();
    s3::disconnect(&name);
    s3::disconnect(&format!("{bucket}-b"));
    // The tiles and archives just added read in the background: wait for them to finish, so a
    // test counting requests counts only its own.
    settle(&srv);
    Saved { srv, store, text, prefs, fgb }
}

/// A new session with the saved connections (the first `count` of them) and the store.
fn reopened(p: &Saved, store: &Arc<Spy>, keep: usize) -> Session {
    let mut s = session(Some(store));
    s.connections = Connections::from_json(&p.prefs);
    s.connections.connections.truncate(keep);
    s
}

#[test]
fn s3_saved_projects_hold_no_keys_and_reload_through_their_one_connection() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_projects_hold_no_keys_and_reload_through_their_one_connection") {
        return;
    }
    let p = saved_project("sv-proj-one");
    for secret in ["SAVEDKEYA", "SAVEDKEYB", "saved-secret"] {
        assert!(!p.text.contains(secret) && !p.prefs.contains(secret), "{secret}");
    }
    assert!(p.text.contains("rustgis-s3:") && p.text.contains("sv-proj-one"));
    // Read as plain data, with no session to answer for it: nothing is fetched.
    let from = p.srv.requests();
    let project = Project::from_json(&p.text).unwrap();
    let source = project.map().layers.iter().find_map(|l| l.source.as_ref()).unwrap();
    assert!(!source.is_open() && source.error.is_none());
    assert_eq!(p.srv.requests(), from);
    // A binding in the file is nobody's login: it is cleared.
    let path = source.path.clone().unwrap();
    assert_eq!(rustgis_data::s3_address::parse(&path).unwrap().unwrap().binding, "");
    assert_eq!(rustgis_data::s3_address::parse(&p.fgb).unwrap().unwrap().binding.len(), 16);
    // One connection could read it: the keys come from the store, and everything opens.
    let mut s = reopened(&p, &p.store, 1);
    s.run("project.openText", &json!({"text": p.text})).unwrap();
    assert!(s.s3_pending.is_empty());
    let id = streamed(&s, "grid.fgb");
    assert!(read_view(&mut s, id) > 0, "{:?}", s.map().layer(id).unwrap().source.as_ref().unwrap().error);
    let layer = s.map().layers.iter().find(|l| l.name == "Grid").unwrap();
    assert!(layer.source.as_ref().unwrap().path.as_deref().is_some_and(|x| x.contains(s.s3_binding("c1").unwrap())));
    // The COG, a raster layer read in place, holds its overview, read through the connection;
    // its tiles are drawn from its pixels.
    let scene = s.map().layers.iter().find(|l| l.name == "Scene").and_then(|l| l.raster.clone()).unwrap();
    let address = scene.path.clone().unwrap();
    assert!(scene.in_place && address.contains(s.s3_binding("c1").unwrap()), "{address}");
    assert!(scene.data.is_some(), "{:?}", scene.error);
    let (z, lon, lat) = (9u32, 10.5f64, 45.5f64);
    let n = f64::from(1u32 << z);
    let x = ((lon + 180.0) / 360.0 * n) as u32;
    let y = ((1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * n) as u32;
    let tile = format!("cog:{z}/{x}/{y}@{address}");
    let mut got = None;
    for _ in 0..300 {
        match cache::get(&tile, cache::Want::Image) {
            cache::State::Ready(p) => {
                got = Some(p);
                break;
            }
            cache::State::Failed(e) => panic!("{e}"),
            cache::State::Pending => cache::wait(std::time::Duration::from_millis(50)),
        }
    }
    let Some(payload) = got else { panic!("the tile never came") };
    let cache::Payload::Image(pm) = &*payload else { panic!("not a picture") };
    assert_eq!((pm.width(), pm.height()), (256, 256));
    assert!(pm.pixels().iter().any(|px| px.alpha() == 255));
    // The PMTiles' park is found where it is.
    s.map_mut().camera.center = rustgis_geom::Coord::new(0.0, 0.0);
    s.map_mut().camera.res = 2.0 * 20_037_508.342_789_244 / 256.0;
    let mut found = Value::Null;
    for _ in 0..500 {
        found = s.run("identify", &json!({"lon": 1.0, "lat": 1.0})).unwrap();
        if !found.is_null() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!((found["sourceLayer"].as_str(), found["attributes"]["name"].as_str()), (Some("parks"), Some("Central")), "{found}");
    assert_eq!(p.srv.key_ids(from), BTreeSet::from(["SAVEDKEYA".to_string()]));
    assert!(!p.srv.saw("saved-secret"));
    // The text saved again has no key either, and the project names the login that was used.
    let (again, _) = s.project.saved_text(None).unwrap();
    assert!(!again.contains("SAVEDKEY") && !again.contains("saved-secret"));
    s3::disconnect("sv-proj-one-a");
}

#[test]
fn s3_saved_several_candidates_wait_for_a_choice_and_nothing_is_fetched_before() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_several_candidates_wait_for_a_choice_and_nothing_is_fetched_before") {
        return;
    }
    let p = saved_project("sv-proj-two");
    let store = p.store.clone();
    let (from, saves) = (p.srv.requests(), store.count("save"));
    let mut s = reopened(&p, &store, 2);
    s.run("project.openText", &json!({"text": p.text})).unwrap();
    assert_eq!(p.srv.requests(), from, "opening fetches nothing while the choice is open");
    assert_eq!(s.s3_pending.len(), 1, "{:?}", s.s3_pending);
    let pending = s.s3_pending[0].clone();
    assert_eq!((pending.bucket.as_str(), pending.candidates.clone()), ("sv-proj-two", vec!["c1".to_string(), "c2".to_string()]));
    assert_eq!(pending.endpoint.as_deref(), Some(p.srv.base.as_str()));
    // A frame draws; blocked sources ask for nothing, signed or not.
    let view = crate::render::View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    let _ = crate::render::render_png(s.map_mut(), &view);
    assert_eq!(p.srv.requests(), from);
    assert_eq!(store.count("save"), saves, "opening saves nothing");
    // A stale or unknown choice does nothing.
    for (source, id) in
        [(pending.source.as_str(), "c99"), ("rustgis-s3::0::s3://nope/x.fgb", "c1"), ("", ""), (pending.source.as_str(), "")]
    {
        let e = s.run("connection.resolveS3", &json!({"source": source, "id": id})).unwrap_err();
        assert_eq!(e, crate::s3_connections::STALE);
    }
    assert_eq!((p.srv.requests(), s.s3_pending.len()), (from, 1));
    // A candidate removed, or moved to another endpoint, is no longer one.
    let mut t = reopened(&p, &store, 2);
    t.run("project.openText", &json!({"text": p.text})).unwrap();
    let source = t.s3_pending[0].source.clone();
    t.connections
        .update("c2", crate::connections::Connection { address: "http://127.0.0.1:9".into(), ..t.connections.find("c2").unwrap().clone() })
        .unwrap();
    assert_eq!(t.run("connection.resolveS3", &json!({"source": source, "id": "c2"})).unwrap_err(), crate::s3_connections::STALE);
    t.connections.remove("c1");
    assert_eq!(t.run("connection.resolveS3", &json!({"source": source, "id": "c1"})).unwrap_err(), crate::s3_connections::STALE);
    assert_eq!(p.srv.requests(), from);
    // The second candidate chosen: its key reads, and no other.
    let r = s.run("connection.resolveS3", &json!({"source": pending.source, "id": "c2"})).unwrap();
    assert_eq!(r["id"], "c2");
    assert!(r["reopened"].as_u64().unwrap() >= 2 && r["failed"] == 0, "{r}");
    assert!(s.s3_pending.is_empty());
    let id = streamed(&s, "grid.fgb");
    assert!(read_view(&mut s, id) > 0);
    assert_eq!(p.srv.key_ids(from), BTreeSet::from(["SAVEDKEYB".to_string()]));
    for name in ["sv-proj-two-a", "sv-proj-two-b"] {
        s3::disconnect(name);
    }
}

#[test]
fn s3_saved_sources_without_keys_zero_matches_and_older_addresses_stay_blocked_or_as_they_were() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test(
        "s3_saved_tests::s3_saved_sources_without_keys_zero_matches_and_older_addresses_stay_blocked_or_as_they_were",
    ) {
        return;
    }
    let p = saved_project("sv-proj-edge");
    let from = p.srv.requests();
    // One candidate, whose keys are not in the store (another computer): blocked, pending for its
    // log in, until the keys are typed.
    let empty = Arc::new(Spy::default());
    let mut s = reopened(&p, &empty, 1);
    s.run("project.openText", &json!({"text": p.text})).unwrap();
    assert_eq!(p.srv.requests(), from);
    assert_eq!(s.s3_pending.len(), 1);
    assert_eq!(s.s3_pending[0].candidates, vec!["c1".to_string()]);
    let source = s.s3_pending[0].source.clone();
    assert_eq!(s.run("connection.resolveS3", &json!({"source": source, "id": "c1"})).unwrap_err(), s3_keys::NONE_SAVED);
    assert_eq!((s.s3_pending.len(), p.srv.requests()), (1, from));
    let blocked = s.map().layers.iter().find_map(|l| l.source.as_ref()).unwrap();
    assert_eq!(blocked.error.as_deref(), Some(s3::UNRESOLVED));
    login(&mut s, "c1", "SAVEDKEYA", "saved-secret-A", false).unwrap();
    let r = s.run("connection.resolveS3", &json!({"source": source, "id": "c1"})).unwrap();
    assert_eq!(r["failed"], 0, "{r}");
    let id = streamed(&s, "grid.fgb");
    assert!(read_view(&mut s, id) > 0);
    assert!(s.s3_pending.is_empty());
    // No saved connection reads it: blocked with a notice, nothing fetched, no one chosen.
    settle(&p.srv);
    let from = p.srv.requests();
    let mut none = session(Some(&empty));
    none.run("project.openText", &json!({"text": p.text})).unwrap();
    assert!(none.s3_pending.is_empty());
    assert!(none.notices.iter().any(|n| n.error && n.text.contains("no saved connection")), "{:?}", none.notices);
    assert_eq!(none.map().layers.iter().find_map(|l| l.source.as_ref()).unwrap().error.as_deref(), Some(s3::UNRESOLVED));
    assert_eq!(p.srv.requests(), from);
    // A GeoTIFF read in place from the bucket waits for the connection too, and says so, once.
    let fgb = none.map().layers.iter().find_map(|l| l.source.as_ref().and_then(|src| src.path.clone())).unwrap();
    let scene = fgb.replace("grid.fgb", "scene.tif");
    assert_ne!(scene, fgb);
    none.map_mut().add_raster("Scene".to_string(), rustgis_map::raster::RasterLayer::unread_in_place(scene));
    let text = serde_json::to_string(&none.project).unwrap();
    let mut none = session(Some(&empty));
    none.run("project.openText", &json!({"text": text})).unwrap();
    let scene = none.map().layers.iter().find(|l| l.name == "Scene").and_then(|l| l.raster.as_ref()).unwrap();
    assert!(scene.data.is_none() && scene.error.as_deref() == Some(s3::UNRESOLVED), "{:?}", scene.error);
    // One notice (the project's maps each hold the layer: one line naming them all).
    let said = |s: &Session| {
        s.notices.iter().filter(|n| n.error && n.text.contains("couldn't be read in place") && n.text.contains("Scene")).count()
    };
    assert_eq!(said(&none), 1, "{:?}", none.notices);
    none.run("map.zoomIn", &Value::Null).unwrap();
    assert_eq!(said(&none), 1);
    assert_eq!(p.srv.requests(), from, "nothing fetched without the connection");
    // An older project's bare address, with a session-only connection of the bucket: as before.
    let legacy = p.text.replace(&p.fgb, "s3://sv-proj-edge/grid.fgb");
    assert!(legacy.contains("s3://sv-proj-edge/grid.fgb") && !legacy.contains(&p.fgb), "the Grid layer is an older project's");
    let mut old = session(Some(&empty));
    old.run("s3.connect", &json!({"name": "sv-proj-plain", "endpoint": p.srv.base, "buckets": ["sv-proj-edge"], "accessKeyId": "PLAINKEY", "secretAccessKey": "plain-secret"})).unwrap();
    old.run("project.openText", &json!({"text": legacy})).unwrap();
    assert!(old.s3_pending.is_empty());
    let id = streamed(&old, "grid.fgb");
    assert!(read_view(&mut old, id) > 0, "{:?}", old.map().layer(id).unwrap().source.as_ref().unwrap().error);
    assert_eq!(p.srv.key_ids(from), BTreeSet::from(["PLAINKEY".to_string()]));
    s3::disconnect("sv-proj-plain");
    // The same bare address with two saved candidates: unresolved placeholder, unknown origin kept.
    settle(&p.srv);
    let from = p.srv.requests();
    let mut two = reopened(&p, &p.store, 2);
    two.run("project.openText", &json!({"text": legacy})).unwrap();
    assert_eq!(p.srv.requests(), from);
    // Two resources wait: the older bare address (unknown origin), and the tiles' known endpoint.
    assert_eq!(two.s3_pending.len(), 2, "{:?}", two.s3_pending);
    let older = two.s3_pending.iter().find(|p| p.endpoint.is_none()).unwrap().clone();
    assert_eq!(older.candidates.len(), 2);
    assert!(two.s3_pending.iter().any(|p| p.endpoint.is_some() && p.candidates.len() == 2));
    let r = two.run("connection.resolveS3", &json!({"source": older.source, "id": "c2"})).unwrap();
    assert_eq!(r["id"], "c2");
    assert_eq!(two.s3_pending.len(), 1, "only the chosen resource is settled");
    let id = streamed(&two, "grid.fgb");
    assert!(read_view(&mut two, id) > 0, "{:?}", two.map().layer(id).unwrap().source.as_ref().unwrap().error);
    assert_eq!(p.srv.key_ids(from), BTreeSet::from(["SAVEDKEYB".to_string()]));
    for name in ["sv-proj-edge-a", "sv-proj-edge-b"] {
        s3::disconnect(name);
    }
}

#[test]
fn s3_saved_imported_maps_layers_and_packages_never_bring_their_own_login() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_imported_maps_layers_and_packages_never_bring_their_own_login") {
        return;
    }
    let p = saved_project("sv-proj-import");
    let token = rustgis_data::s3_address::parse(&p.fgb).unwrap().unwrap().binding.to_string();
    let from = p.srv.requests();
    // Package and template reads (and a plain project read) clear every binding.
    for read in [Project::from_import_json(&p.text, None).unwrap(), Project::from_json(&p.text).unwrap()] {
        for a in read.maps.iter().flat_map(|m| m.layers.iter()).filter_map(|l| l.source.as_ref().and_then(|s| s.path.as_ref())) {
            assert!(a.starts_with("rustgis-s3::") && !a.contains(&token), "{a}");
        }
        for t in &read.map().tiles {
            assert!(!t.url.contains(&token), "{}", t.url);
        }
    }
    // A map imported with no saved connection to read it keeps the cleared address, unfetched.
    let map = serde_json::to_string(&Project::from_import_json(&p.text, None).unwrap().maps[0]).unwrap();
    let raw_map = map.replace("rustgis-s3::", &format!("rustgis-s3:{token}:"));
    assert!(raw_map.contains(&token));
    let mut s = session(Some(&Arc::new(Spy::default())));
    s.run("map.import", &json!({"text": raw_map})).unwrap();
    assert!(!serde_json::to_string(&s.project).unwrap().contains(&token), "the map's own token is not kept");
    assert_eq!(p.srv.requests(), from);
    // With one saved connection: bound to this session's login, not the file's token.
    let mut t = reopened(&p, &p.store, 1);
    t.run("map.import", &json!({"text": raw_map})).unwrap();
    let own = t.s3_binding("c1").unwrap().to_string();
    assert_ne!(own, token);
    let text = serde_json::to_string(&t.project).unwrap();
    assert!(text.contains(&own) && !text.contains(&token));
    // A layer file likewise.
    let layer =
        serde_json::to_string(&Project::from_import_json(&p.text, None).unwrap().maps[0].layers.iter().find(|l| l.name == "Grid").unwrap())
            .unwrap();
    let raw_layer = layer.replace("rustgis-s3::", &format!("rustgis-s3:{token}:"));
    let mut u = session(Some(&Arc::new(Spy::default())));
    crate::cmd::layer::add_layer_text(&mut u, &raw_layer).unwrap();
    assert!(!serde_json::to_string(&u.project).unwrap().contains(&token));
    assert_eq!(u.map().layers[0].source.as_ref().unwrap().error.as_deref(), Some(s3::UNRESOLVED));
    s3::disconnect("sv-proj-import-a");
    s3::disconnect("sv-proj-import-b");
}

#[test]
fn s3_saved_typed_keys_are_in_no_script_result_notice_or_list_the_app_shows() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_typed_keys_are_in_no_script_result_notice_or_list_the_app_shows") {
        return;
    }
    let srv = serve_bucket("sv-seen", vec![], vec![]);
    let store = Arc::new(Spy::default());
    let mut s = session(Some(&store));
    let id = saved(&mut s, "sv-seen-conn", &srv.base, "sv-seen", "keys");
    let params =
        json!({"id": id, "accessKeyId": "SAVEDKEYA", "secretAccessKey": "saved-secret-A", "sessionToken": "tok-1", "remember": true});
    // Exercise the exported history commands, not the raw internal Python formatter.
    s.project.gp_history.push(crate::GpRecord {
        tool: "Buffer".into(),
        id: "buffer".into(),
        params: params.clone(),
        command: None,
        at: 0,
        ok: true,
        messages: Vec::new(),
        millis: 0,
        outputs: Vec::new(),
    });
    for kind in ["python", "command", "cli"] {
        let code = s.run("gp.historyCode", &json!({"run": 0, "as": kind})).unwrap();
        for secret in ["SAVEDKEYA", "saved-secret-A", "tok-1"] {
            assert!(!code.to_string().contains(secret), "{secret} in {code}");
        }
    }
    let script = s.run("gp.historyScript", &Value::Null).unwrap();
    for secret in ["SAVEDKEYA", "saved-secret-A", "tok-1"] {
        assert!(!script.to_string().contains(secret), "{secret} in {script}");
    }
    s.project.gp_history.clear();
    assert!(crate::sources::holds_secret(&params));
    // Everything the session shows afterwards.
    let result = s.run("connection.login", &params).unwrap();
    login(&mut s, &id, "SAVEDKEYA", "saved-secret-A", true).unwrap();
    assert!(s.project.gp_history.is_empty(), "view-only logins never enter History");
    let mut shown = vec![result.to_string(), s.connections.to_json(), s.sources.to_json(), serde_json::to_string(&s.project).unwrap()];
    shown.push(s.run("connection.list", &json!({})).unwrap().to_string());
    shown.push(s.run("s3.connections", &Value::Null).unwrap().to_string());
    shown.push(serde_json::to_string(&s.notices.iter().map(|n| n.text.clone()).collect::<Vec<_>>()).unwrap());
    shown.push(s.project.saved_text(None).unwrap().0);
    for e in [
        s.run("connection.login", &json!({"id": id, "accessKeyId": "SAVEDKEYA", "secretAccessKey": "saved-secret-A", "remember": "yes"}))
            .unwrap_err(),
        s.run("connection.login", &json!({"id": id, "accessKeyId": "SAVEDKEYA", "remember": true})).unwrap_err(),
        s.run("connection.edit", &json!({"id": id, "address": "ftp://x"})).unwrap_err(),
    ] {
        shown.push(e);
    }
    for text in &shown {
        for secret in ["SAVEDKEYA", "saved-secret-A", "tok-1"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    // Rendering frames and listing connections ask nothing more of the store.
    let ops = store.ops().len();
    let view = crate::render::View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    let _ = crate::render::render_png(s.map_mut(), &view);
    s.run("connection.list", &json!({})).unwrap();
    assert_eq!(store.ops().len(), ops);
    s3::disconnect("sv-seen-conn");
}

#[test]
fn s3_saved_database_and_s3_share_one_store_without_crossing_credentials_or_cleanup() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test(
        "s3_saved_tests::s3_saved_database_and_s3_share_one_store_without_crossing_credentials_or_cleanup",
    ) {
        return;
    }
    let store = Arc::new(Spy::default());
    let mut s = session(Some(&store));
    let db = s
        .run(
            "connection.new",
            &json!({
                "kind": "postgres", "name": "sv-combined-db",
                "address": "postgres:host=127.0.0.1 port=1 dbname=combined user=reader sslmode=disable"
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let s3id = saved(&mut s, "sv-combined-s3", "http://127.0.0.1:9", "sv-combined", "keys");
    s.run("connection.login", &json!({"id": db, "password": "combined-db-password", "remember": true})).unwrap();
    login(&mut s, &s3id, "COMBINEDKEY", "combined-s3-secret", true).unwrap();
    let db_value = store.raw(&db).unwrap();
    let s3_value = store.raw(&s3id).unwrap();
    assert!(db_value.contains("combined-db-password") && !db_value.contains("combined-s3-secret"));
    assert!(s3_value.contains("combined-s3-secret") && !s3_value.contains("combined-db-password"));
    let prefs = s.connections.to_json();
    for secret in ["combined-db-password", "COMBINEDKEY", "combined-s3-secret"] {
        assert!(!prefs.contains(secret));
    }
    s3::disconnect("sv-combined-s3");
    let mut fresh = session(Some(&store));
    fresh.connections = Connections::from_json(&prefs);
    fresh.run("connection.login", &json!({"id": db})).unwrap();
    fresh.run("connection.login", &json!({"id": s3id})).unwrap();
    assert_eq!(fresh.database_logins.known_password(&db).unwrap().expose(), "combined-db-password");
    assert_eq!(live_key("sv-combined-s3").as_deref(), Some("COMBINEDKEY"));
    // One malformed opt-in contract, checked before either kind mutates state or the store.
    for id in [&db, &s3id] {
        let before = store.ops().len();
        assert!(fresh.run("connection.login", &json!({"id": id, "remember": "yes"})).is_err());
        assert_eq!(store.ops().len(), before);
    }
    // Both kinds use the same validate/delete/commit boundary on a refused deletion.
    store.refuse_delete.store(true, Ordering::Relaxed);
    for (id, address) in [(&db, "postgres:host=changed dbname=combined user=reader"), (&s3id, "http://127.0.0.1:10")] {
        let before = fresh.connections.to_json();
        let generation = fresh.connections.generation;
        assert!(fresh.run("connection.edit", &json!({"id": id, "address": address})).is_err());
        assert!(fresh.run("connection.remove", &json!({"id": id})).is_err());
        assert_eq!(fresh.connections.to_json(), before);
        assert_eq!(fresh.connections.generation, generation);
    }
    assert_eq!(fresh.database_logins.known_password(&db).unwrap().expose(), "combined-db-password");
    assert_eq!(live_key("sv-combined-s3").as_deref(), Some("COMBINEDKEY"));
    store.refuse_delete.store(false, Ordering::Relaxed);
    fresh.run("connection.forgetSavedKeys", &json!({"id": s3id})).unwrap();
    assert!(store.raw(&s3id).is_none());
    assert_eq!(store.raw(&db).as_deref(), Some(db_value.as_str()));
    assert_eq!(live_key("sv-combined-s3").as_deref(), Some("COMBINEDKEY"));
    fresh.run("connection.forgetPassword", &json!({"id": db})).unwrap();
    assert!(store.raw(&db).is_none());
    assert!(!fresh.connections.find(&db).unwrap().remembered);
    assert!(!fresh.connections.find(&s3id).unwrap().saved_keys);
    s3::disconnect("sv-combined-s3");
}

#[test]
fn s3_saved_raster_pixels_wait_for_selection_then_reload_with_style_intact() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_raster_pixels_wait_for_selection_then_reload_with_style_intact") {
        return;
    }
    let bytes = rustgis_data::raster::cog::example(256, 256);
    let pixels = rustgis_data::raster::read_bytes("scene.tif", &bytes).unwrap();
    let srv = serve_bucket("sv-raster", vec![("scene.tif", bytes)], vec![]);
    let store = Arc::new(Spy::default());
    let mut seed = session(Some(&store));
    let first = saved(&mut seed, "sv-raster-first", &srv.base, "sv-raster", "keys");
    login(&mut seed, &first, "RASTERKEYA", "raster-secret-a", true).unwrap();
    let mut layer = rustgis_map::Layer::new(1, rustgis_data::Dataset::new("Saved raster", rustgis_data::GeometryType::None, Vec::new()));
    layer.raster = Some(rustgis_map::RasterLayer::from_path(pixels, bound(&seed, &first, "s3://sv-raster/scene.tif")));
    layer.transparency = 0.25;
    seed.map_mut().layers.push(layer);
    let text = seed.project.saved_text(None).unwrap().0;
    let prefs = seed.connections.to_json();
    s3::disconnect("sv-raster-first");
    let mut unique = session(Some(&store));
    unique.connections = Connections::from_json(&prefs);
    unique.run("project.openText", &json!({"text": text})).unwrap();
    let raster_id = unique.map().layers.iter().find(|l| l.dataset.name == "Saved raster").unwrap().id;
    read_view(&mut unique, raster_id);
    let raster = unique.map().layers.iter().find(|l| l.dataset.name == "Saved raster").unwrap();
    let data = raster.raster.as_ref().unwrap().data.as_ref().expect("unique saved raster must reload");
    assert_eq!((data.width, data.height), (256, 256));
    assert_eq!(raster.transparency, 0.25);
    assert_eq!(srv.key_ids(0), BTreeSet::from(["RASTERKEYA".to_string()]));
    let second = saved(&mut unique, "sv-raster-second", &srv.base, "sv-raster", "keys");
    login(&mut unique, &second, "RASTERKEYB", "raster-secret-b", true).unwrap();
    let prefs = unique.connections.to_json();
    s3::disconnect("sv-raster-first");
    s3::disconnect("sv-raster-second");
    let mut ambiguous = session(Some(&store));
    ambiguous.connections = Connections::from_json(&prefs);
    let from = srv.requests();
    ambiguous.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(srv.requests(), from);
    let source = ambiguous.s3_pending[0].source.clone();
    let choice = ambiguous.run("connection.resolveS3", &json!({"source": source, "id": second})).unwrap();
    assert_eq!((choice["reopened"].as_u64(), choice["failed"].as_u64()), (Some(1), Some(0)));
    let raster_id = ambiguous.map().layers.iter().find(|l| l.dataset.name == "Saved raster").unwrap().id;
    read_view(&mut ambiguous, raster_id);
    let raster = ambiguous.map().layers.iter().find(|l| l.dataset.name == "Saved raster").unwrap();
    assert_eq!(raster.raster.as_ref().unwrap().data.as_ref().unwrap().width, 256);
    assert_eq!(raster.transparency, 0.25);
    assert_eq!(srv.key_ids(from), BTreeSet::from(["RASTERKEYB".to_string()]));
    s3::disconnect("sv-raster-second");
}

#[test]
fn s3_saved_resolving_one_bucket_does_not_retry_another_buckets_failed_source() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_resolving_one_bucket_does_not_retry_another_buckets_failed_source") {
        return;
    }
    let srv = serve_bucket("sv-group-one", Vec::new(), Vec::new());
    let store = Arc::new(Spy::default());
    let mut seed = session(Some(&store));
    let id = saved(&mut seed, "sv-group-first", &srv.base, "", "keys");
    let other = saved(&mut seed, "sv-group-second", &srv.base, "", "keys");
    login(&mut seed, &id, "GROUPKEYA", "group-secret-a", true).unwrap();
    login(&mut seed, &other, "GROUPKEYB", "group-secret-b", true).unwrap();
    for bucket in ["sv-group-one", "sv-group-two"] {
        let path = bound(&seed, &id, &format!("s3://{bucket}/missing.fgb"));
        let source = serde_json::from_value(json!({"path": path})).unwrap();
        seed.map_mut().add_streamed(bucket, source);
    }
    let text = seed.project.saved_text(None).unwrap().0;
    let mut fresh = session(Some(&store));
    fresh.connections = Connections::from_json(&seed.connections.to_json());
    s3::disconnect("sv-group-first");
    s3::disconnect("sv-group-second");
    fresh.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(fresh.s3_pending.len(), 2);
    for bucket in ["sv-group-one", "sv-group-two"] {
        let source = fresh.s3_pending.iter().find(|p| p.bucket == bucket).unwrap().source.clone();
        let result = fresh.run("connection.resolveS3", &json!({"source": source, "id": id})).unwrap();
        assert_eq!(
            result["reopened"].as_u64().unwrap_or(0) + result["failed"].as_u64().unwrap_or(0),
            1,
            "only this bucket's missing source is retried: {result}"
        );
        let layer = fresh.map().layer_by_name(bucket).unwrap().id;
        read_view(&mut fresh, layer);
        assert!(fresh.map().layer(layer).unwrap().source.as_ref().unwrap().error.is_some());
    }
    s3::disconnect("sv-group-first");
}

#[test]
fn s3_saved_browse_reauthenticates_this_session_instead_of_reusing_a_foreign_namesake() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test(
        "s3_saved_tests::s3_saved_browse_reauthenticates_this_session_instead_of_reusing_a_foreign_namesake",
    ) {
        return;
    }
    let srv = serve_bucket("sv-session-owner", Vec::new(), Vec::new());
    let store_a = Arc::new(Spy::default());
    let store_b = Arc::new(Spy::default());
    let mut a = session(Some(&store_a));
    let id = saved(&mut a, "sv-session-owner", &srv.base, "sv-session-owner", "keys");
    login(&mut a, &id, "SESSIONKEYA", "session-secret-a", true).unwrap();
    let mut b = session(Some(&store_b));
    b.connections = Connections::from_json(&a.connections.to_json());
    login(&mut b, &id, "SESSIONKEYB", "session-secret-b", true).unwrap();
    a.run("connection.login", &json!({"id": id})).unwrap();
    let status = b.run("connection.list", &Value::Null).unwrap();
    assert_eq!(status["connections"][0]["loggedIn"], false, "another session's same-name login is not ours");
    b.run("connection.browse", &json!({"id": id, "node": "s3:sv-session-owner/"})).unwrap();
    assert_eq!(srv.key_ids(0), BTreeSet::from(["SESSIONKEYB".to_string()]));
    assert_eq!(srv.unsigned(0), 0);
    assert_eq!(s3::binding_of("sv-session-owner").as_deref(), b.s3_binding(&id));
    // An ordinary replacement invalidates the old token; it must not turn that token into
    // permission to reuse a different login by name.
    b.run(
        "s3.connect",
        &json!({"name":"sv-session-owner","endpoint":srv.base,"buckets":["sv-session-owner"],
        "accessKeyId":"REPLACEMENTKEY","secretAccessKey":"replacement-secret"}),
    )
    .unwrap();
    let status = b.run("connection.list", &Value::Null).unwrap();
    assert_eq!(status["connections"][0]["loggedIn"], false, "a stale binding never falls back to a replacement");
    let from = srv.requests();
    b.run("connection.browse", &json!({"id":id,"node":"s3:sv-session-owner/"})).unwrap();
    assert_eq!(srv.key_ids(from), BTreeSet::from(["SESSIONKEYB".to_string()]));
    s3::disconnect("sv-session-owner");
}

#[test]
fn s3_saved_favourites_keep_provenance_overhead_without_relaxing_destination_limits() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test(
        "s3_saved_tests::s3_saved_favourites_keep_provenance_overhead_without_relaxing_destination_limits",
    ) {
        return;
    }
    let endpoint = "https://s3.example.org";
    let inner = format!("{endpoint}/bucket/{}.fgb?versionId={}", "k".repeat(990), "v".repeat(1000));
    assert!(inner.len() <= 2048);
    let address = rustgis_data::s3_address::bind("0123456789abcdef", endpoint, &inner).unwrap();
    assert!(address.len() > 2048);
    let source =
        crate::sources::Source { kind: "url".into(), name: "Saved range source".into(), address: address.clone(), extra: Value::Null };
    let mut sources = crate::sources::Sources::default();
    sources.keep("favourites", source.clone()).unwrap();
    let restored = crate::sources::Sources::from_json(&sources.to_json());
    let provenance = rustgis_data::s3_address::parse(&restored.favourites[0].address).unwrap().unwrap();
    assert_eq!((provenance.endpoint, provenance.url, provenance.binding), (endpoint, inner.as_str(), ""));
    let oversized = format!("{inner}{}", "v".repeat(2049 - inner.len()));
    let too_long = rustgis_data::s3_address::bind("0123456789abcdef", endpoint, &oversized).unwrap();
    assert!(sources.keep("favourites", crate::sources::Source { address: too_long, ..source }).is_err());
}

#[test]
fn s3_saved_legacy_choice_never_rebinds_an_explicit_aws_source_for_the_same_object() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_legacy_choice_never_rebinds_an_explicit_aws_source_for_the_same_object")
    {
        return;
    }
    for aws_first in [false, true] {
        let bucket = format!("sv-mixed-{aws_first}");
        let srv = serve_bucket(&bucket, vec![("same.fgb", grid_fgb())], Vec::new());
        let store = Arc::new(Spy::default());
        let mut s = session(Some(&store));
        let custom_name = format!("{bucket}-custom");
        let custom = saved(&mut s, &custom_name, &srv.base, &bucket, "keys");
        login(&mut s, &custom, "MIXEDCUSTOMKEY", "mixed-custom-secret", true).unwrap();
        let aws_a = saved(&mut s, &format!("{bucket}-aws-a"), "", &bucket, "none");
        let aws_b = saved(&mut s, &format!("{bucket}-aws-b"), "", &bucket, "none");
        let raw = format!("s3://{bucket}/same.fgb");
        let aws = rustgis_data::s3_address::bind("", "", &raw).unwrap();
        let sources = if aws_first { [("AWS", &aws), ("Legacy", &raw)] } else { [("Legacy", &raw), ("AWS", &aws)] };
        for (name, path) in sources {
            s.map_mut().add_streamed(name, serde_json::from_value(json!({"path":path})).unwrap());
        }
        let text = s.project.saved_text(None).unwrap().0;
        s.run("project.openText", &json!({"text":text})).unwrap();
        assert_eq!(srv.requests(), 0, "neither resource is read before a choice");
        // Saving while a choice is pending must retain unknown origin, not turn it into AWS.
        let text = s.project.saved_text(None).unwrap().0;
        let definitions = s.connections.to_json();
        s = session(Some(&store));
        s.connections = Connections::from_json(&definitions);
        s.run("project.openText", &json!({"text":text})).unwrap();
        assert_eq!(srv.requests(), 0, "a pending save/reopen still fetches nothing");
        let legacy = s.s3_pending.iter().find(|p| p.endpoint.is_none()).unwrap().clone();
        let aws_pending = s.s3_pending.iter().find(|p| p.endpoint.as_deref() == Some("")).unwrap().clone();
        assert_eq!(aws_pending.candidates, vec![aws_a, aws_b]);
        let result = s.run("connection.resolveS3", &json!({"source":legacy.source,"id":custom})).unwrap();
        assert_eq!((result["reopened"].as_u64(), result["failed"].as_u64()), (Some(1), Some(0)));
        let legacy_id = s.map().layer_by_name("Legacy").unwrap().id;
        assert!(read_view(&mut s, legacy_id) > 0);
        let aws_source = s.map().layer_by_name("AWS").unwrap().source.as_ref().unwrap();
        assert_eq!(aws_source.path.as_deref(), Some(aws.as_str()), "explicit AWS provenance is unchanged");
        assert!(!aws_source.is_open());
        assert_eq!(s.s3_pending, vec![aws_pending.clone()], "AWS still requires its own choice");
        settle(&srv);
        let from = srv.requests();
        assert_eq!(
            s.run("connection.resolveS3", &json!({"source":aws_pending.source,"id":custom})).unwrap_err(),
            crate::s3_connections::STALE
        );
        crate::s3_connections::resolve_project_s3(&mut s);
        assert_eq!(s.s3_pending, vec![aws_pending]);
        assert_eq!(srv.requests(), from, "the AWS source never reaches the custom endpoint");
        s3::disconnect(&custom_name);
    }
}

#[test]
fn s3_saved_unscoped_connection_requires_a_choice_and_does_not_repeat_the_prompt() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_unscoped_connection_requires_a_choice_and_does_not_repeat_the_prompt") {
        return;
    }
    let p = saved_project("sv-unscoped");
    let mut s = reopened(&p, &p.store, 1);
    s.connections.connections[0].options.remove("buckets");
    let before = p.store.count("load");
    s.run("project.openText", &json!({"text": p.text})).unwrap();
    assert_eq!(s.s3_pending.len(), 1);
    assert_eq!(p.store.count("load"), before, "unscoped keys are never automatically read");
    let mark = s.notice_mark();
    crate::s3_connections::resolve_project_s3(&mut s);
    assert_eq!(s.s3_pending.len(), 1);
    assert!(s.notices_since(mark).is_empty(), "an unchanged pending choice is not repeated");
    assert_eq!(p.store.count("load"), before);
}

#[test]
fn s3_saved_stale_marker_cannot_skip_credential_deletion() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_stale_marker_cannot_skip_credential_deletion") {
        return;
    }
    for (index, command) in ["connection.forgetSavedKeys", "connection.remove", "connection.edit"].into_iter().enumerate() {
        let spy = Arc::new(Spy::default());
        let mut s = session(Some(&spy));
        let name = format!("sv-stale-delete-{index}");
        let id = saved(&mut s, &name, "http://127.0.0.1:9", "sv-stale-delete", "keys");
        login(&mut s, &id, "STALEKEY", "stale-secret", true).unwrap();
        // Preferences can lose the indication while the OS store retains the account.
        s.connections.set_saved_keys(&id, false);
        let before = s.connections.to_json();
        let generation = s.connections.generation;
        let raw = spy.raw(&id);
        assert!(raw.is_some());
        let params = json!({"id": id, "address": "http://127.0.0.1:10"});
        spy.refuse_delete.store(true, Ordering::Relaxed);
        assert_eq!(s.run(command, &params).unwrap_err(), s3_keys::NOT_FORGOTTEN, "{command}");
        assert_eq!(s.connections.to_json(), before);
        assert_eq!(s.connections.generation, generation);
        assert_eq!(spy.raw(&id), raw);
        assert_eq!(live_key(&name).as_deref(), Some("STALEKEY"));
        spy.refuse_delete.store(false, Ordering::Relaxed);
        s.run(command, &params).unwrap();
        assert!(spy.raw(&id).is_none(), "{command} must delete even without a saved-key indication");
        match command {
            "connection.forgetSavedKeys" => {
                assert_eq!(live_key(&name).as_deref(), Some("STALEKEY"), "forget keeps the live login");
                assert_eq!(s.run("connection.login", &json!({"id": id})).unwrap_err(), s3_keys::NONE_SAVED);
            }
            "connection.remove" => assert!(s.connections.find(&id).is_none()),
            _ => assert_eq!(s.connections.find(&id).unwrap().address, "http://127.0.0.1:10"),
        }
        s3::disconnect(&name);
    }
}

#[test]
fn s3_saved_changing_away_from_keys_forgets_the_pair() {
    #[cfg(not(target_arch = "wasm32"))]
    if crate::tests::isolate_process_test("s3_saved_tests::s3_saved_changing_away_from_keys_forgets_the_pair") {
        return;
    }
    let srv = serve_bucket("sv-mode-change", vec![], vec![]);
    let spy = Arc::new(Spy::default());
    let mut s = session(Some(&spy));
    let id = saved(&mut s, "sv-mode-change-conn", &srv.base, "sv-mode-change", "keys");
    login(&mut s, &id, "MODEKEY", "mode-secret", true).unwrap();
    assert!(spy.raw(&id).is_some());
    let r = s.run("connection.edit", &json!({"id": id, "options": {"credentials": "none", "buckets": "sv-mode-change"}})).unwrap();
    assert_eq!(r["savedKeys"], false);
    assert!(spy.raw(&id).is_none());
}
