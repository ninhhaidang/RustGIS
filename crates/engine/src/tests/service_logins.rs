//! Service logins belong to a session/connection, never an origin-global request context.
use super::*;
use crate::{render::rustgis_tiles::logins, service_logins as web};
use rustgis_data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn session(store: Option<Arc<dyn SecretStore>>) -> Session {
    let mut s = Session::new(Project::default());
    s.database_logins.set_store(store);
    s
}

fn new(s: &mut Session, p: Value) -> String {
    s.run("connection.new", &p).unwrap()["id"].as_str().unwrap().to_string()
}

fn bound(s: &mut Session, id: &str, url: &str) -> String {
    web::request(s, Some(id), url).unwrap()
}

fn header(address: &str) -> Option<(String, String)> {
    logins::apply(address).unwrap().and_then(|a| a.header)
}

fn probe_connection(kind: &str, address: &str, auth: &str, user: &str, key_name: &str) -> crate::connections::Connection {
    crate::connections::Connection {
        kind: kind.trim().into(),
        address: address.into(),
        auth: auth.into(),
        user: user.into(),
        key_name: key_name.into(),
        ..crate::connections::Connection::default()
    }
}

#[test]
fn temporary_probes_use_cleaned_basic_inputs_and_revoke_on_unwind() {
    let origin = "https://temporary-basic757.example.org";
    let url = format!("{origin}/wmts?SERVICE=WMTS&REQUEST=GetCapabilities");
    let definition = probe_connection("wmts", &format!(" {origin}/wmts "), " BASIC ", " ana ", "ignored");
    let (address, guard) = web::temporary_probe(&definition, "temporary-password-757", false, &url).unwrap();
    assert_eq!(header(&address), Some(("Authorization".into(), logins::basic("ana", "temporary-password-757"))));
    assert_eq!(logins::inner(&address), url);
    assert!(!address.contains("temporary-password-757"));
    assert!(header(&url).is_none(), "the ordinary URL must stay anonymous");
    let inherited = logins::inherit(&address, &format!("{origin}/legend"));
    assert!(header(&inherited).is_some());
    for foreign in [
        "http://temporary-basic757.example.org/legend",
        "https://temporary-basic757.example.org:444/legend",
        "https://temporary-basic757.example.org.evil.test/legend",
    ] {
        assert!(header(&logins::inherit(&address, foreign)).is_none());
    }
    let unwound = std::panic::catch_unwind(move || {
        let _guard = guard;
        panic!("simulated test cancellation");
    });
    assert!(unwound.is_err());
    assert!(logins::apply(&address).is_err());
    assert!(logins::apply(&inherited).is_err());
    assert!(rustgis_data::login_address::check(&address).is_err());
    assert_eq!(logins::scrub("temporary-password-757"), "temporary-password-757", "revocation must also discard the credential");
}

#[test]
fn temporary_probes_never_replace_saved_logins_or_connection_selections() {
    let origin = "https://temporary-saved757.example.org";
    let url = format!("{origin}/wmts");
    let mut s = session(Some(Arc::new(MemorySecrets::new())));
    let id = new(&mut s, json!({"kind":"wmts","address":url,"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"saved-temporary-757"})).unwrap();
    let saved = bound(&mut s, &id, &url);
    let definitions = s.connections.clone();
    let pending = requests(&mut s);
    assert!(web::temporary_probe(&s.connections.connections[0], "must-not-replace-saved-757", false, &url).is_err());
    let (first, first_guard) =
        web::temporary_probe(&probe_connection("wmts", &url, "header", "", " X-Key "), "first-temporary-757", false, &url).unwrap();
    let (second, second_guard) =
        web::temporary_probe(&probe_connection("wmts", &url, "header", "", "X-Key"), "second-temporary-757", false, &url).unwrap();
    assert_ne!(saved, first);
    assert_ne!(first, second, "each test needs an isolated cache identity");
    assert_eq!(header(&first).unwrap().1, "first-temporary-757");
    assert_eq!(header(&second).unwrap().1, "second-temporary-757");
    assert_eq!(header(&saved).unwrap().1, "saved-temporary-757");
    assert_eq!(web::request(&mut s, None, &url).unwrap(), saved);
    assert_eq!(web::request(&mut s, None, &first).unwrap_err(), web::NOT_THIS_SESSION);
    drop(first_guard);
    assert!(logins::apply(&first).is_err());
    assert_eq!(header(&second).unwrap().1, "second-temporary-757");
    assert_eq!(header(&saved).unwrap().1, "saved-temporary-757");
    drop(second_guard);
    assert_eq!(s.connections, definitions);
    assert_eq!(s.database_logins.selection(crate::connections::WEB, origin), Some(id.as_str()));
    assert_eq!(requests(&mut s), pending);
    let connection = s.connections.connections[0].clone();
    assert_eq!(web::known_secret(&mut s, &connection).unwrap().expose(), "saved-temporary-757");
    assert_eq!(bound(&mut s, &id, &url), saved);
}

#[test]
fn temporary_probes_reject_unsafe_definitions_credentials_and_mismatched_origins() {
    let url = "https://temporary-validation757.example.org/wmts";
    let secret = "temporary-validation-secret-757";
    for (auth, user, key) in [("basic", "ana", ""), ("header", "", "X-Key")] {
        let definition = probe_connection("wmts", url, auth, user, key);
        assert_eq!(web::temporary_probe(&definition, "", false, url).err().unwrap(), web::REQUIRED);
        assert_eq!(web::temporary_probe(&definition, "bad\nsecret", false, url).err().unwrap(), web::BAD_SECRET);
    }
    let invalid = [
        ("s3", url, "header", "", "X-Key", secret, false, url),
        ("postgres", url, "header", "", "X-Key", secret, false, url),
        ("wmts", url, "none", "", "", secret, false, url),
        ("wmts", url, "unknown-auth-secret-757", "", "", secret, false, url),
        ("wmts", url, "basic", "", "", secret, false, url),
        ("wmts", url, "basic", "bad:user", "", secret, false, url),
        ("wmts", url, "header", "", "Cookie", secret, false, url),
        ("wmts", url, "header", "", "Sec-Key", secret, false, url),
        ("wmts", url, "header", "", "X-Key", "", false, url),
        ("wmts", url, "header", "", "X-Key", "injected\r\nX-Leak: secret", false, url),
        ("wmts", url, "header", "", "X-Key", "non-ASCII-é", false, url),
        ("wmts", "https://user:secret@temporary-validation757.example.org/wmts", "header", "", "X-Key", secret, false, url),
        ("wmts", "https://temporary-validation757.example.org/wmts?token=secret", "header", "", "X-Key", secret, false, url),
        ("wmts", "https://temporary-validation757.example.org\\evil/wmts", "header", "", "X-Key", secret, false, url),
        ("wmts", url, "header", "", "X-Key", secret, false, "http://temporary-validation757.example.org/wmts"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://temporary-validation757.example.org:444/wmts"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://temporary-validation757.example.org.evil.test/wmts"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://user:secret@temporary-validation757.example.org/wmts"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://temporary-validation757.example.org/wmts?api_key=secret"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://temporary-validation757.example.org/wmts#token=secret"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://temporary-validation757.example.org/wm ts"),
        ("wmts", url, "header", "", "X-Key", secret, false, "https://temporary-validation757.example.org\\evil/wmts"),
    ];
    for (kind, address, auth, user, key, credential, insecure, probe) in invalid {
        let error = web::temporary_probe(&probe_connection(kind, address, auth, user, key), credential, insecure, probe).err().unwrap();
        assert!(!error.contains(secret));
        assert!(!error.contains("unknown-auth-secret-757"));
        assert!(!error.contains("injected"));
    }
    for (auth, user, key, credential) in [("basic", "ana", "", "x".repeat(4090)), ("header", "", "X-Key", "x".repeat(4097))] {
        assert!(web::temporary_probe(&probe_connection("wmts", url, auth, user, key), &credential, false, url).is_err());
    }
    let (wrapped, guard) = web::temporary_probe(&probe_connection("wmts", url, "header", "", "X-Key"), secret, false, url).unwrap();
    assert!(web::temporary_probe(&probe_connection("wmts", url, "header", "", "X-Key"), secret, false, &wrapped).is_err());
    drop(guard);
    let oversized = format!("{url}?filter={}", "x".repeat(17000));
    assert!(web::temporary_probe(&probe_connection("wmts", url, "header", "", "X-Key"), secret, false, &oversized).is_err());
    assert_eq!(logins::scrub(secret), secret, "a failure after activation must not leave a binding alive");
}

#[test]
fn temporary_http_probes_require_explicit_consent_each_time() {
    let url = "http://temporary-consent757.example.org/wmts";
    assert_eq!(
        web::temporary_probe(&probe_connection("wmts", url, "header", "", "X-Key"), "temporary-consent-757", false, url).err().unwrap(),
        web::INSECURE
    );
    let (address, guard) =
        web::temporary_probe(&probe_connection("wmts", url, "header", "", "X-Key"), "temporary-consent-757", true, url).unwrap();
    assert_eq!(header(&address).unwrap().1, "temporary-consent-757");
    assert!(web::temporary_probe(&probe_connection("wmts", url, "header", "", "X-Key"), "temporary-consent-757", false, url).is_err());
    assert_eq!(header(&address).unwrap().1, "temporary-consent-757", "a failed test must not replace a live test");
    drop(guard);
    assert!(logins::apply(&address).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn temporary_probe_caches_are_isolated_and_revoked_data_cannot_be_read() {
    use crate::render::rustgis_tiles::cache::{self, Payload, State, Want};
    let server = LoginFile::new(br#"{"temporaryProbe":true}"#.to_vec());
    let url = format!("{}/wmts", server.base);
    let definition = probe_connection("wmts", &url, "header", "", "X-Key");
    let (allowed, allowed_guard) = web::temporary_probe(&definition, "fixture-login-757", true, &url).unwrap();
    let (denied, denied_guard) = web::temporary_probe(&definition, "wrong-temporary-cache-757", true, &url).unwrap();
    let fetch = |address: &str| {
        let started = std::time::Instant::now();
        loop {
            match cache::get(address, Want::Json) {
                State::Pending => {
                    assert!(started.elapsed() < std::time::Duration::from_secs(10), "temporary probe fetch timed out");
                    std::thread::sleep(std::time::Duration::from_millis(2)); // test-wait: legacy polling for the authenticated probe response
                }
                state => break state,
            }
        }
    };
    match fetch(&allowed) {
        State::Ready(payload) => match payload.as_ref() {
            Payload::Json(value) => assert_eq!(value["temporaryProbe"], true),
            _ => panic!("expected the authenticated JSON response"),
        },
        _ => panic!("the typed test login did not authenticate"),
    }
    assert!(matches!(fetch(&denied), State::Failed(_)), "another test must not inherit cached authenticated data");
    assert!(matches!(fetch(&url), State::Failed(_)), "a bare URL must not inherit a temporary credential or its cache");
    let sent = server.requests.lock().unwrap().len();
    drop(allowed_guard);
    assert!(cache::peek(&allowed).is_none());
    assert!(matches!(cache::get(&allowed, Want::Json), State::Failed(_)));
    assert!(cache::download_blocking(&allowed).is_err());
    assert_eq!(server.requests.lock().unwrap().len(), sent, "revocation must fail before a network request");
    assert!(logins::check(&denied).is_ok(), "dropping one test must not revoke another");
    drop(denied_guard);
    assert!(logins::check(&denied).is_err());
}

fn project(origin: &str, id: &str) -> String {
    let mut p = Project::default();
    let mut t = rustgis_map::TileLayer::new("Secured", rustgis_map::TileKind::Raster, format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"));
    t.connection = id.into();
    p.map_mut().tiles.push(t);
    serde_json::to_string(&p).unwrap()
}

fn requests(s: &mut Session) -> Value {
    s.run("connection.list", &Value::Null).unwrap()["loginRequests"].clone()
}

#[cfg(not(target_arch = "wasm32"))]
struct LoginFile {
    base: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl LoginFile {
    /// A server that sends the whole file, whatever is asked for (as before ranges were answered).
    fn new(body: Vec<u8>) -> Self {
        Self::with(body, false)
    }

    /// A server that answers a range asked for with that part (a file read in place asks so).
    fn ranged(body: Vec<u8>) -> Self {
        Self::with(body, true)
    }

    fn with(body: Vec<u8>, ranges: bool) -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (stopping, log) = (stop.clone(), requests.clone());
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2)); // test-wait: pace nonblocking accepts so the fixture can stop
                        continue;
                    }
                    Err(_) => break,
                };
                // Accepted sockets can inherit the listener's nonblocking mode.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                while request.len() < 32768 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let cap = chunk.len().min(32768 - request.len());
                    match stream.read(&mut chunk[..cap]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&chunk[..n]),
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let authorized = request.to_ascii_lowercase().contains("\r\nx-key: fixture-login-757\r\n")
                    || request.lines().next().is_some_and(|line| line.contains("api_key=fixture-query-757 "))
                    || request.split_whitespace().nth(1).is_some_and(|p| p.starts_with("/public"));
                // A range asked for (a file read in place): that part.
                let range =
                    request.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_string)).and_then(|r| {
                        let (a, b) = r.trim().split_once('-')?;
                        let last = body.len().checked_sub(1)?;
                        if a.is_empty() {
                            let n = b.parse::<usize>().ok()?.min(body.len());
                            return Some((body.len() - n, last));
                        }
                        Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?.min(last)))
                    });
                let range = range.filter(|_| ranges);
                let head = request.to_ascii_lowercase().starts_with("head ");
                log.lock().unwrap().push(request);
                let (status, bytes) = match (authorized, range) {
                    (false, _) => ("401 Unauthorized", &[][..]),
                    (true, Some((a, b))) if a <= b => ("206 Partial Content", &body[a..=b]),
                    (true, _) => ("200 OK", body.as_slice()),
                };
                let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                // A HEAD request is answered with the length alone.
                if !head {
                    let _ = stream.write_all(bytes);
                }
            }
        });
        Self { base, requests, stop, worker: Some(worker) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for LoginFile {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_reopened_http_raster_recovers_its_pixels_after_login_without_reloading_unchanged_paths() {
    let bytes = rustgis_data::raster::cog::example(256, 256);
    let pixels = rustgis_data::raster::read_bytes("scene.tif", &bytes).unwrap();
    let server = LoginFile::new(bytes);
    let mut seed = session(None);
    let id = new(&mut seed, json!({"kind":"ogcapi","address":format!("{}/collections",server.base),"auth":"header","keyName":"X-Key"}));
    seed.run("connection.login", &json!({"id":id,"apiKey":"fixture-login-757","allowInsecure":true})).unwrap();
    let mut layer = rustgis_map::Layer::new(1, rustgis_data::Dataset::new("Secured raster", rustgis_data::GeometryType::None, Vec::new()));
    layer.connection = id.clone();
    layer.raster = Some(rustgis_map::RasterLayer::from_path(pixels, bound(&mut seed, &id, &format!("{}/scene.tif", server.base))));
    layer.transparency = 0.25;
    let style = serde_json::to_value(&layer.raster.as_ref().unwrap().style).unwrap();
    seed.map_mut().layers.push(layer);
    let text = seed.project.saved_text(None).unwrap().0;
    let mut reopened = session(None);
    reopened.connections = crate::connections::Connections::from_json(&seed.connections.to_json());
    reopened.run("project.openText", &json!({"text":text})).unwrap();
    assert!(reopened.map().layers[0].raster.as_ref().unwrap().data.is_none());
    assert_eq!(requests(&mut reopened)[0]["reason"], "password");
    let from = server.requests.lock().unwrap().len();
    reopened.run("connection.login", &json!({"id":id,"apiKey":"fixture-login-757","allowInsecure":true})).unwrap();
    let view = rustgis_geom::Extent::new(-180.0, -85.0, 180.0, 85.0);
    let scale = reopened.map().scale();
    let start = std::time::Instant::now();
    while reopened.map_mut().sync_sources(&view, scale, false) {
        assert!(start.elapsed() < std::time::Duration::from_secs(30), "raster recovery timed out");
        std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for recovered raster pixels
    }
    let layer = &reopened.map().layers[0];
    let raster = layer.raster.as_ref().unwrap();
    let pixels = raster.data.as_ref().expect("login must restart the failed raster read");
    assert_eq!((pixels.width, pixels.height), (256, 256));
    assert!(raster.error.is_none(), "{:?}", raster.error);
    assert_eq!(layer.transparency, 0.25);
    assert_eq!(serde_json::to_value(&raster.style).unwrap(), style);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[from..].len(), 1);
    assert!(requests[from].to_ascii_lowercase().contains("\r\nx-key: fixture-login-757\r\n"));
    drop(requests);
    let pixels = pixels.clone();
    let count = server.requests.lock().unwrap().len();
    for _ in 0..3 {
        web::resolve(&mut reopened, None);
        assert!(!reopened.map_mut().sync_sources(&view, scale, false));
    }
    assert!(Arc::ptr_eq(&pixels, reopened.map().layers[0].raster.as_ref().unwrap().data.as_ref().unwrap()));
    assert_eq!(server.requests.lock().unwrap().len(), count);
}

#[test]
fn a_reopened_dated_tile_authenticates_its_recorded_origin_not_the_displayed_foreign_origin() {
    let (a_origin, b_origin) = ("https://dates-a757.example.org", "https://dates-b757.example.org");
    let mut seed = session(None);
    let a = new(&mut seed, json!({"kind":"wmts","address":a_origin,"auth":"header","keyName":"X-Key"}));
    let b = new(&mut seed, json!({"kind":"wmts","address":b_origin,"auth":"header","keyName":"X-Key"}));
    let mut tile = rustgis_map::TileLayer::new("Dates", rustgis_map::TileKind::Raster, format!("{b_origin}/b/{{z}}/{{x}}/{{y}}.png"));
    tile.connection = a.clone();
    tile.time_urls = vec![(1704067200000.0, format!("{a_origin}/a/{{z}}/{{x}}/{{y}}.png")), (1704153600000.0, tile.url.clone())];
    seed.map_mut().tiles.push(tile);
    seed.run("time.set", &json!({"dates":["2024-01-01","2024-01-02"],"date":"2024-01-02"})).unwrap();
    let text = seed.project.saved_text(None).unwrap().0;
    let mut reopened = session(None);
    reopened.connections = crate::connections::Connections::from_json(&seed.connections.to_json());
    reopened.run("connection.login", &json!({"id":b,"apiKey":"dated-b-757"})).unwrap();
    reopened.run("project.openText", &json!({"text":text})).unwrap();
    let pending = requests(&mut reopened);
    assert_eq!(pending.as_array().unwrap().len(), 1, "{pending}");
    assert_eq!(pending[0]["origin"], a_origin);
    assert_eq!(pending[0]["connections"], json!([a]));
    assert_eq!(pending[0]["layers"], 1, "the dated addresses count as one tile");
    assert!(header(&reopened.map().tiles[0].url).is_none());
    reopened.run("connection.login", &json!({"id":a,"apiKey":"dated-a-757"})).unwrap();
    assert_eq!(requests(&mut reopened), json!([]));
    assert!(header(&reopened.map().tiles[0].url).is_none(), "B stays anonymous even with B logged in");
    reopened.run("time.set", &json!({"date":"2024-01-01"})).unwrap();
    assert_eq!(header(&reopened.map().tiles[0].url).unwrap().1, "dated-a-757");
    reopened.run("time.set", &json!({"date":"2024-01-02"})).unwrap();
    assert!(header(&reopened.map().tiles[0].url).is_none());
    assert_eq!(reopened.map().tiles[0].connection, a);
}

#[test]
fn a_missing_dated_tiles_connection_asks_only_about_its_displayed_origin() {
    let mut s = session(None);
    let b = new(&mut s, json!({"kind":"wmts","address":"https://missing-dates-b757.example.org","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":b,"apiKey":"missing-b-757"})).unwrap();
    let mut p = Project::default();
    let mut tile = rustgis_map::TileLayer::new(
        "Unknown dates",
        rustgis_map::TileKind::Raster,
        "https://missing-dates-b757.example.org/b/{z}/{x}/{y}.png",
    );
    tile.connection = "gone".into();
    tile.time_urls =
        vec![(1704067200000.0, "https://missing-dates-a757.example.org/a/{z}/{x}/{y}.png".into()), (1704153600000.0, tile.url.clone())];
    p.map_mut().tiles.push(tile);
    let mut seed = Session::new(p);
    seed.run("time.set", &json!({"dates":["2024-01-01","2024-01-02"],"date":"2024-01-02"})).unwrap();
    let text = seed.project.saved_text(None).unwrap().0;
    s.run("project.openText", &json!({"text":text})).unwrap();
    let pending = requests(&mut s);
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["reason"], "connection");
    assert_eq!(pending[0]["origin"], "https://missing-dates-b757.example.org");
    assert!(header(&s.map().tiles[0].url).is_none());
    assert_eq!(s.map().tiles[0].connection, "gone");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_oversized_ogc_items_binding_is_refused_before_native_fetch() {
    use crate::render::rustgis_tiles::cache;
    let server = LoginFile::new(br#"{"type":"FeatureCollection","features":[]}"#.to_vec());
    let public = LoginFile::new(b"public".to_vec());
    assert_ne!(server.base, public.base, "the fixtures need different origins");
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"ogcapi","address":format!("{}/collections",server.base),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"fixture-login-757","allowInsecure":true})).unwrap();
    let address = bound(&mut s, &id, &format!("{}/collections", server.base));
    let long = format!("{}/items?filter={}&limit=10&f=json", server.base, "a".repeat(17000));
    let document = serde_json::to_vec(&json!({"collections":[
        {"id":"long","links":[{"rel":"items","type":"application/geo+json","href":long}]},
        {"id":"valid","links":[{"rel":"items","type":"application/geo+json","href":format!("{}/items?limit=10",server.base)}]},
        {"id":"foreign","links":[{"rel":"items","type":"application/geo+json","href":format!("{}/public?limit=10",public.base)}]}
    ]}))
    .unwrap();
    let service = rustgis_data::ogc::parse(&address, &document).unwrap();
    let items = |id: &str| match &service.layers.iter().find(|l| l.name == id).unwrap().offer {
        rustgis_data::ogc::Offer::Features { url } => url.as_str(),
        _ => panic!("feature offer"),
    };
    let oversized = items("long");
    assert!(rustgis_data::login_address::is_wrapper(oversized));
    assert!(rustgis_data::login_address::parse(oversized).is_err());
    assert!(cache::download_blocking(oversized).is_err());
    assert!(server.requests.lock().unwrap().is_empty(), "an oversized binding must never fetch anonymously");
    assert!(cache::download_blocking(items("valid")).is_ok());
    assert!(server.requests.lock().unwrap()[0].to_ascii_lowercase().contains("\r\nx-key: fixture-login-757\r\n"));
    let foreign = items("foreign");
    assert!(!rustgis_data::login_address::is_wrapper(foreign));
    assert_eq!(foreign, format!("{}/public?limit=10", public.base));
    let downloaded = cache::download_blocking(foreign).unwrap_or_else(|_| {
        // Only request structure is diagnostic: never print headers, targets or error text.
        let summarize = |request: &String| {
            let mut words = request.split_whitespace();
            let method = match words.next() {
                Some("GET") => "GET",
                Some("CONNECT") => "CONNECT",
                Some(_) => "other",
                None => "missing",
            };
            let target = words.next().unwrap_or("");
            let form = match target {
                "" => "missing",
                t if t.starts_with('/') => "origin",
                t if t.starts_with("http://") || t.starts_with("https://") => "absolute",
                _ if method == "CONNECT" => "authority",
                _ => "other",
            };
            json!({
                "bytes": request.len(), "fullHeader": request.contains("\r\n\r\n"),
                "method": method, "targetForm": form, "publicPrefix": target.starts_with("/public"),
                "xKeyPresent": request.to_ascii_lowercase().contains("\r\nx-key:"),
            })
        };
        let protected = server.requests.lock().unwrap().iter().map(&summarize).collect::<Vec<_>>();
        let anonymous = public.requests.lock().unwrap().iter().map(&summarize).collect::<Vec<_>>();
        panic!("foreign fixture download failed; protected={protected:?}; public={anonymous:?}");
    });
    assert_eq!(downloaded, b"public");
    assert!(!public.requests.lock().unwrap()[0].to_ascii_lowercase().contains("\r\nx-key:"));
}

#[test]
fn failed_same_origin_rebinding_keeps_the_old_address_and_reports_its_count() {
    let mut s = session(None);
    let origin = "https://rebind-limit757.example.org";
    let id = new(&mut s, json!({"kind":"wmts","address":origin,"auth":"header","keyName":"X-Key"}));
    let long = format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png?filter={}", "a".repeat(17000));
    let mut tile = rustgis_map::TileLayer::new("Long", rustgis_map::TileKind::Raster, &long);
    tile.connection = id.clone();
    s.map_mut().tiles.push(tile);
    let mark = s.notice_mark();
    s.run("connection.login", &json!({"id":id,"apiKey":"rebind-limit-757"})).unwrap();
    assert_eq!(s.map().tiles[0].url, long);
    let notices = s.notices_since(mark);
    assert_eq!(notices.len(), 1);
    let counts: Vec<usize> = notices[0].text.split_whitespace().filter_map(|word| word.parse().ok()).collect();
    assert_eq!(counts, [1], "count failed rebinding without treating foreign-origin anonymity as an error");
    s.map_mut().tiles[0].url = "https://foreign-rebind757.example.org/tiles/{z}/{x}/{y}.png".into();
    let mark = s.notice_mark();
    s.run("connection.login", &json!({"id":id,"apiKey":"rebind-limit-757"})).unwrap();
    assert!(s.notices_since(mark).is_empty());
}

#[test]
fn web_login_goes_only_to_its_connections_origin() {
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wms","address":"https://scope757.example.org/wms","auth":"header","keyName":"X-Api-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"scope-sentinel-757"})).unwrap();
    // A raw URL is never authority to use a service login, even at its origin.
    assert!(header("https://scope757.example.org/wms").is_none());
    let address = bound(&mut s, &id, "https://SCOPE757.example.org:443/tiles/1/2/3.png");
    assert_eq!(header(&address).unwrap(), ("X-Api-Key".into(), "scope-sentinel-757".into()));
    for other in [
        "https://other757.example.org/wms",
        "https://scope757.example.org.evil.example/wms",
        "https://scope757.example.org:8443/wms",
        "http://scope757.example.org/wms",
    ] {
        assert!(header(&bound(&mut s, &id, other)).is_none(), "{other}");
        assert!(header(&logins::inherit(&address, other)).is_none(), "{other}");
    }
    s.run("map.addTiles", &json!({"url":"https://scope757.example.org/tiles/{z}/{x}/{y}.png","name":"Named","connection":id})).unwrap();
    // Replacement has the same (URL, name), but the newly created layer still needs provenance.
    s.run("map.addTiles", &json!({"url":"https://scope757.example.org/tiles/{z}/{x}/{y}.png","name":"Named","connection":id})).unwrap();
    let tile = s.map().tiles.iter().find(|t| t.name == "Named").unwrap();
    assert_eq!(tile.connection, id);
    assert_eq!(header(&tile.url).unwrap().1, "scope-sentinel-757");
    let previous = tile.url.clone();
    s.run("connection.logout", &json!({"id":id})).unwrap();
    assert!(logins::apply(&previous).is_err());
    assert_eq!(web::request(&mut s, Some(&id), "https://scope757.example.org/wms").unwrap_err(), web::LOGGED_OUT);
}

#[test]
fn web_login_methods_and_encoded_basic_size_are_checked() {
    let mut s = session(None);
    for bad in [
        json!({"kind":"wms","address":"https://m757.example.org","auth":"digest"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"query"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"query","keyName":"bad name"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"query","keyName":"a=b"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"query","keyName":"x".repeat(65)}),
        json!({"kind":"s3","auth":"query","keyName":"key"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"basic"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"basic","user":"a:b"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"header","keyName":"Cookie"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"header","keyName":"X Key"}),
        json!({"kind":"s3","auth":"header","keyName":"X-Key"}),
    ] {
        assert!(s.run("connection.new", &bad).is_err(), "{bad}");
    }
    let id = new(&mut s, json!({"kind":"arcgis","address":"https://m757.example.org/rest/services","auth":"basic","user":"ana"}));
    s.run("connection.login", &json!({"id":id,"password":"pw-757"})).unwrap();
    let address = bound(&mut s, &id, "https://m757.example.org/rest/services");
    assert_eq!(header(&address).unwrap(), ("Authorization".into(), logins::basic("ana", "pw-757")));
    for bad in [
        json!({"id":id,"apiKey":"do-not-echo-757"}),
        json!({"id":id,"password":"bad\nline"}),
        json!({"id":id,"password":"x".repeat(3070)}),
        json!({"id":id,"password":"pw-757","allowInsecure":"yes"}),
    ] {
        let error = s.run("connection.login", &bad).unwrap_err();
        assert!(!error.contains("do-not-echo-757") && !error.contains("bad\nline") && !error.contains("pw-757"));
        assert_eq!(header(&address).unwrap().1, logins::basic("ana", "pw-757"));
    }
}

#[test]
fn plain_http_confirmation_applies_only_to_this_sessions_login_identity() {
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wfs","address":"http://http757.example.org/wfs","auth":"basic","user":"ana"}));
    assert_eq!(s.run("connection.login", &json!({"id":id,"password":"http-sentinel-757"})).unwrap_err(), web::INSECURE);
    assert!(!s.database_logins.has_session_password(&id));
    s.run("connection.login", &json!({"id":id,"password":"http-sentinel-757","allowInsecure":true})).unwrap();
    assert!(header(&bound(&mut s, &id, "http://http757.example.org/features")).is_some());
    assert!(header(&bound(&mut s, &id, "https://http757.example.org/features")).is_none());
    // Reuse within this session needs no new confirmation; a later session has no credential.
    s.run("connection.login", &json!({"id":id})).unwrap();
    let prefs = s.connections.to_json();
    let mut later = session(None);
    later.connections = crate::connections::Connections::from_json(&prefs);
    assert_eq!(later.run("connection.login", &json!({"id":id})).unwrap_err(), web::REQUIRED);
    s.run("connection.logout", &json!({"id":id})).unwrap();
    assert_eq!(s.run("connection.login", &json!({"id":id,"password":"http-sentinel-757"})).unwrap_err(), web::INSECURE);
    s.run("connection.login", &json!({"id":id,"password":"http-sentinel-757","allowInsecure":true})).unwrap();
    s.run("connection.edit", &json!({"id":id,"address":"http://http757.example.org:8080/wfs"})).unwrap();
    assert_eq!(s.run("connection.login", &json!({"id":id})).unwrap_err(), web::REQUIRED);
}

#[test]
fn service_login_edit_and_remove_do_not_depend_on_a_credential_store() {
    for store in [
        None,
        Some(Arc::new(MemorySecrets::failing(SecretError::Denied)) as Arc<dyn SecretStore>),
        Some(Arc::new(MemorySecrets::failing(SecretError::Unavailable("synthetic refusal".into()))) as Arc<dyn SecretStore>),
    ] {
        let mut s = session(store);
        let id = new(&mut s, json!({"kind":"ogcapi","address":"https://memory757.example.org/ogc","auth":"header","keyName":"X-Key"}));
        let result = s.run("connection.login", &json!({"id":id,"apiKey":"memory-sentinel-757","remember":true})).unwrap();
        assert_eq!(result["warning"], web::NOT_REMEMBERED);
        assert!(s.database_logins.has_session_password(&id));
        assert!(!s.connections.find(&id).unwrap().remembered);
        let address = bound(&mut s, &id, "https://memory757.example.org/features");
        assert_eq!(header(&address).unwrap().1, "memory-sentinel-757");
        let duplicate = s.run("connection.duplicate", &json!({"id":id,"name":"Another account"})).unwrap();
        assert_eq!(duplicate["loggedIn"], false);
        assert_eq!(s.run("connection.test", &json!({"id":duplicate["id"],"probe":true})).unwrap_err(), web::REQUIRED);
        s.run("connection.edit", &json!({"id":id,"name":"Renamed","address":"https://memory757.example.org/v2"})).unwrap();
        assert_eq!(header(&address).unwrap().1, "memory-sentinel-757");
        s.run("connection.edit", &json!({"id":id,"address":"https://changed-memory757.example.org/ogc"})).unwrap();
        assert!(logins::apply(&address).is_err());
        s.run("connection.login", &json!({"id":id,"apiKey":"replacement-memory-757"})).unwrap();
        let replacement = bound(&mut s, &id, "https://changed-memory757.example.org/features");
        s.run("connection.remove", &json!({"id":id})).unwrap();
        assert!(logins::apply(&replacement).is_err());
    }
}

/// Count store access without consulting a real credential store.
#[derive(Default)]
struct CountingStore {
    inner: MemorySecrets,
    reads: AtomicUsize,
    saves: AtomicUsize,
    deletes: AtomicUsize,
    refuse_save: AtomicBool,
    refuse_delete: AtomicBool,
    compete_on_delete: AtomicBool,
}

impl SecretStore for CountingStore {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.load(account)
    }

    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        if self.refuse_save.load(Ordering::SeqCst) {
            return Err(SecretError::Denied);
        }
        self.inner.save(account, value)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        if self.compete_on_delete.load(Ordering::SeqCst) {
            assert!(!rustgis_data::address::note_login_query_name("zq_competing_1979"));
        }
        if self.refuse_delete.load(Ordering::SeqCst) {
            return Err(SecretError::Denied);
        }
        self.inner.delete(account)
    }
}

#[test]
fn remembered_query_logins_recover_projects_through_the_opt_in_preferences_path() {
    use rustgis_data::diagnostics::{Filter, History};
    let origin = "https://remember-query1979.example.org";
    let key = "remember-query/1979 é";
    let encoded = "remember-query%2F1979%20%C3%A9";
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wmts","address":format!("{origin}/wmts"),"auth":"query","keyName":"api_key"}));
    let account = format!("connection/{id}");
    let history = History::default();
    let _scope = history.enter();
    s.run("connection.login", &json!({"id":id,"apiKey":key})).unwrap();
    assert_eq!(store.saves.load(Ordering::SeqCst), 0, "remember is off by default");
    assert_eq!(store.inner.load(&account).unwrap(), None);
    s.run("connection.login", &json!({"id":id,"apiKey":key,"remember":true})).unwrap();
    assert!(s.connections.find(&id).unwrap().remembered);
    let duplicate = s.run("connection.duplicate", &json!({"id":id,"name":"Unremembered copy"})).unwrap();
    assert_eq!(duplicate["remembered"], false);
    assert_eq!(store.inner.load(&format!("connection/{}", duplicate["id"].as_str().unwrap())).unwrap(), None);
    s.run("project.openText", &json!({"text":project(origin,&id)})).unwrap();
    assert_eq!(requests(&mut s), json!([]));
    let previous = s.map().tiles[0].url.clone();
    assert!(logins::apply(&previous).unwrap().unwrap().url.ends_with(&format!("?api_key={encoded}")));
    let prefs = s.connections.to_json();
    let (saved_project, _) = s.project.saved_text(None).unwrap();
    let mut recording = Vec::new();
    s.log.write(&mut recording, false).unwrap();
    let visible = format!(
        "{prefs}\n{saved_project}\n{}\n{:?}\n{}\n{:?}",
        String::from_utf8(recording).unwrap(),
        history.records(),
        history.report(Filter::All),
        s.notices
    );
    for private in [key, encoded, "remember-query/1979", "rustgis-login"] {
        assert!(!visible.contains(private), "{private} reached saved text, recording or History: {visible}");
    }
    drop(s);
    assert!(logins::apply(&previous).is_err());
    // CLI and Python use this same explicit opt-in path. Without it the marker is not authority.
    let mut no_store = session(None);
    no_store.load_connections(&prefs);
    assert_eq!(web::request(&mut no_store, Some(&id), &format!("{origin}/wmts")).unwrap_err(), web::UNAVAILABLE);
    let saved = serde_json::Map::from_iter([(crate::connections::PREF.to_string(), Value::String(prefs))]);
    let mut later = session(None);
    crate::prefs::use_saved_secrets(&mut later, &saved, store.clone());
    let reads = store.reads.load(Ordering::SeqCst);
    later.run("project.openText", &json!({"text":saved_project})).unwrap();
    assert_eq!(store.reads.load(Ordering::SeqCst), reads + 1);
    assert_eq!(requests(&mut later), json!([]));
    let recovered = later.map().tiles[0].url.clone();
    let applied = logins::apply(&recovered).unwrap().unwrap();
    assert_eq!(applied.url, format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png?api_key={encoded}"));
    assert!(applied.header.is_none());
    for foreign in [
        "https://elsewhere1979.example.org/wmts",
        "https://remember-query1979.example.org:8443/wmts",
        "http://remember-query1979.example.org/wmts",
    ] {
        assert!(logins::apply(&bound(&mut later, &id, foreign)).unwrap().is_none());
    }
    later.run("connection.forgetLogin", &json!({"id":id})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), None);
    assert!(!later.connections.find(&id).unwrap().remembered);
    assert!(later.database_logins.has_session_password(&id));
    assert_eq!(bound(&mut later, &id, &format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png")), recovered);
    assert_eq!(logins::apply(&recovered).unwrap().unwrap().url, applied.url);
    let mut forgotten = session(Some(store));
    forgotten.load_connections(&later.connections.to_json());
    assert_eq!(web::request(&mut forgotten, Some(&id), &format!("{origin}/wmts")).unwrap_err(), web::REQUIRED);
}

#[test]
fn remembered_http_consent_stays_bound_to_the_exact_origin() {
    let store = Arc::new(MemorySecrets::new());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wfs","address":"http://remember-http1979.example.org/wfs","auth":"basic","user":"ana"}));
    let account = format!("connection/{id}");
    assert_eq!(s.run("connection.login", &json!({"id":id,"password":"remember-http-1979","remember":true})).unwrap_err(), web::INSECURE);
    assert!(!s.database_logins.has_session_password(&id));
    assert_eq!(store.load(&account).unwrap(), None);
    s.run("connection.login", &json!({"id":id,"password":"remember-http-1979","remember":true,"allowInsecure":true})).unwrap();
    let mut later = session(Some(store.clone()));
    later.load_connections(&s.connections.to_json());
    let url = bound(&mut later, &id, "http://remember-http1979.example.org/features");
    assert_eq!(header(&url).unwrap().1, logins::basic("ana", "remember-http-1979"));
    assert!(header(&bound(&mut later, &id, "https://remember-http1979.example.org/features")).is_none());
    later.run("connection.edit", &json!({"id":id,"address":"http://remember-http1979.example.org:8080/wfs"})).unwrap();
    assert!(logins::apply(&url).is_err());
    assert_eq!(store.load(&account).unwrap(), None);
    assert_eq!(later.run("connection.login", &json!({"id":id})).unwrap_err(), web::REQUIRED);
    assert_eq!(later.run("connection.login", &json!({"id":id,"password":"remember-http-1979"})).unwrap_err(), web::INSECURE);
}

#[test]
fn remembered_logins_reject_every_changed_identity_including_query_header_method_swaps() {
    for (auth, changed) in [
        ("query", json!({"auth":"header"})),
        ("header", json!({"auth":"query"})),
        ("query", json!({"keyName":"other_key"})),
        ("header", json!({"keyName":"X-Other-Key"})),
        ("basic", json!({"user":"other-user"})),
        ("query", json!({"address":"https://moved-remember1979.example.org/wms"})),
        ("query", json!({"address":"https://identity-remember1979.example.org:8443/wms"})),
        ("query", json!({"address":"http://identity-remember1979.example.org/wms"})),
    ] {
        let store = Arc::new(CountingStore::default());
        let mut s = session(Some(store.clone()));
        let id = new(
            &mut s,
            json!({"kind":"wms","address":"https://identity-remember1979.example.org/wms","auth":auth,"user":"ana","keyName":"X-Key"}),
        );
        let credential = if auth == "basic" { "password" } else { "apiKey" };
        s.run("connection.login", &json!({"id":id,credential:"identity-remember-1979","remember":true})).unwrap();
        let mut prefs: Value = serde_json::from_str(&s.connections.to_json()).unwrap();
        for (name, value) in changed.as_object().unwrap() {
            prefs["connections"][0][name] = value.clone();
        }
        let mut later = session(Some(store.clone()));
        later.load_connections(&prefs.to_string());
        let c = later.connections.find(&id).unwrap().clone();
        let reads = store.reads.load(Ordering::SeqCst);
        assert_eq!(web::request(&mut later, Some(&id), &c.address).unwrap_err(), web::SAVED_UNUSABLE, "{auth}: {changed}");
        assert_eq!(store.reads.load(Ordering::SeqCst), reads + 1);
        assert!(!later.database_logins.has_session_password(&id));
        assert!(!web::logged_in(&later, &c));
        assert!(header(&c.address).is_none());
        assert_eq!(web::known_secret(&mut later, &c).unwrap_err(), web::SAVED_UNUSABLE);
        assert_eq!(store.reads.load(Ordering::SeqCst), reads + 1);
    }
}

#[test]
fn failed_saved_login_reads_are_cached_until_explicit_login() {
    for failure in [None, Some(SecretError::Denied), Some(SecretError::Unavailable("store-error-must-not-escape-1979".into()))] {
        let inner = failure.clone().map_or_else(MemorySecrets::new, MemorySecrets::failing);
        let store = Arc::new(CountingStore { inner, ..CountingStore::default() });
        let origin = "https://retry-remember1979.example.org";
        let mut s = session(Some(store.clone()));
        let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
        let mut prefs: Value = serde_json::from_str(&s.connections.to_json()).unwrap();
        prefs["connections"][0]["savedLogin"] = json!(true);
        let expected = if failure.is_some() {
            web::UNAVAILABLE
        } else {
            store.inner.save(&format!("connection/{id}"), &Secret::new("damaged-envelope-1979")).unwrap();
            web::SAVED_UNUSABLE
        };
        s.load_connections(&prefs.to_string());
        let mark = s.notice_mark();
        s.run("project.openText", &json!({"text":project(origin,&id)})).unwrap();
        assert_eq!(store.reads.load(Ordering::SeqCst), 1);
        assert_eq!(s.notices_since(mark).iter().filter(|n| n.text.contains(expected)).count(), 1);
        let notices = s.notices.len();
        let c = s.connections.find(&id).unwrap().clone();
        for _ in 0..3 {
            web::resolve(&mut s, None);
            assert_eq!(requests(&mut s)[0]["reason"], "password");
            assert_eq!(web::request(&mut s, Some(&id), &format!("{origin}/wms")).unwrap_err(), expected);
            assert_eq!(web::known_secret(&mut s, &c).unwrap_err(), expected);
        }
        assert_eq!(store.reads.load(Ordering::SeqCst), 1);
        assert_eq!(s.notices.len(), notices);
        assert!(!format!("{:?}", s.notices).contains("store-error-must-not-escape-1979"));
        assert_eq!(s.run("connection.login", &json!({"id":id})).unwrap_err(), expected);
        assert_eq!(store.reads.load(Ordering::SeqCst), 2, "explicit login retries once");
        s.run("connection.login", &json!({"id":id,"apiKey":"replacement-retry-1979"})).unwrap();
        assert_eq!(header(&bound(&mut s, &id, &format!("{origin}/wms"))).unwrap().1, "replacement-retry-1979");
        assert_eq!(store.reads.load(Ordering::SeqCst), 2, "a typed login never needs the failing store");
    }
}

#[test]
fn a_failed_saved_login_activation_does_not_poison_session_recovery() {
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":"http://activation1979.example/wms","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"activation-secret","remember":true,"allowInsecure":true})).unwrap();
    let mut later = session(Some(store));
    later.load_connections(&s.connections.to_json());
    let c = later.connections.find(&id).unwrap().clone();
    let secret = web::known_secret(&mut later, &c).unwrap();
    assert!(web::activate(&mut later, &c, Some(&secret), false).is_err());
    assert!(!later.database_logins.has_session_password(&id));
    let retry = web::known_secret(&mut later, &c).unwrap();
    web::activate(&mut later, &c, Some(&retry), true).unwrap();
    assert_eq!(header(&bound(&mut later, &id, &c.address)).unwrap().1, "activation-secret");
}

#[test]
fn a_missing_saved_login_clears_its_marker_and_logout_never_automatically_reloads() {
    let store = Arc::new(CountingStore::default());
    let origin = "https://sticky-remember1979.example.org";
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"sticky-remember-1979","remember":true})).unwrap();
    let prefs = s.connections.to_json();
    let mut later = session(Some(store.clone()));
    later.load_connections(&prefs);
    later.run("project.openText", &json!({"text":project(origin,&id)})).unwrap();
    let previous = later.map().tiles[0].url.clone();
    assert_eq!(header(&previous).unwrap().1, "sticky-remember-1979");
    later.run("connection.logout", &json!({"id":id})).unwrap();
    assert!(logins::apply(&previous).is_err());
    assert!(!later.database_logins.has_session_password(&id));
    assert!(later.connections.find(&id).unwrap().remembered);
    assert!(store.inner.load(&format!("connection/{id}")).unwrap().is_some());
    let reads = store.reads.load(Ordering::SeqCst);
    for _ in 0..3 {
        web::resolve(&mut later, None);
        assert_eq!(requests(&mut later)[0]["reason"], "password");
        assert_eq!(web::request(&mut later, Some(&id), &format!("{origin}/wms")).unwrap_err(), web::LOGGED_OUT);
        assert!(later.run("connection.browse", &json!({"id":id,"probe":true})).is_err());
        assert!(later.run("connection.test", &json!({"id":id,"probe":true})).is_err());
    }
    assert_eq!(store.reads.load(Ordering::SeqCst), reads);
    later.run("connection.login", &json!({"id":id})).unwrap();
    assert_eq!(store.reads.load(Ordering::SeqCst), reads + 1);
    assert_eq!(header(&later.map().tiles[0].url).unwrap().1, "sticky-remember-1979");
    store.inner.delete(&format!("connection/{id}")).unwrap();
    let mut missing = session(Some(store.clone()));
    missing.load_connections(&prefs);
    assert_eq!(missing.run("connection.login", &json!({"id":id})).unwrap_err(), web::NONE_SAVED);
    assert!(!missing.connections.find(&id).unwrap().remembered);
    let reads = store.reads.load(Ordering::SeqCst);
    web::resolve(&mut missing, None);
    assert_eq!(store.reads.load(Ordering::SeqCst), reads);
}

#[test]
fn saved_login_deletion_failures_leave_definition_generation_binding_and_session_unchanged() {
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(
        &mut s,
        json!({"kind":"wms","address":"https://atomic-remember1979.example.org/wms","auth":"header","keyName":"X-Key","user":"ana"}),
    );
    s.run("connection.login", &json!({"id":id,"apiKey":"atomic-remember-1979","remember":true})).unwrap();
    let account = format!("connection/{id}");
    let saved = store.inner.load(&account).unwrap();
    let previous = bound(&mut s, &id, "https://atomic-remember1979.example.org/tiles");
    let deletes = store.deletes.load(Ordering::SeqCst);
    s.run("connection.edit", &json!({"id":id,"name":"Renamed","address":"https://ATOMIC-REMEMBER1979.example.org:443/v2"})).unwrap();
    assert_eq!(store.deletes.load(Ordering::SeqCst), deletes);
    assert_eq!(header(&previous).unwrap().1, "atomic-remember-1979");
    let definition = s.connections.find(&id).unwrap().clone();
    let generation = s.connections.generation;
    store.refuse_delete.store(true, Ordering::SeqCst);
    for (command, params) in [
        ("connection.edit", json!({"id":id,"address":"https://moved-atomic1979.example.org/wms"})),
        ("connection.edit", json!({"id":id,"auth":"query"})),
        ("connection.edit", json!({"id":id,"keyName":"X-Other"})),
        ("connection.forgetLogin", json!({"id":id})),
        ("connection.remove", json!({"id":id})),
    ] {
        assert_eq!(s.run(command, &params).unwrap_err(), web::NOT_FORGOTTEN);
        assert_eq!(s.connections.find(&id).unwrap(), &definition);
        assert_eq!(s.connections.generation, generation);
        assert!(s.database_logins.has_session_password(&id));
        assert_eq!(store.inner.load(&account).unwrap(), saved);
        assert_eq!(header(&previous).unwrap().1, "atomic-remember-1979");
    }
    store.refuse_delete.store(false, Ordering::SeqCst);
    s.run("connection.remove", &json!({"id":id})).unwrap();
    assert!(s.connections.find(&id).is_none());
    assert_eq!(store.inner.load(&account).unwrap(), None);
    assert!(logins::apply(&previous).is_err());
}

/// An identity edit refused because this run's query-name registry is full never deletes the
/// saved login first: the edit changes nothing, and the remembered login still loads.
#[test]
fn a_full_query_name_registry_refuses_an_identity_edit_before_deleting_the_saved_login() {
    #[cfg(not(target_arch = "wasm32"))]
    if super::isolate_process_test(
        "tests::service_logins::a_full_query_name_registry_refuses_an_identity_edit_before_deleting_the_saved_login",
    ) {
        return;
    }
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let origin = "https://full-registry1979.example.org";
    let id = new(&mut s, json!({"kind":"wmts","address":format!("{origin}/wmts"),"auth":"query","keyName":"api_key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"full-registry-1979","remember":true})).unwrap();
    let account = format!("connection/{id}");
    let saved = store.inner.load(&account).unwrap();
    let definition = s.connections.find(&id).unwrap().clone();
    let generation = s.connections.generation;
    let deletes = store.deletes.load(Ordering::SeqCst);
    let mut filler = 0;
    while rustgis_data::address::note_login_query_name(&format!("zqfullregistry{filler}")) {
        filler += 1;
    }
    let e = s.run("connection.edit", &json!({"id":id,"keyName":"another_unheld_key"})).unwrap_err();
    assert_eq!(e, crate::render::rustgis_tiles::logins::TOO_MANY_QUERY_NAMES);
    assert_eq!(store.deletes.load(Ordering::SeqCst), deletes, "the saved login wasn't touched");
    assert_eq!(store.inner.load(&account).unwrap(), saved);
    assert_eq!(s.connections.find(&id).unwrap(), &definition);
    assert_eq!(s.connections.generation, generation);
    let mut later = session(Some(store));
    later.load_connections(&s.connections.to_json());
    let applied = logins::apply(&bound(&mut later, &id, &format!("{origin}/wmts"))).unwrap().unwrap();
    assert!(applied.url.ends_with("api_key=full-registry-1979"), "{}", applied.url);
}

#[test]
fn failed_replacement_remember_keeps_the_old_saved_login_and_the_new_session_login() {
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":"https://replace-remember1979.example.org/wms","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"saved-replace-1979","remember":true})).unwrap();
    let old = bound(&mut s, &id, "https://replace-remember1979.example.org/wms");
    let saved = store.inner.load(&format!("connection/{id}")).unwrap();
    store.refuse_save.store(true, Ordering::SeqCst);
    let result = s.run("connection.login", &json!({"id":id,"apiKey":"session-replace-1979","remember":true})).unwrap();
    assert_eq!(result["warning"], web::NOT_REMEMBERED);
    assert!(s.connections.find(&id).unwrap().remembered);
    assert!(logins::apply(&old).is_err());
    assert_eq!(header(&bound(&mut s, &id, "https://replace-remember1979.example.org/wms")).unwrap().1, "session-replace-1979");
    assert_eq!(store.inner.load(&format!("connection/{id}")).unwrap(), saved);
    let mut later = session(Some(store));
    later.load_connections(&s.connections.to_json());
    assert_eq!(header(&bound(&mut later, &id, "https://replace-remember1979.example.org/wms")).unwrap().1, "saved-replace-1979");
}

#[test]
fn query_name_reservations_survive_competing_acceptance_and_release_on_failed_deletion() {
    #[cfg(not(target_arch = "wasm32"))]
    if super::isolate_process_test(
        "tests::service_logins::query_name_reservations_survive_competing_acceptance_and_release_on_failed_deletion",
    ) {
        return;
    }
    use rustgis_data::address::{note_login_query_name, reserve_login_query_name};
    let shared = reserve_login_query_name("zq_shared_1979").unwrap();
    let abandoned = reserve_login_query_name("zq_shared_1979").unwrap();
    shared.commit();
    drop(abandoned);
    let mut url = "https://example.org/?zq_shared_1979=private".to_string();
    rustgis_data::address::redact_query(&mut url);
    assert!(!url.contains("private"));

    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":"https://transaction1979.example/wms","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"transaction-secret","remember":true})).unwrap();
    let saved = store.inner.load(&format!("connection/{id}")).unwrap();
    let definition = s.connections.find(&id).unwrap().clone();
    let mut fillers = Vec::new();
    while let Some(reservation) = reserve_login_query_name(&format!("zq_reserved_{}", fillers.len())) {
        fillers.push(reservation);
    }
    drop(fillers.pop().unwrap());
    store.refuse_delete.store(true, Ordering::SeqCst);
    let edit = json!({"id":id,"auth":"query","keyName":"zq_transaction_1979"});
    assert_eq!(s.run("connection.edit", &edit).unwrap_err(), web::NOT_FORGOTTEN);
    assert_eq!(s.connections.find(&id).unwrap(), &definition);
    assert_eq!(store.inner.load(&format!("connection/{id}")).unwrap(), saved);
    let released = reserve_login_query_name("zq_probe_1979").unwrap();
    drop(released);
    store.refuse_delete.store(false, Ordering::SeqCst);
    store.compete_on_delete.store(true, Ordering::SeqCst);
    s.run("connection.edit", &edit).unwrap();
    assert_eq!(store.inner.load(&format!("connection/{id}")).unwrap(), None);
    assert!(!s.connections.find(&id).unwrap().remembered);
    assert!(note_login_query_name("zq_transaction_1979"));
    s.run("connection.login", &json!({"id":id,"apiKey":"new-transaction-secret"})).unwrap();
    let applied = logins::apply(&bound(&mut s, &id, &definition.address)).unwrap().unwrap();
    assert!(applied.url.ends_with("zq_transaction_1979=new-transaction-secret"));
}

#[test]
fn stale_unremembered_sessions_clean_other_sessions_credentials_or_warn_without_blocking() {
    for remove in [false, true] {
        for refused in [false, true] {
            let store = Arc::new(CountingStore::default());
            let mut a = session(Some(store.clone()));
            let id = new(&mut a, json!({"kind":"wms","address":"https://stale-marker1979.example/wms","auth":"header","keyName":"X-Key"}));
            let stale_prefs = a.connections.to_json();
            a.run("connection.login", &json!({"id":id,"apiKey":"stale-marker-secret","remember":true})).unwrap();
            let mut b = session(Some(store.clone()));
            b.load_connections(&stale_prefs);
            store.refuse_delete.store(refused, Ordering::SeqCst);
            let mark = b.notice_mark();
            let command = if remove { "connection.remove" } else { "connection.edit" };
            b.run(command, &json!({"id":id,"keyName":"X-New-Key"})).unwrap();
            assert_eq!(b.connections.find(&id).is_none(), remove);
            if !remove {
                assert_eq!(b.connections.find(&id).unwrap().key_name, "X-New-Key");
            }
            assert_eq!(store.inner.load(&format!("connection/{id}")).unwrap().is_some(), refused);
            let notices = b.notices_since(mark);
            assert_eq!(notices.iter().filter(|n| n.text.contains("a saved login may remain")).count(), usize::from(refused));
            assert!(!format!("{notices:?}").contains("stale-marker-secret"));
        }
    }
}

#[test]
fn unmarked_cleanup_preserves_colliding_accounts_with_another_identity() {
    for remove in [false, true] {
        for (auth, user, key_name, changed) in [
            ("header", "", "X-Key", json!({"address":"https://other-account.example/wms"})),
            ("header", "", "X-Key", json!({"keyName":"X-Other"})),
            ("header", "", "X-Key", json!({"auth":"query","keyName":"api_key"})),
            ("basic", "reader", "", json!({"user":"another-reader"})),
        ] {
            let store = Arc::new(CountingStore::default());
            let mut a = session(Some(store.clone()));
            let definition = json!({
                "kind":"wms","address":"https://saved-account.example/wms","auth":auth,"user":user,"keyName":key_name
            });
            let id = new(&mut a, definition.clone());
            let mut login = json!({"id":id,"remember":true});
            login[if auth == "basic" { "password" } else { "apiKey" }] = json!("collision-secret");
            a.run("connection.login", &login).unwrap();
            let account = format!("connection/{id}");
            let saved = store.inner.load(&account).unwrap();
            let mut other = definition;
            for (key, value) in changed.as_object().unwrap() {
                other[key] = value.clone();
            }
            let mut b = session(Some(store.clone()));
            assert_eq!(new(&mut b, other), id);
            let mark = b.notice_mark();
            let command = if remove { "connection.remove" } else { "connection.edit" };
            let params = if remove { json!({"id":id}) } else { json!({"id":id,"address":"https://edited-account.example/wms"}) };
            b.run(command, &params).unwrap();
            assert_eq!(store.inner.load(&account).unwrap(), saved);
            assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
            assert!(b.notices_since(mark).is_empty(), "{auth}, {changed}, remove={remove}: {:?}", b.notices_since(mark));
            let mut recovered = session(Some(store));
            recovered.load_connections(&a.connections.to_json());
            let request = bound(&mut recovered, &id, "https://saved-account.example/wms");
            let expected = if auth == "basic" {
                ("Authorization".into(), logins::basic(user, "collision-secret"))
            } else {
                (key_name.into(), "collision-secret".into())
            };
            assert_eq!(header(&request), Some(expected));
        }
    }
}

#[test]
fn remembering_preserves_unrecognized_envelopes_and_keeps_the_new_session_login() {
    for change in [json!({"version":2}), json!({"extra":"future-data"}), json!({"version":0})] {
        let store = Arc::new(CountingStore::default());
        let mut s = session(Some(store.clone()));
        let id = new(
            &mut s,
            json!({
                "kind":"wms","address":"https://future-envelope.example/wms","auth":"header","keyName":"X-Key"
            }),
        );
        s.run("connection.login", &json!({"id":id,"apiKey":"previous-secret","remember":true})).unwrap();
        let account = format!("connection/{id}");
        let mut previous: Value = serde_json::from_str(store.inner.load(&account).unwrap().unwrap().expose()).unwrap();
        for (field, value) in change.as_object().unwrap() {
            previous[field] = value.clone();
        }
        let bytes = Secret::new(previous.to_string());
        store.inner.save(&account, &bytes).unwrap();
        let result = s.run("connection.login", &json!({"id":id,"apiKey":"replacement-secret","remember":true})).unwrap();
        assert_eq!(result["warning"], web::NOT_REMEMBERED);
        assert_eq!(store.inner.load(&account).unwrap(), Some(bytes.clone()));
        assert_eq!(header(&bound(&mut s, &id, "https://future-envelope.example/wms")).unwrap().1, "replacement-secret");
        let mut unmarked = session(Some(store.clone()));
        unmarked.load_connections(&s.connections.to_json());
        unmarked.connections.set_remembered(&id, false);
        unmarked.run("connection.remove", &json!({"id":id})).unwrap();
        assert_eq!(store.inner.load(&account).unwrap(), Some(bytes));
        let mut later = session(Some(store));
        later.load_connections(&s.connections.to_json());
        assert_eq!(web::request(&mut later, Some(&id), "https://future-envelope.example/wms").unwrap_err(), web::SAVED_UNUSABLE);
    }
}

#[test]
fn forget_and_remember_leave_another_connections_login_and_clear_a_damaged_one() {
    let store = Arc::new(CountingStore::default());
    let mut a = session(Some(store.clone()));
    let id = new(&mut a, json!({"kind":"wms","address":"https://forget-owner.example/wms","auth":"header","keyName":"X-Key"}));
    a.run("connection.login", &json!({"id":id,"apiKey":"owner-secret","remember":true})).unwrap();
    let account = format!("connection/{id}");
    let saved = store.inner.load(&account).unwrap();
    // Another session numbers a different connection the same and forgets "its" login.
    let mut b = session(Some(store.clone()));
    assert_eq!(new(&mut b, json!({"kind":"wms","address":"https://forget-other.example/wms","auth":"header","keyName":"X-Key"})), id);
    b.connections.set_remembered(&id, true);
    let mark = b.notice_mark();
    assert_eq!(b.run("connection.forgetLogin", &json!({"id":id})).unwrap()["remembered"], false);
    assert_eq!(store.inner.load(&account).unwrap(), saved);
    assert_eq!(b.notices_since(mark).iter().filter(|n| n.text == web::OTHERS_LEFT).count(), 1);
    // A damaged value under the id is no one's: Forget deletes it, and Remember replaces it.
    store.inner.save(&account, &Secret::new("damaged-owner-envelope")).unwrap();
    let mark = a.notice_mark();
    a.run("connection.forgetLogin", &json!({"id":id})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), None);
    assert!(a.notices_since(mark).is_empty());
    store.inner.save(&account, &Secret::new("damaged-owner-envelope")).unwrap();
    let out = a.run("connection.login", &json!({"id":id,"apiKey":"replacement-owner","remember":true})).unwrap();
    assert!(out["warning"].is_null());
    let mut later = session(Some(store));
    later.load_connections(&a.connections.to_json());
    assert_eq!(header(&bound(&mut later, &id, "https://forget-owner.example/wms")).unwrap().1, "replacement-owner");
}

#[test]
fn a_database_password_under_a_web_connections_id_survives_forget_remember_edit_and_remove() {
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":"https://db-envelope1979.example/wms","auth":"header","keyName":"X-Key"}));
    let account = format!("connection/{id}");
    // A Postgres password saved by another session under the same connection id.
    let database = Secret::new(json!({"kind":"postgres","address":"postgres:host=h dbname=g","password":"database-1979"}).to_string());
    store.inner.save(&account, &database).unwrap();
    let saved = store.inner.load(&account).unwrap();
    let mark = s.notice_mark();
    s.run("connection.forgetLogin", &json!({"id":id})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), saved);
    assert_eq!(s.notices_since(mark).iter().filter(|n| n.text == web::OTHERS_LEFT).count(), 1);
    let result = s.run("connection.login", &json!({"id":id,"apiKey":"web-1979","remember":true})).unwrap();
    assert_eq!(result["warning"], web::NOT_REMEMBERED);
    assert_eq!(store.inner.load(&account).unwrap(), saved);
    s.run("connection.edit", &json!({"id":id,"address":"https://moved-db-envelope1979.example/wms"})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), saved);
    let mut later = session(Some(store.clone()));
    later.load_connections(&s.connections.to_json());
    later.run("connection.remove", &json!({"id":id})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), saved);
}

#[test]
fn s3_keys_under_a_web_connections_id_survive_forget_remember_edit_and_remove() {
    let s3 = Secret::new(r#"{"endpoint":null,"accessKeyId":"AKIDEXAMPLE","secretAccessKey":"s3-secret-1979"}"#);
    for operation in ["forget", "remember", "edit", "remove"] {
        let store = Arc::new(CountingStore::default());
        let mut s = session(Some(store.clone()));
        let id = new(&mut s, json!({"kind":"wms","address":"https://s3-envelope1979.example/wms","auth":"header","keyName":"X-Key"}));
        s.connections.set_remembered(&id, true);
        let account = format!("connection/{id}");
        store.inner.save(&account, &s3).unwrap();
        let out = match operation {
            "forget" => s.run("connection.forgetLogin", &json!({"id":id})),
            "remember" => s.run("connection.login", &json!({"id":id,"apiKey":"web-1979","remember":true})),
            "edit" => s.run("connection.edit", &json!({"id":id,"keyName":"X-Other"})),
            _ => s.run("connection.remove", &json!({"id":id})),
        }
        .unwrap();
        if operation == "remember" {
            assert_eq!(out["warning"], web::NOT_REMEMBERED);
        }
        assert_eq!(store.inner.load(&account).unwrap(), Some(s3.clone()), "{operation}");
    }
}

#[test]
fn a_web_envelope_missing_an_identity_field_is_cleared_by_forget_and_remember() {
    let store = Arc::new(CountingStore::default());
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":"https://partial-envelope.example/wms","auth":"header","keyName":"X-Key"}));
    let account = format!("connection/{id}");
    let partial = Secret::new(r#"{"version":1,"origin":"https://partial-envelope.example","auth":"header"}"#);
    store.inner.save(&account, &partial).unwrap();
    s.connections.set_remembered(&id, true);
    s.run("connection.forgetLogin", &json!({"id":id})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), None);
    store.inner.save(&account, &partial).unwrap();
    let out = s.run("connection.login", &json!({"id":id,"apiKey":"replaced-partial","remember":true})).unwrap();
    assert!(out["warning"].is_null());
    let mut later = session(Some(store));
    later.load_connections(&s.connections.to_json());
    assert_eq!(header(&bound(&mut later, &id, "https://partial-envelope.example/wms")).unwrap().1, "replaced-partial");
}

#[test]
fn forgetting_a_saved_login_clears_the_failed_read_it_leaves_behind() {
    let store = Arc::new(CountingStore::default());
    let origin = "https://forget-failed1979.example";
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"failed-read-1979","remember":true})).unwrap();
    let account = format!("connection/{id}");
    store.inner.save(&account, &Secret::new("damaged-failed-read-1979")).unwrap();
    let mut later = session(Some(store.clone()));
    later.load_connections(&s.connections.to_json());
    let c = later.connections.find(&id).unwrap().clone();
    assert_eq!(web::known_secret(&mut later, &c).err().as_deref(), Some(web::SAVED_UNUSABLE));
    later.run("connection.forgetLogin", &json!({"id":id})).unwrap();
    assert_eq!(store.inner.load(&account).unwrap(), None);
    let c = later.connections.find(&id).unwrap().clone();
    assert_eq!(web::known_secret(&mut later, &c).err().as_deref(), Some(web::REQUIRED));
}

#[test]
fn a_stale_remembered_marker_never_deletes_another_identitys_login() {
    for remove in [false, true] {
        let store = Arc::new(CountingStore::default());
        let mut a = session(Some(store.clone()));
        let id = new(&mut a, json!({"kind":"wms","address":"https://stale-owner.example/wms","auth":"header","keyName":"X-Key"}));
        a.run("connection.login", &json!({"id":id,"apiKey":"first-secret","remember":true})).unwrap();
        // Another session moves the connection to another origin and remembers a login there.
        let mut b = session(Some(store.clone()));
        b.load_connections(&a.connections.to_json());
        b.run("connection.edit", &json!({"id":id,"address":"https://moved-owner.example/wms"})).unwrap();
        b.run("connection.login", &json!({"id":id,"apiKey":"moved-secret","remember":true})).unwrap();
        let account = format!("connection/{id}");
        let saved = store.inner.load(&account).unwrap();
        // This session still holds the old definition, marked remembered.
        assert!(a.connections.find(&id).unwrap().remembered);
        let (command, params) =
            if remove { ("connection.remove", json!({"id":id})) } else { ("connection.edit", json!({"id":id,"keyName":"X-Other"})) };
        a.run(command, &params).unwrap();
        assert_eq!(store.inner.load(&account).unwrap(), saved, "remove={remove}");
        let mut later = session(Some(store));
        later.load_connections(&b.connections.to_json());
        assert_eq!(header(&bound(&mut later, &id, "https://moved-owner.example/wms")).unwrap().1, "moved-secret");
    }
}

#[test]
fn a_web_saved_login_marker_is_written_where_an_older_reader_keeps_it() {
    let mut s = session(None);
    let web = new(&mut s, json!({"kind":"wms","address":"https://marker-field.example/wms","auth":"header","keyName":"X-Key"}));
    let database = new(&mut s, json!({"kind":"postgres","address":"postgres:host=127.0.0.1 port=1 user=u dbname=gis"}));
    assert!(s.connections.set_remembered(&web, true));
    assert!(s.connections.set_remembered(&database, true));
    let prefs: Value = serde_json::from_str(&s.connections.to_json()).unwrap();
    let row = |id: &str| prefs["connections"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
    // An older RustGIS clears `remembered` on a web connection, but keeps a field it doesn't know.
    assert_eq!(row(&web)["savedLogin"], true);
    assert!(row(&web).get("remembered").is_none());
    assert_eq!(row(&database)["remembered"], true);
    assert!(row(&database).get("savedLogin").is_none());
    let mut later = session(None);
    later.load_connections(&prefs.to_string());
    assert!(later.connections.find(&web).unwrap().remembered);
    assert!(later.connections.find(&database).unwrap().remembered);
}

#[test]
fn simultaneous_sessions_and_connections_never_share_or_replace_logins() {
    let origin = "https://sessions757.example.org";
    let definition = json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"});
    let mut a = session(None);
    let mut b = session(None);
    let id = new(&mut a, definition.clone());
    assert_eq!(new(&mut b, definition.clone()), id);
    a.run("connection.login", &json!({"id":id,"apiKey":"session-a-757"})).unwrap();
    let a_address = bound(&mut a, &id, &format!("{origin}/wms"));
    assert!(web::request(&mut b, Some(&id), &format!("{origin}/wms")).is_err());
    assert!(web::request(&mut b, None, &a_address).is_err());
    b.run("connection.login", &json!({"id":id,"apiKey":"session-b-757"})).unwrap();
    let b_address = bound(&mut b, &id, &format!("{origin}/wms"));
    assert_ne!(a_address, b_address);
    assert_eq!(header(&a_address).unwrap().1, "session-a-757");
    assert_eq!(header(&b_address).unwrap().1, "session-b-757");
    let second =
        new(&mut a, json!({"name":"Other account", "kind":"wms","address":format!("{origin}/other"),"auth":"header","keyName":"X-Key"}));
    a.run("connection.login", &json!({"id":second,"apiKey":"second-a-757"})).unwrap();
    assert_eq!(header(&a_address).unwrap().1, "session-a-757");
    assert_eq!(header(&bound(&mut a, &second, &format!("{origin}/wms"))).unwrap().1, "second-a-757");
    drop(b);
    assert!(logins::apply(&b_address).is_err());
    assert_eq!(header(&a_address).unwrap().1, "session-a-757");
    drop(a);
    assert!(logins::apply(&a_address).is_err());
}

#[test]
fn a_project_without_its_connection_or_login_asks_then_rebinds_without_persisting_authority() {
    let origin = "https://open757.example.org";
    let mut s = session(None);
    s.run("project.openText", &json!({"text":project(origin,"c7")})).unwrap();
    let r = requests(&mut s);
    assert_eq!((r[0]["reason"].as_str(), r[0]["origin"].as_str(), r[0]["layers"].as_u64()), (Some("connection"), Some(origin), Some(1)));
    let id = new(&mut s, json!({"kind":"wmts","address":format!("{origin}/wmts"),"auth":"header","keyName":"X-Key"}));
    // A connection made meanwhile is nobody's choice for layers that recorded another one.
    assert_eq!(requests(&mut s)[0]["reason"], "connection");
    assert_eq!(s.map().tiles[0].connection, "c7");
    s.run("connection.login", &json!({"id":id,"apiKey":"open-sentinel-757"})).unwrap();
    assert_eq!(requests(&mut s), json!([]));
    assert_eq!(s.map().tiles[0].connection, id);
    let runtime = s.map().tiles[0].url.clone();
    assert_eq!(header(&runtime).unwrap().1, "open-sentinel-757");
    let (saved, _) = s.project.saved_text(None).unwrap();
    assert!(saved.contains(&format!(r#""connection":"{id}""#)) || saved.contains(&format!(r#""connection": "{id}""#)));
    assert!(!saved.contains("rustgis-login") && !saved.contains("open-sentinel-757"));
    assert_eq!(crate::sources::redact(&runtime), format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"));
    s.notify("server echoed open-sentinel-757", true);
    let error =
        s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"kind":"open-sentinel-757"})).unwrap_err();
    let visible = format!(
        "{}{}{}{:?}{:?}",
        s.run("connection.list", &Value::Null).unwrap(),
        s.connections.to_json(),
        saved,
        s.notices,
        s.database_logins
    );
    assert!(!error.contains("open-sentinel-757") && !visible.contains("open-sentinel-757"));
    assert!(crate::sources::holds_secret(&json!({"apiKey":"open-sentinel-757"})));
    // A persisted runtime token from another session is not adopted on load.
    let forged = serde_json::to_string(&s.project).unwrap();
    let mut other = session(None);
    other.run("project.openText", &json!({"text":forged})).unwrap();
    assert_eq!(requests(&mut other)[0]["reason"], "connection");
    assert!(header(&other.map().tiles[0].url).is_none());
    s.run("connection.edit", &json!({"id":id,"address":"https://moved-open757.example.org/wmts"})).unwrap();
    s.run("project.openText", &json!({"text":saved})).unwrap();
    assert_eq!(requests(&mut s)[0]["reason"], "connection");
}

#[test]
fn recorded_connections_at_one_origin_remain_independent_and_unassigned_layers_ask() {
    let origin = "https://two757.example.org";
    let mut s = session(None);
    let a = new(&mut s, json!({"kind":"wms","name":"A","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    let b = new(&mut s, json!({"kind":"wms","name":"B","address":format!("{origin}/wms?map=b")}));
    s.run("project.openText", &json!({"text":project(origin, "")})).unwrap();
    assert_eq!(requests(&mut s)[0]["reason"], "choose");
    s.run("connection.login", &json!({"id":a,"apiKey":"a-sentinel-757"})).unwrap();
    assert_eq!(s.map().tiles[0].connection, a);
    assert_eq!(header(&s.map().tiles[0].url).unwrap().1, "a-sentinel-757");
    s.run("connection.login", &json!({"id":b})).unwrap();
    // Choosing anonymous B for unassigned requests cannot revoke A's recorded layer.
    assert_eq!(header(&s.map().tiles[0].url).unwrap().1, "a-sentinel-757");
    assert!(header(&web::request(&mut s, None, &format!("{origin}/wms")).unwrap()).is_none());
}

#[test]
fn public_connections_at_one_origin_never_ask_which_to_use() {
    // Two public connections of one server: any choice reads the same anonymously.
    let origin = "https://public757.example.org";
    let mut s = session(None);
    new(&mut s, json!({"kind":"wms","name":"Public WMS","address":format!("{origin}/wms")}));
    new(&mut s, json!({"kind":"wfs","name":"Public WFS","address":format!("{origin}/wfs")}));
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"Bare"})).unwrap();
    assert_eq!(requests(&mut s), json!([]));
    s.run("project.openText", &json!({"text":project(origin, "")})).unwrap();
    assert_eq!(requests(&mut s), json!([]));
}

#[test]
fn test_refuses_changed_endpoint_and_missing_login_instead_of_testing_anonymously() {
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wms","address":"https://test757.example.org/wms","auth":"header","keyName":"X-Key"}));
    for changed in [
        json!({"address":"https://else757.example.org/wms"}),
        json!({"auth":"basic"}),
        json!({"user":"other"}),
        json!({"keyName":"X-Other"}),
        json!({"kind":"stac"}),
    ] {
        let mut p = changed;
        p["id"] = json!(id);
        p["probe"] = json!(true);
        p["apiKey"] = json!("not-forwarded-757");
        assert_eq!(s.run("connection.test", &p).unwrap_err(), web::SAVE_BEFORE_TEST);
    }
    assert_eq!(s.run("connection.test", &json!({"id":id,"probe":true})).unwrap_err(), web::REQUIRED);
    assert!(!s.database_logins.has_session_password(&id));
}

#[test]
fn junk_login_parameters_never_panic() {
    let mut s = session(Some(Arc::new(MemorySecrets::new())));
    let id = new(&mut s, json!({"kind":"csw","address":"https://junk757.example.org/csw","auth":"header","keyName":"X-Key"}));
    let junk = [
        json!(null),
        json!([]),
        json!({"id":id,"apiKey":7,"password":{"a":1},"remember":[],"allowInsecure":null}),
        json!({"id":id,"auth":"\u{0}","user":"\u{feff}","keyName":"x".repeat(10_000)}),
        json!({"id":id,"apiKey":"\u{7f}","allowInsecure":true}),
        json!({"id":[id],"connection":{"id":id}}),
    ];
    for cmd in [
        "connection.new",
        "connection.edit",
        "connection.login",
        "connection.logout",
        "connection.forgetLogin",
        "connection.test",
        "map.addTiles",
        "service.add",
    ] {
        for p in &junk {
            let _ = s.run(cmd, p);
        }
    }
}

#[test]
fn a_group_child_read_in_place_is_counted_once_and_refresh_respects_logout() {
    let origin = "https://group757.example.org";
    let mut s = session(Some(Arc::new(MemorySecrets::new())));
    let id = new(&mut s, json!({"kind":"ogcapi","address":format!("{origin}/ogc"),"auth":"basic","user":"ana"}));
    let mut layer = rustgis_map::Layer::new(41, rustgis_data::Dataset::new("Roads", rustgis_data::GeometryType::None, Vec::new()));
    layer.connection = id.clone();
    layer.source = Some(serde_json::from_value(json!({"path":format!("{origin}/ogc/collections/roads/items")})).unwrap());
    s.project.map_mut().layers.push(layer);
    s.project.map_mut().service_groups.push(rustgis_map::ServiceGroup {
        id: 5,
        name: "Imported".into(),
        url: format!("{origin}/ogc/collections"),
        kind: "ogcapi".into(),
        anonymous: false,
        expanded: true,
        children: vec![rustgis_map::ServiceChild {
            layer: 41,
            collection: "roads".into(),
            source_url: format!("{origin}/ogc/collections/roads"),
        }],
    });
    web::resolve(&mut s, None);
    assert_eq!(requests(&mut s)[0]["layers"], 1);
    s.run("connection.login", &json!({"id":id,"password":"group-sentinel-757"})).unwrap();
    assert_eq!(
        header(s.map().layers[0].source.as_ref().unwrap().path.as_deref().unwrap()).unwrap().1,
        logins::basic("ana", "group-sentinel-757")
    );
    assert!(header(&s.map().service_groups[0].url).is_some());
    s.run("connection.logout", &json!({"id":id})).unwrap();
    assert_eq!(s.run("service.refresh", &json!({"group":5,"text":"not a document"})).unwrap_err(), web::LOGGED_OUT);
    assert_eq!(s.run("connection.list", &Value::Null).unwrap()["connections"][0]["loggedIn"], false);
}

#[test]
fn anonymous_reads_never_borrow_a_groups_recorded_login() {
    let origin = "https://anon-group757.example.org";
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"ogcapi","address":format!("{origin}/ogc"),"auth":"header","keyName":"X-Key"}));
    let mut layer = rustgis_map::Layer::new(42, rustgis_data::Dataset::new("Roads", rustgis_data::GeometryType::None, Vec::new()));
    layer.connection = id.clone();
    s.project.map_mut().layers.push(layer);
    s.project.map_mut().service_groups.push(rustgis_map::ServiceGroup {
        id: 6,
        name: "Imported".into(),
        url: format!("{origin}/ogc/collections"),
        kind: "ogcapi".into(),
        anonymous: false,
        expanded: true,
        children: vec![rustgis_map::ServiceChild {
            layer: 42,
            collection: "roads".into(),
            source_url: format!("{origin}/ogc/collections/roads"),
        }],
    });
    s.run("connection.login", &json!({"id":id,"apiKey":"anon-group-sentinel-757"})).unwrap();
    let plan = |s: &mut Session| s.run("service.groupPlan", &json!({"group":6,"sourceOnly":true})).unwrap();
    // The session's own plan reads the group with the connection its layers record...
    assert_eq!(header(plan(&mut s)["url"].as_str().unwrap()).unwrap().1, "anon-group-sentinel-757");
    // ...but an anonymous read (a plugin's command) never infers it.
    let was = s.web_logins.set_anonymous(true);
    let anonymous = plan(&mut s);
    s.web_logins.set_anonymous(was);
    assert!(header(anonymous["url"].as_str().unwrap()).is_none(), "{anonymous}");
    assert!(!anonymous.to_string().contains("anon-group-sentinel-757"));
}

#[test]
fn a_panic_during_an_anonymous_read_puts_the_logins_back() {
    let mut s = session(None);
    let out: std::thread::Result<()> = web::anonymously(&mut s, true, |s| {
        assert!(s.web_logins.is_anonymous());
        panic!("a command failed unexpectedly")
    });
    assert!(out.is_err());
    assert!(!s.web_logins.is_anonymous(), "later requests must carry their logins again");
}

#[test]
fn a_blank_connection_is_no_connection() {
    let mut s = session(None);
    for (name, connection) in [("Without", None), ("Blank", Some("")), ("Spaces", Some("  "))] {
        let mut p = json!({"url":"https://blank757.example.org/tiles/{z}/{x}/{y}.png","name":name});
        if let Some(c) = connection {
            p["connection"] = json!(c);
        }
        s.run("map.addTiles", &p).unwrap();
    }
    assert!(s.map().tiles.iter().all(|t| t.connection.is_empty()));
}

#[test]
fn header_keys_are_plain_ascii_basic_passwords_need_not_be() {
    let mut s = session(None);
    let key = new(&mut s, json!({"kind":"wms","address":"https://ascii757.example.org/wms","auth":"header","keyName":"X-Key"}));
    for typed in ["café-757", "€757"] {
        let e = s.run("connection.login", &json!({"id":key,"apiKey":typed})).unwrap_err();
        assert_eq!(e, web::HEADER_ASCII);
        assert!(!e.contains(typed));
    }
    s.run("connection.login", &json!({"id":key,"apiKey":"plain-ascii-757"})).unwrap();
    // Basic logins are base64 of UTF-8, built the same way on every platform.
    let basic = new(&mut s, json!({"kind":"wms","address":"https://ascii-basic757.example.org/wms","auth":"basic","user":"ana"}));
    s.run("connection.login", &json!({"id":basic,"password":"café-757"})).unwrap();
}

#[test]
fn a_definition_reloaded_with_another_identity_cannot_reuse_a_session_secret() {
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wms","address":"https://identity757.example.org/wms","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"identity-sentinel-757"})).unwrap();
    let mut prefs: Value = serde_json::from_str(&s.connections.to_json()).unwrap();
    prefs["connections"][0]["address"] = json!("https://changed757.example.org/wms");
    s.connections = crate::connections::Connections::from_json(&prefs.to_string());
    assert_eq!(web::request(&mut s, Some(&id), "https://changed757.example.org/wms").unwrap_err(), web::UNUSABLE);
    assert!(header("https://changed757.example.org/wms").is_none());
    assert_eq!(s.run("connection.list", &Value::Null).unwrap()["connections"][0]["loggedIn"], false);
}

#[test]
fn logins_drop_the_tile_cache_only_when_one_ends_or_is_replaced() {
    let mut s = session(None);
    let public = new(&mut s, json!({"kind":"wms","name":"Public","address":"https://cache-public757.example.org/wms"}));
    let private = new(
        &mut s,
        json!({"kind":"wms","name":"Private","address":"https://cache-private757.example.org/wms","auth":"header","keyName":"X-Key"}),
    );
    let start = web::cache_clears();
    // Opening a connection without a login method, and a first login, change no cached response.
    s.run("connection.login", &json!({"id":public})).unwrap();
    s.run("connection.login", &json!({"id":public})).unwrap();
    s.run("connection.login", &json!({"id":private,"apiKey":"cache-sentinel-757"})).unwrap();
    assert_eq!(web::cache_clears(), start);
    // Logging in again without typing (choosing it again) keeps the binding and the cache.
    let first = bound(&mut s, &private, "https://cache-private757.example.org/wms");
    s.run("connection.login", &json!({"id":private})).unwrap();
    assert_eq!(bound(&mut s, &private, "https://cache-private757.example.org/wms"), first);
    assert_eq!(web::cache_clears(), start);
    // Replacing or ending a login leaves authenticated responses behind: those go.
    s.run("connection.login", &json!({"id":private,"apiKey":"fake004"})).unwrap();
    assert_eq!(web::cache_clears(), start + 1);
    s.run("connection.logout", &json!({"id":private})).unwrap();
    assert_eq!(web::cache_clears(), start + 2);
    s.run("connection.logout", &json!({"id":private})).unwrap();
    assert_eq!(web::cache_clears(), start + 2, "there was no login to end");
}

#[test]
fn a_recorded_connection_that_is_gone_is_asked_about_not_replaced_by_the_only_other_one() {
    let origin = "https://gone757.example.org";
    let mut s = session(None);
    let other = new(&mut s, json!({"kind":"wms","name":"Other","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":other,"apiKey":"other-sentinel-757"})).unwrap();
    s.run("project.openText", &json!({"text":project(origin,"deleted")})).unwrap();
    assert_eq!(requests(&mut s)[0]["reason"], "connection");
    assert_eq!(s.map().tiles[0].connection, "deleted");
    assert!(header(&s.map().tiles[0].url).is_none(), "the other connection's login is not sent");
    // Choosing it is the user's to do.
    s.run("connection.login", &json!({"id":other})).unwrap();
    assert_eq!(requests(&mut s), json!([]));
    assert_eq!(s.map().tiles[0].connection, other);
    assert_eq!(header(&s.map().tiles[0].url).unwrap().1, "other-sentinel-757");

    // Layers that record no connection still use the origin's only one.
    let mut fresh = session(None);
    let sole = new(&mut fresh, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    fresh.run("connection.login", &json!({"id":sole,"apiKey":"sole-sentinel-757"})).unwrap();
    fresh.run("project.openText", &json!({"text":project(origin,"")})).unwrap();
    assert_eq!(requests(&mut fresh), json!([]));
    assert_eq!(fresh.map().tiles[0].connection, sole);

    // Nor does an unrecorded layer's use of it become the choice for a layer that recorded a gone one.
    let mut mixed = session(None);
    let sole = new(&mut mixed, json!({"kind":"wms","address":format!("{origin}/wms")}));
    let mut p = Project::default();
    for (name, recorded) in [("Unrecorded", ""), ("Gone", "deleted")] {
        let mut t = rustgis_map::TileLayer::new(name, rustgis_map::TileKind::Raster, format!("{origin}/{name}/{{z}}/{{x}}/{{y}}.png"));
        t.connection = recorded.into();
        p.map_mut().tiles.push(t);
    }
    mixed.run("project.openText", &json!({"text":serde_json::to_string(&p).unwrap()})).unwrap();
    let recorded = |s: &Session, name: &str| s.map().tiles.iter().find(|t| t.name == name).unwrap().connection.clone();
    assert_eq!(recorded(&mixed, "Unrecorded"), sole);
    assert_eq!(recorded(&mixed, "Gone"), "deleted");
    assert_eq!(requests(&mut mixed)[0]["reason"], "connection");
}

#[test]
fn an_unsaved_anonymous_test_never_carries_a_saved_login() {
    let mut s = session(None);
    let address = "https://probe757.example.org/wms";
    let id = new(&mut s, json!({"kind":"wms","address":address,"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"fake005"})).unwrap();
    let unsaved = s.run("connection.test", &json!({"kind":"wms","address":address,"probe":true})).unwrap();
    assert!(header(unsaved["fetch"].as_str().unwrap()).is_none(), "{unsaved}");
    let saved = s.run("connection.test", &json!({"id":id,"probe":true})).unwrap();
    assert_eq!(header(saved["fetch"].as_str().unwrap()).unwrap().1, "fake005");
    // The anonymous read ended with the test.
    assert!(header(&web::request(&mut s, None, address).unwrap()).is_some());
}

#[test]
fn plugins_cannot_send_a_saved_web_service_login() {
    super::isolate_plugins();
    let mut s = session(None);
    let origin = "https://plugin757.example.org";
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"plugin-sentinel-757"})).unwrap();
    // With files and network granted too: neither lends the plugin the user's login.
    super::plugin("eng-login-757", &["edit", "network", "files"]);
    let tiles = format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png");
    // Naming the connection is refused, as logging in to it is.
    for command in ["map.addTiles", "layer.addUrl", "service.layers"] {
        let e = super::forward(&mut s, "eng-login-757", command, json!({"url":tiles,"connection":id})).unwrap_err();
        assert!(e.contains("web-service connection"), "{command}: {e}");
    }
    assert!(s.map().tiles.is_empty());
    // Not naming it, the only one at the origin isn't used for it either; the user's own command is.
    super::forward(
        &mut s,
        "eng-login-757",
        "map.addTiles",
        json!({"url":format!("{origin}/plugin/{{z}}/{{x}}/{{y}}.png"),"name":"Plugin"}),
    )
    .unwrap();
    assert!(header(&s.map().tiles[0].url).is_none());
    assert!(s.map().tiles[0].connection.is_empty());
    s.run("map.addTiles", &json!({"url":tiles,"name":"Mine"})).unwrap();
    let mine = s.map().tiles.iter().find(|t| t.name == "Mine").unwrap();
    assert_eq!(header(&mine.url).unwrap().1, "plugin-sentinel-757");
    // A legend it sets at the user's logged-in layer's origin is refused: the next login would
    // bind it, and the plugin would have picked where the login goes.
    s.run("map.addTiles", &json!({"url":tiles,"name":"Named","connection":id})).unwrap();
    let e = super::forward(&mut s, "eng-login-757", "map.setTiles", json!({"name":"Named","legend":format!("{origin}/legend.png")}))
        .unwrap_err();
    assert!(e.contains("plugins can't set a legend"), "{e}");
    let named = s.map().tiles.iter().find(|t| t.name == "Named").unwrap();
    assert_eq!(named.connection, id, "the layer records the connection, so a legend there would be bound");
    assert!(named.legends.is_empty());
    // One at another origin is its own, and stays plain when the user logs in again.
    let elsewhere = "https://plugin-other757.example.org/legend.png";
    super::forward(&mut s, "eng-login-757", "map.setTiles", json!({"name":"Named","legend":elsewhere})).unwrap();
    s.run("connection.login", &json!({"id":id,"apiKey":"plugin-sentinel-757"})).unwrap();
    let named = s.map().tiles.iter().find(|t| t.name == "Named").unwrap();
    assert_eq!(named.legends[0].url, elsewhere);
    let plugin_tile = s.map().tiles.iter().find(|t| t.name == "Plugin").unwrap();
    assert!(header(&plugin_tile.url).is_none(), "a later login must not adopt the plugin's tile");
    assert!(plugin_tile.connection.is_empty());
    assert_eq!(header(&named.url).unwrap().1, "plugin-sentinel-757", "the user's layer still uses its login");
    let (saved, _) = s.project.clone().saved_text(None).unwrap();
    s.run("project.openText", &json!({"text":saved})).unwrap();
    s.run("connection.login", &json!({"id":id,"apiKey":"plugin-sentinel-757"})).unwrap();
    let plugin_tile = s.map().tiles.iter().find(|t| t.name == "Plugin").unwrap();
    assert!(header(&plugin_tile.url).is_none(), "saving and reopening must preserve anonymous reads");
    assert!(plugin_tile.connection.is_empty());
    rustgis_plugins::unload("eng-login-757");
}

#[test]
fn plugin_legends_are_refused_before_a_public_connection_gains_a_login() {
    super::isolate_plugins();
    let mut s = session(None);
    let origin = "https://public-plugin757.example.org";
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"none"}));
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"Public","connection":id})).unwrap();
    super::plugin("eng-public-login-757", &["edit", "network", "files"]);
    let error = super::forward(
        &mut s,
        "eng-public-login-757",
        "map.setTiles",
        json!({"name":"Public","legend":format!("{origin}/chosen-by-plugin.png")}),
    )
    .unwrap_err();
    assert!(error.contains("plugins can't set a legend"), "{error}");
    s.run("connection.edit", &json!({"id":id,"auth":"header","keyName":"X-Key"})).unwrap();
    s.run("connection.login", &json!({"id":id,"apiKey":"later-sentinel-757"})).unwrap();
    let tile = s.map().tiles.iter().find(|t| t.name == "Public").unwrap();
    assert!(tile.legends.is_empty(), "no plugin path was stored for the new login to bind");
    assert_eq!(header(&tile.url).unwrap().1, "later-sentinel-757");
    rustgis_plugins::unload("eng-public-login-757");
}

#[test]
fn plugin_imports_never_acquire_a_later_connections_login() {
    super::isolate_plugins();
    let mut s = session(None);
    let origin = "https://import-plugin757.example.org";
    let mut imported = Project::default().map().clone();
    imported.name = "Plugin imports".into();
    let mut tile = rustgis_map::TileLayer::new("Dates", rustgis_map::TileKind::Raster, format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"));
    tile.time_url = Some(format!("{origin}/{{date:YYYY}}.png"));
    tile.time_urls = vec![(1.0, format!("{origin}/dated.png"))];
    tile.legends = vec![rustgis_data::ogc::Legend { url: format!("{origin}/legend.png"), ..Default::default() }];
    imported.tiles.push(tile);
    let mut layer = rustgis_map::Layer::new(1, rustgis_data::Dataset::new("Remote", rustgis_data::GeometryType::None, Vec::new()));
    layer.dataset.source = Some(format!("{origin}/data.geojson"));
    layer.source = Some(serde_json::from_value(json!({"path":format!("{origin}/source.fgb")})).unwrap());
    layer.raster = Some(serde_json::from_value(json!({"path":format!("{origin}/raster.tif"),"style":{"renderer":"Colormap"}})).unwrap());
    imported.layers.push(layer);
    super::plugin("eng-import-login-757", &["edit", "network", "files"]);
    super::forward(&mut s, "eng-import-login-757", "map.import", json!({"text":serde_json::to_string(&imported).unwrap()})).unwrap();
    // Identify metadata is runtime-only; it belongs to the same anonymous tile.
    s.project.map_mut().tiles[0].feature_info =
        Some(rustgis_data::ogc::FeatureInfo { url: format!("{origin}/identify"), ..Default::default() });
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    for reopened in [false, true] {
        if reopened {
            let (text, _) = s.project.clone().saved_text(None).unwrap();
            s.run("project.openText", &json!({"text":text})).unwrap();
        }
        s.run("connection.login", &json!({"id":id,"apiKey":"import-sentinel-757"})).unwrap();
        let map = s.map();
        let tile = &map.tiles[0];
        assert!(tile.connection.is_empty());
        for address in std::iter::once(tile.url.as_str())
            .chain(tile.time_url.as_deref())
            .chain(tile.time_urls.iter().map(|(_, url)| url.as_str()))
            .chain(tile.legends.iter().map(|legend| legend.url.as_str()))
            .chain(tile.feature_info.as_ref().map(|info| info.url.as_str()))
        {
            assert!(header(address).is_none(), "plugin tile acquired a login: {address}");
        }
        let layer = &map.layers[0];
        assert!(layer.connection.is_empty());
        for address in [
            layer.dataset.source.as_deref().unwrap(),
            layer.source.as_ref().unwrap().path.as_deref().unwrap(),
            layer.raster.as_ref().unwrap().path.as_deref().unwrap(),
        ] {
            assert!(header(address).is_none(), "plugin layer acquired a login: {address}");
        }
    }
    rustgis_plugins::unload("eng-import-login-757");
}

#[test]
fn plugin_service_groups_stay_anonymous_through_refresh_and_reopen() {
    super::isolate_plugins();
    for (plugin, import_map, auth) in [("eng-group-map-login-757", true, "basic"), ("eng-group-service-login-757", false, "header")] {
        let origin = format!("https://{plugin}.example.org");
        let url = format!("{origin}/collections");
        let collections = |names: &[&str]| {
            json!({"collections":names.iter().map(|name| json!({
                "id":name,"title":name,
                "links":[{"rel":"items","type":"application/geo+json","href":format!("{url}/{name}/items")}]
            })).collect::<Vec<_>>()})
            .to_string()
        };
        let responses = |plan: &Value| {
            plan["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|layer| {
                    json!({
                        "url":layer["offer"]["url"],
                        "text":json!({"type":"FeatureCollection","features":[{
                            "type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"collection":layer["name"]}
                        }]}).to_string()
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut s = session(None);
        super::plugin(plugin, &["edit", "network", "files"]);
        if import_map {
            let mut map = Project::default().map().clone();
            map.name = "Plugin service groups".into();
            let mut layer = rustgis_map::Layer::new(1, rustgis_data::Dataset::new("roads", rustgis_data::GeometryType::None, Vec::new()));
            layer.dataset.source = Some(format!("{url}/roads/items"));
            map.layers.push(layer);
            map.service_groups = vec![
                rustgis_map::ServiceGroup {
                    id: 100,
                    name: "Plugin".into(),
                    url: url.clone(),
                    kind: "ogcapi".into(),
                    anonymous: false,
                    expanded: true,
                    children: vec![rustgis_map::ServiceChild { layer: 1, collection: "roads".into(), source_url: url.clone() }],
                },
                rustgis_map::ServiceGroup {
                    id: 101,
                    name: "Initially empty".into(),
                    url: url.clone(),
                    kind: "ogcapi".into(),
                    anonymous: false,
                    expanded: true,
                    children: vec![],
                },
            ];
            super::forward(&mut s, plugin, "map.import", json!({"text":serde_json::to_string(&map).unwrap()})).unwrap();
        } else {
            let plan = super::forward(
                &mut s,
                plugin,
                "service.groupPlan",
                json!({
                    "url":url,"type":"ogcapi","text":collections(&["roads"])
                }),
            )
            .unwrap();
            super::forward(
                &mut s,
                plugin,
                "service.import",
                json!({
                    "url":url,"type":"ogcapi","text":collections(&["roads"]),"responses":responses(&plan)
                }),
            )
            .unwrap();
        }
        let group = s.map().service_groups[0].id;
        let id = new(&mut s, json!({"kind":"ogcapi","address":url,"auth":auth,"user":"ana","keyName":"X-Key"}));
        let login = if auth == "basic" {
            json!({"id":id,"password":"group-import-sentinel-757"})
        } else {
            json!({"id":id,"apiKey":"group-import-sentinel-757"})
        };
        s.run("connection.login", &login).unwrap();
        for g in &s.map().service_groups {
            assert!(g.anonymous, "even a plugin-imported empty group needs provenance");
        }
        for group in s.map().service_groups.iter().map(|g| g.id).collect::<Vec<_>>() {
            let source = s.run("service.groupPlan", &json!({"group":group,"sourceOnly":true})).unwrap();
            let prepared = web::request_params(&mut s, &source).unwrap();
            assert!(header(prepared["url"].as_str().unwrap()).is_none());
        }
        let assert_anonymous = |s: &mut Session| {
            let source = s.run("service.groupPlan", &json!({"group":group,"sourceOnly":true,"connection":id})).unwrap();
            assert_eq!(source["anonymous"], true);
            assert!(source.get("connection").is_none());
            // Exercise the consumer, not just the flag: it must not infer the saved login.
            let prepared = web::request_params(s, &source).unwrap();
            assert_eq!(prepared["url"], url);
            assert!(header(prepared["url"].as_str().unwrap()).is_none());
            let g = s.map().service_groups.iter().find(|g| g.id == group).unwrap();
            assert!(g.anonymous);
            assert!(header(&g.url).is_none());
            for child in &g.children {
                let layer = s.map().layer(child.layer).unwrap();
                assert!(layer.anonymous && layer.connection.is_empty());
                assert!(header(layer.dataset.source.as_deref().unwrap()).is_none());
                assert!(header(&child.source_url).is_none());
            }
            assert_eq!(requests(s), json!([]));
        };
        assert_anonymous(&mut s);
        for reopen in [false, true] {
            if reopen {
                let (text, _) = s.project.clone().saved_text(None).unwrap();
                s.run("project.openText", &json!({"text":text})).unwrap();
                s.run("connection.login", &login).unwrap();
            }
            let plan = s.run("service.groupPlan", &json!({"group":group,"text":collections(&["roads","water"])})).unwrap();
            let prepared = web::request_params(&mut s, &plan["request"]).unwrap();
            assert!(header(prepared["url"].as_str().unwrap()).is_none());
            s.run(
                "service.refresh",
                &json!({
                    "group":group,"text":collections(&["roads","water"]),"responses":responses(&plan)
                }),
            )
            .unwrap();
            assert_anonymous(&mut s);
            let g = s.map().service_groups.iter().find(|g| g.id == group).unwrap();
            assert_eq!(g.children.len(), 2);
            assert!(g.children.iter().any(|c| c.collection == "water"));
            for child in &g.children {
                assert_eq!(s.map().layer(child.layer).unwrap().dataset.features.len(), 1);
            }
            let (text, _) = s.project.clone().saved_text(None).unwrap();
            s.run("project.openText", &json!({"text":text})).unwrap();
            s.run("connection.login", &login).unwrap();
            assert_anonymous(&mut s);
            // Removing every child must not erase the group's independent provenance.
            s.run("service.refresh", &json!({"group":group,"text":collections(&[]),"responses":[]})).unwrap();
            assert!(s.map().service_groups.iter().find(|g| g.id == group).unwrap().children.is_empty());
            assert_anonymous(&mut s);
        }
        rustgis_plugins::unload(plugin);
    }
}

#[test]
fn refreshing_an_anonymous_child_does_not_adopt_or_anonymize_its_user_group() {
    let mut s = session(None);
    let url = "https://mixed-group757.example.org/collections";
    let id = new(&mut s, json!({"kind":"ogcapi","address":url,"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"mixed-group-sentinel-757"})).unwrap();
    let collections = ["roads", "water"].map(|name| {
        json!({
            "id":name,"title":name,"links":[{"rel":"items","type":"application/geo+json","href":format!("{url}/{name}/items")}]
        })
    });
    let document = json!({"collections":collections}).to_string();
    let features = r#"{"type":"FeatureCollection","features":[]}"#;
    let responses = |plan: &Value| {
        plan["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| {
                json!({
                    "url":layer["offer"]["url"],"text":features
                })
            })
            .collect::<Vec<_>>()
    };
    let plan = s.run("service.groupPlan", &json!({"url":url,"type":"ogcapi","connection":id,"text":document})).unwrap();
    let added = s
        .run(
            "service.import",
            &json!({
                "url":url,"type":"ogcapi","connection":id,"text":document,
                "responses":responses(&plan)
            }),
        )
        .unwrap();
    let group = added["group"].clone();
    let child = s.map().service_groups[0].children[0].layer;
    let other = s.map().service_groups[0].children[1].layer;
    s.map_mut().layer_mut(child).unwrap().anonymous = true;
    s.map_mut().layer_mut(child).unwrap().connection.clear();
    let source = s.run("service.groupPlan", &json!({"layer":child,"sourceOnly":true,"connection":id})).unwrap();
    let prepared = web::request_params(&mut s, &source).unwrap();
    assert!(header(prepared["url"].as_str().unwrap()).is_none());
    let plan = s.run("service.groupPlan", &json!({"layer":child,"connection":id,"text":document})).unwrap();
    s.run(
        "service.refresh",
        &json!({
            "layer":child,"connection":id,"text":document,"responses":responses(&plan)
        }),
    )
    .unwrap();
    assert!(s.map().layer(child).unwrap().anonymous);
    assert!(s.map().layer(child).unwrap().connection.is_empty());
    assert!(header(s.map().layer(child).unwrap().dataset.source.as_deref().unwrap()).is_none());
    assert!(!s.map().service_groups[0].anonymous, "the user's other children keep their login");
    assert_eq!(s.map().layer(other).unwrap().connection, id);
    let source = s.run("service.groupPlan", &json!({"group":group,"sourceOnly":true})).unwrap();
    assert_eq!(header(source["url"].as_str().unwrap()).unwrap().1, "mixed-group-sentinel-757");
}

#[test]
fn plugin_downloads_stay_anonymous_after_the_front_end_adds_them() {
    super::isolate_plugins();
    let mut s = session(None);
    let origin = "https://deferred-plugin757.example.org";
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"deferred-sentinel-757"})).unwrap();
    super::plugin("eng-deferred-login-757", &["edit", "network", "files"]);
    s.run_leaving_downloads(
        "plugin:eng-deferred-login-757/f",
        &json!({
            "op":"layer.addUrl","params":{"url":format!("{origin}/roads.geojson"),"name":"Deferred","anonymous":false,"service":true}
        }),
    )
    .unwrap();
    let request = s.take_url_requests().pop().unwrap();
    // The claimed UI path preserves the engine's bound parameters and anonymous provenance.
    let mut pending = request.params().clone();
    assert_eq!(pending["anonymous"], true);
    assert!(header(pending["url"].as_str().unwrap()).is_none());
    let token = s.download_started_for(&request).unwrap();
    assert!(s.download_landed(token, br#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}]}"#.to_vec()));
    pending["id"] = json!(token);
    s.complete_download(token, &pending).unwrap();
    s.run("layer.addUrl", &json!({"url":format!("{origin}/deferred.pmtiles"),"anonymous":true,"name":"Archive"})).unwrap();
    s.run("connection.login", &json!({"id":id,"apiKey":"deferred-sentinel-757"})).unwrap();
    let layer = s.map().layers.iter().find(|l| l.name == "Deferred").unwrap();
    assert!(layer.connection.is_empty());
    assert!(header(layer.dataset.source.as_deref().unwrap()).is_none());
    let tile = s.map().tiles.iter().find(|t| t.name == "Archive").unwrap();
    assert!(tile.connection.is_empty());
    assert!(header(&tile.url).is_none());
    rustgis_plugins::unload("eng-deferred-login-757");
}

#[test]
fn plugin_legends_on_unconnected_layers_never_acquire_a_login() {
    super::isolate_plugins();
    let mut s = session(None);
    let origin = "https://unconnected-plugin757.example.org";
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"Unconnected"})).unwrap();
    super::plugin("eng-unconnected-login-757", &["edit", "network", "files"]);
    super::forward(
        &mut s,
        "eng-unconnected-login-757",
        "map.setTiles",
        json!({
            "name":"Unconnected","legend":format!("{origin}/plugin-legend.png")
        }),
    )
    .unwrap();
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"unconnected-sentinel-757"})).unwrap();
    let tile = s.map().tiles.iter().find(|t| t.name == "Unconnected").unwrap();
    assert!(tile.connection.is_empty());
    assert!(header(&tile.url).is_none());
    assert!(header(&tile.legends[0].url).is_none());
    rustgis_plugins::unload("eng-unconnected-login-757");
}

#[test]
fn only_web_services_compare_a_test_with_their_saved_definition() {
    let mut s = session(None);
    let db = new(&mut s, json!({"kind":"postgres","address":"postgres:host=db757.example.org dbname=gis user=ana"}));
    let out = s
        .run(
            "connection.test",
            &json!({"id":db,"probe":true,"kind":"mysql","address":"postgres:host=elsewhere dbname=gis user=ana","auth":null,"user":7}),
        )
        .unwrap();
    assert_eq!(out["connection"], db);
}

#[test]
fn a_streamed_layer_that_failed_before_the_login_reopens_on_its_new_address() {
    let origin = "https://stream757.example.org";
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"ogcapi","address":format!("{origin}/ogc"),"auth":"header","keyName":"X-Key"}));
    let failed = |n: u32, path: String| {
        let mut layer = rustgis_map::Layer::new(n, rustgis_data::Dataset::new("Roads", rustgis_data::GeometryType::None, Vec::new()));
        layer.connection = id.clone();
        let mut source: rustgis_map::source::SourceLayer = serde_json::from_value(json!({ "path": path })).unwrap();
        source.error = Some("the server refused the request".into());
        layer.source = Some(source);
        layer
    };
    // One layer reads at the connection's origin; another, linked from it, reads elsewhere.
    s.project.map_mut().layers.push(failed(1, format!("{origin}/ogc/collections/roads/items")));
    s.project.map_mut().layers.push(failed(2, "https://linked757.example.org/items".into()));
    s.run("connection.login", &json!({"id":id,"apiKey":"stream-sentinel-757"})).unwrap();
    let [own, linked] = &s.map().layers[..] else { panic!("two layers") };
    let own = own.source.as_ref().unwrap();
    assert_eq!(header(own.path.as_deref().unwrap()).unwrap().1, "stream-sentinel-757");
    assert!(own.error.is_none() && !own.is_open());
    // Its address did not change, so its failure stands.
    assert!(linked.source.as_ref().unwrap().error.is_some());
}

#[test]
fn a_plain_replacement_legend_reads_through_the_tiles_logged_in_connection() {
    let origin = "https://legend757.example.org";
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"legend-sentinel-757"})).unwrap();
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"T","connection":id})).unwrap();
    s.run("map.setTiles", &json!({"name":"T","legend":[format!("{origin}/legend.png"),"https://other757.example.org/legend.png"]}))
        .unwrap();
    let legends = &s.map().tiles[0].legends;
    assert_eq!(header(&legends[0].url).unwrap().1, "legend-sentinel-757");
    assert_eq!(crate::sources::redact(&legends[0].url), format!("{origin}/legend.png"));
    assert_eq!(legends[1].url, "https://other757.example.org/legend.png");
}

#[test]
fn geoprocessing_history_never_keeps_a_login_binding() {
    let mut s = Session::new(crate::sample::world_project());
    let id = new(&mut s, json!({"kind":"wfs","address":"https://gp757.example.org/wfs","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"gp-sentinel-757"})).unwrap();
    let bound = bound(&mut s, &id, "https://gp757.example.org/wfs/roads.pmtiles");
    assert!(bound.starts_with(rustgis_data::login_address::PREFIX));
    // A failed write quotes where it went, and the history keeps the failed run's message.
    let input = s.map().layers.iter().find(|l| l.name.eq_ignore_ascii_case("cities")).map(|l| l.name.clone()).unwrap();
    let params = json!({"input":input,"minZoom":0,"maxZoom":0,"output":bound});
    let error = s.run("gp.run", &json!({"tool":"vectorToPmtiles","params":params})).unwrap_err();
    let record = s.project.gp_history.last().unwrap_or_else(|| panic!("the failed run is recorded: {error}"));
    let text = record.messages.join("\n");
    assert!(text.contains("https://gp757.example.org/wfs/roads.pmtiles"), "the message quotes the address: {text}");
    assert!(!text.contains(rustgis_data::login_address::PREFIX) && !text.contains("gp-sentinel-757"), "{text}");
    // The record's params keep the bound output to run again; what is written keeps none.
    assert!(record.params["output"].as_str().is_some_and(|o| o.starts_with(rustgis_data::login_address::PREFIX)));
    let (saved, _) = s.project.clone().saved_text(None).unwrap();
    assert!(!saved.contains(rustgis_data::login_address::PREFIX), "the saved project keeps a binding: {saved}");
    // Neither the project package nor the results package (its history.json) keeps one.
    for (command, file) in [("project.package", "project"), ("gp.package", "results")] {
        let path = std::env::temp_dir().join(format!("gp757-{file}-{}.rgispkg", std::process::id()));
        s.run(command, &json!({"path": path.to_string_lossy()})).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        for (name, entry) in rustgis_data::zip::read(&bytes).unwrap() {
            let entry = String::from_utf8_lossy(&entry);
            assert!(!entry.contains(rustgis_data::login_address::PREFIX), "{command}: {name} keeps a binding: {entry}");
        }
    }
}

#[test]
fn templates_tasks_and_models_written_out_keep_no_login_binding() {
    let mut s = Session::new(crate::sample::world_project());
    let id = new(&mut s, json!({"kind":"wmts","address":"https://export757.example.org/tiles","auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"export-sentinel-757"})).unwrap();
    let tile = bound(&mut s, &id, "https://export757.example.org/tiles/{z}/{x}/{y}.png");
    s.run("map.addTiles", &json!({"url": tile, "name": "Bound"})).unwrap();
    s.run("task.new", &json!({"name":"Fetch","steps":[{"title":"Add","command":"map.addTiles","params":{"url": tile}}]})).unwrap();
    // Keep the synthetic address in a tool's parameters, not a layer-name selector.
    s.run("model.new", &json!({"name":"Tiles","graph":{"nodes":[{"id":"tool1","kind":"tool","tool":"buffer","params":{"input": tile,"distance":1}}],"edges":[]}})).unwrap();
    // A graph input selects an existing layer by name; its actual source fields are cleaned.
    let graph = json!({"nodes":[{"id":"in","kind":"input","layer": "Cities"}], "edges": []});
    s.run("model.new", &json!({"name":"Drawn","graph": graph})).unwrap();
    let dir = std::env::temp_dir().join(format!("export757-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (template, task) = (dir.join("bound.rgistemplate"), dir.join("bound.rgistask"));
    s.run("project.saveTemplate", &json!({"path": template.to_string_lossy()})).unwrap();
    s.run("task.save", &json!({"task":"Fetch","path": task.to_string_lossy()})).unwrap();
    let model = s.run("model.export", &json!({"name":"Tiles"})).unwrap()["text"].as_str().unwrap().to_string();
    let drawn = s.run("model.export", &json!({"name":"Drawn"})).unwrap()["text"].as_str().unwrap().to_string();
    let graph: Value = serde_json::from_str(&drawn).unwrap();
    assert_eq!(graph["graph"]["nodes"][0]["layer"], "Cities");
    assert!(!drawn.contains(rustgis_data::login_address::PREFIX) && !drawn.contains("export-sentinel-757"));
    let written = [std::fs::read_to_string(&template).unwrap(), std::fs::read_to_string(&task).unwrap(), model];
    let _ = std::fs::remove_dir_all(&dir);
    for text in &written {
        assert!(!text.contains(rustgis_data::login_address::PREFIX) && !text.contains("export-sentinel-757"), "{text}");
        assert!(text.contains("export757.example.org/tiles"), "the plain address stays: {text}");
    }
}

#[test]
fn a_legend_that_cant_carry_the_login_is_refused_not_read_without_it() {
    let origin = "https://legend-space757.example.org";
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"space-sentinel-757"})).unwrap();
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"T","connection":id})).unwrap();
    let error = s.run("map.setTiles", &json!({"name":"T","legend":format!("{origin}/my legend.png")})).unwrap_err();
    assert!(error.contains("can't carry the connection's login"), "{error}");
    assert!(s.map().tiles[0].legends.is_empty(), "refused before anything changed");
}

#[test]
fn service_requests_take_a_connection_without_saying_they_ignore_it() {
    let url = "https://browse757.example.org/collections";
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"ogcapi","address":url,"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"fake006"})).unwrap();
    let document = json!({"collections":[{"id":"roads","title":"roads",
        "links":[{"rel":"items","type":"application/geo+json","href":format!("{url}/roads/items")}]}]});
    let mark = s.notice_mark();
    s.run("service.layers", &json!({"url":url,"type":"ogcapi","text":document.to_string(),"connection":id})).unwrap();
    s.run("service.groupPlan", &json!({"url":url,"type":"ogcapi","sourceOnly":true,"connection":id})).unwrap();
    let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert!(said.iter().all(|n| !n.contains("ignores")), "{said:?}");
}

#[test]
fn resolved_same_origin_tiles_keep_authenticated_and_public_connection_provenance() {
    use crate::cmd::service::add_resolved_tiles;
    for auth in ["header", "none"] {
        let origin = format!("https://resolved-{auth}-r16.example.invalid");
        let url = format!("{origin}/collections");
        let list = format!("{origin}/tiles");
        let template = format!("{origin}/paint/{{z}}/{{x}}/{{y}}.png");
        let document = json!({"collections":[{"id":"relief","links":[{"rel":"tilesets-map","href":list}]}]}).to_string();
        let mut s = session(None);
        let id = new(&mut s, json!({"kind":"ogcapi","address":url,"auth":auth,"keyName":"X-Key"}));
        if auth == "header" {
            s.run("connection.login", &json!({"id":id,"apiKey":"fake007"})).unwrap();
        }
        let template = bound(&mut s, &id, &template);
        add_resolved_tiles(&mut s, &json!({"url":url,"text":document,"layer":"relief (map tiles)","connection":id}), &template, false)
            .unwrap();
        let tile = &s.map().tiles[0];
        assert!(!tile.anonymous);
        assert_eq!(tile.connection, id);
        assert_eq!(header(&tile.url).is_some(), auth == "header");
        if auth == "none" {
            // No binding was present, but no foreign hop occurred: the saved connection is
            // still the user's provenance when they later add a login to it.
            s.run("connection.edit", &json!({"id":id,"auth":"header","keyName":"X-Key"})).unwrap();
            s.run("connection.login", &json!({"id":id,"apiKey":"fake008"})).unwrap();
            assert_eq!(header(&s.map().tiles[0].url).unwrap().1, "fake008");
        }
    }
}

#[test]
fn notebook_output_keeps_no_login_binding() {
    #[cfg(not(target_arch = "wasm32"))]
    if super::isolate_process_test("tests::service_logins::notebook_output_keeps_no_login_binding") {
        return;
    }
    let origin = "https://notebook757.example.org";
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wmts","address":format!("{origin}/tiles"),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"notebook-sentinel-757"})).unwrap();
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"T","connection":id})).unwrap();
    assert!(s.map().tiles[0].url.starts_with(rustgis_data::login_address::PREFIX));
    s.run("notebook.new", &json!({"name": "Tiles"})).unwrap();
    s.run("notebook.addCell", &json!({"source": "map.tiles"})).unwrap();
    s.run("notebook.run", &Value::Null).unwrap();
    let output = s.project.notebooks[0].cells.iter().filter_map(|c| c.output.clone()).collect::<Vec<_>>().join("\n");
    assert!(output.contains(&format!("{origin}/tiles/")), "the cell still shows the address: {output}");
    let (saved, _) = s.project.clone().saved_text(None).unwrap();
    assert!(!saved.contains(rustgis_data::login_address::PREFIX), "the saved project keeps a binding: {saved}");

    // An active secret can also be ordinary address syntax. It must not hide the scheme
    // from the separate URL-credential sanitizer before that sanitizer has run.
    s.run("connection.login", &json!({"id":id,"apiKey":"https"})).unwrap();
    let token = "RGSYNTH_notebook_collision_r18";
    s.run(
        "map.addTiles",
        &json!({"url":format!("https://tiles-collision.invalid/{{z}}/{{x}}/{{y}}.png?token={token}"),"name":"Collision"}),
    )
    .unwrap();
    s.run("notebook.run", &Value::Null).unwrap();
    let output = s.project.notebooks[0].cells.last().unwrap().output.as_deref().unwrap();
    assert!(!output.contains(token), "URL query secret survives a colliding active key");
    assert!(output.contains("tiles-collision.invalid/"), "the useful address survives: {output}");
    let (saved, _) = s.project.clone().saved_text(None).unwrap();
    assert!(!saved.contains(token));
    assert!(!saved.contains(rustgis_data::login_address::PREFIX));

    // The durable generated-output boundary also receives raw command results, such as
    // an imported notebook's display, independently of this notebook's run producer.
    let raw = s.run("map.tiles", &Value::Null).unwrap().to_string();
    assert!(raw.contains(token));
    let mut project = s.project.clone();
    project.notebooks[0].cells.last_mut().unwrap().output = Some(raw);
    let (saved, _) = project.saved_text(None).unwrap();
    assert!(!saved.contains(token), "durable generated text keeps a URL query secret");
    assert!(!saved.contains(rustgis_data::login_address::PREFIX));
}

/// The server has the credential before the lifecycle action, but its refusal arrives after it.
/// Exercise the public HEAD transport and the real session commands, without a credential store.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn delayed_head_refusal_keeps_secrets_out_of_history_across_login_lifecycle() {
    use rustgis_data::diagnostics::{Filter, History};
    use std::io::{Read, Write};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    if std::env::var_os("RUSTGIS_TEST_SERVICE_HEAD_DIAGNOSTICS").is_none() {
        // Other engine tests can fill the native HEAD admission limit and skip this probe.
        // Isolate the scenario so every iteration exercises the real public HEAD transport.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::service_logins::delayed_head_refusal_keeps_secrets_out_of_history_across_login_lifecycle",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RUSTGIS_TEST_SERVICE_HEAD_DIAGNOSTICS", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed; 0 failed"), "the child must run the delayed-HEAD test: {stdout}");
        return;
    }

    for action in ["active", "logout", "replace", "drop", "anonymous"] {
        for basic in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let secret = "delayed-head/sentinel+757";
            let value = if basic { logins::basic("head-user", secret) } else { secret.to_string() };
            let encoded = value.bytes().map(|b| format!("%{b:02X}")).collect::<String>();
            let payload = value.strip_prefix("Basic ").unwrap_or(&value).to_string();
            let echo = format!("{value} {payload} {encoded}");
            let (received_tx, received_rx) = channel();
            let (release_tx, release_rx) = channel();
            let expected = value.clone();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 2048];
                while request.len() < 32768 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    assert_ne!(n, 0);
                    request.extend_from_slice(&buf[..n]);
                }
                assert!(request.ends_with(b"\r\n\r\n"));
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("HEAD /probe HTTP/1.1\r\n"));
                if action == "anonymous" {
                    assert!(!request.to_ascii_lowercase().contains("x-key:") && !request.to_ascii_lowercase().contains("authorization:"));
                } else {
                    let name = if basic { "authorization" } else { "x-key" };
                    assert!(request.lines().any(|line| {
                        line.split_once(':').is_some_and(|(n, v)| n.eq_ignore_ascii_case(name) && v.trim_start() == expected)
                    }));
                }
                received_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                let id = if action == "anonymous" { "anonymous-request-id" } else { &echo };
                write!(stream, "HTTP/1.1 403 Forbidden\r\nx-request-id: {id}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            });
            let mut s = session(None);
            let id = new(
                &mut s,
                json!({"kind":"service", "address":base, "auth":if basic {"basic"} else {"header"}, "user":"head-user", "keyName":"X-Key"}),
            );
            let credential = if basic { "password" } else { "apiKey" };
            let login = json!({"id":id, credential:secret, "allowInsecure":true});
            let plain = format!("{base}/probe");
            let url = if action == "anonymous" {
                plain.clone()
            } else {
                s.run("connection.login", &login).unwrap();
                bound(&mut s, &id, &plain)
            };
            let history = History::default();
            let _scope = history.enter();
            let (done_tx, done_rx) = channel();
            crate::render::rustgis_tiles::cache::head(&url, Box::new(move |r| done_tx.send(r).unwrap()));
            received_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            match action {
                "logout" => {
                    s.run("connection.logout", &json!({"id":id})).unwrap();
                }
                "replace" => {
                    s.run("connection.login", &json!({"id":id,credential:"replacement-head-secret","allowInsecure":true})).unwrap();
                }
                "drop" => drop(s),
                _ => {}
            }
            if matches!(action, "logout" | "replace" | "drop") {
                assert!(logins::apply(&url).is_err(), "{action} must revoke the old generation");
            }
            release_tx.send(()).unwrap();
            assert!(done_rx.recv_timeout(Duration::from_secs(10)).unwrap().is_err());
            server.join().unwrap();
            let records = history.records();
            assert_eq!(records.len(), 1, "{action}: {records:?}");
            let network = records[0].network.as_ref().unwrap();
            assert_eq!(network.status, Some(403));
            assert_eq!(network.method, "HEAD");
            assert_eq!(network.url, plain);
            let text = format!("{records:?}\n{}", history.report(Filter::All));
            assert!(text.contains("403"), "{action}: {text}");
            assert!(text.contains(&plain), "{action}: {text}");
            for private in [secret, &value, &payload, &encoded] {
                assert!(!text.contains(private), "{action}: credential reached History or copied report");
            }
            if action == "anonymous" {
                assert!(text.contains("x-request-id: anonymous-request-id"), "{text}");
            } else {
                assert!(!text.contains("x-request-id"), "{action}: {text}");
            }
        }
    }
}

/// A foreign capability fallback cannot select a destination login now or after reopening.
pub(super) fn foreign_service_fallback_stays_anonymous(kind: &str, source: &str, layer: &str, clean: &str, foreign: &str) {
    let mut results = Vec::new();
    for (crossed, text) in [(false, clean), (true, foreign)] {
        let mut s = session(None);
        let a = new(&mut s, json!({"kind":kind,"address":source,"auth":"header","keyName":"X-Key"}));
        let b = new(&mut s, json!({"kind":kind,"address":"https://fallback-b.invalid","auth":"header","keyName":"X-Key"}));
        s.run("connection.login", &json!({"id":a,"apiKey":"RGSYNTH_fallback_a"})).unwrap();
        s.run("connection.login", &json!({"id":b,"apiKey":"RGSYNTH_fallback_b"})).unwrap();
        s.run("service.add", &json!({"url":source,"layer":layer,"text":text,"connection":a,"time":"default"})).unwrap();
        for reopened in [false, true] {
            if reopened {
                let saved = s.project.saved_text(None).unwrap().0;
                s.run("project.openText", &json!({"text":saved})).unwrap();
            }
            s.run("connection.login", &json!({"id":b,"apiKey":"RGSYNTH_fallback_rotated"})).unwrap();
            let tile = &s.map().tiles[0];
            results.push((
                crossed,
                reopened,
                if crossed {
                    tile.anonymous && tile.connection.is_empty() && header(&tile.url).is_none()
                } else {
                    !tile.anonymous && tile.connection == a && header(&tile.url).is_some()
                },
            ));
        }
    }
    assert!(results.iter().all(|(_, _, kept)| *kept), "foreign fallback adopted a login or same-origin binding changed: {results:?}");
}

/// A foreign capabilities document remains foreign even when it links back to the source origin.
#[test]
fn capabilities_round_trip_keeps_foreign_lineage() {
    let mut results = Vec::new();
    for crossed in [false, true] {
        let mut s = session(None);
        let a = "https://lineage-a.invalid";
        let bridge = if crossed { "https://lineage-b.invalid/bridge" } else { "https://lineage-a.invalid/bridge" };
        let id = new(&mut s, json!({"kind":"ogcapi","address":a,"auth":"header","keyName":"X-Key"}));
        s.run("connection.login", &json!({"id":id,"apiKey":"RGSYNTH_capabilities_lineage"})).unwrap();
        let link = |url: &str| json!({"links":[{"rel":"data","type":"application/json","href":url}]}).to_string();
        s.run("service.add", &json!({
            "url":format!("{a}/root"),"type":"ogcapi","connection":id,"layer":"relief (map tiles)",
            "text":link(bridge),"documents":[
                {"url":bridge,"text":link(&format!("{a}/returned"))},
                {"url":format!("{a}/returned"),"text":json!({"collections":[{"id":"relief","links":[{"rel":"tilesets-map","href":format!("{a}/list")}]}]}).to_string()}
            ],"tiles":format!("{a}/paint/{{z}}/{{x}}/{{y}}.png")
        })).unwrap();
        let tile = &s.map().tiles[0];
        results.push((
            crossed,
            if crossed {
                tile.anonymous && tile.connection.is_empty() && header(&tile.url).is_none()
            } else {
                !tile.anonymous && tile.connection == id && header(&tile.url).is_some()
            },
        ));
    }
    assert!(results.iter().all(|(_, kept)| *kept), "capabilities discovery forgot its foreign hop: {results:?}");
}

/// Styles reached through capabilities retain the same origin history as raster/vector tiles.
#[test]
fn capabilities_styles_keep_foreign_lineage() {
    let mut results = Vec::new();
    for crossed in [false, true] {
        let mut s = session(None);
        let a = "https://style-lineage-a.invalid";
        let b = "https://style-lineage-b.invalid";
        let source = if crossed { b } else { a };
        let doc = format!("{source}/rest/services/X/VectorTileServer");
        let login = new(&mut s, json!({"kind":"ogcapi","address":a,"auth":"header","keyName":"X-Key"}));
        let other = new(&mut s, json!({"kind":"ogcapi","address":b,"auth":"header","keyName":"X-Key"}));
        s.run("connection.login", &json!({"id":login,"apiKey":"RGSYNTH_style_a"})).unwrap();
        s.run("connection.login", &json!({"id":other,"apiKey":"RGSYNTH_style_b"})).unwrap();
        s.run(
            "service.add",
            &json!({
                "url":format!("{a}/root"),"type":"ogcapi","connection":login,"layer":"style",
                "text":json!({"links":[{"rel":"data","type":"application/json","href":doc}]}).to_string(),
                "documents":[{"url":doc,"text":json!({"currentVersion":11,"name":"Private style"}).to_string()}]
            }),
        )
        .unwrap();
        for reopened in [false, true] {
            if reopened {
                let saved = s.project.saved_text(None).unwrap().0;
                s.run("project.openText", &json!({"text":saved})).unwrap();
            }
            s.run("connection.login", &json!({"id":other,"apiKey":"RGSYNTH_style_rotated"})).unwrap();
            let tile = &s.map().tiles[0];
            results.push((
                crossed,
                reopened,
                if crossed {
                    tile.anonymous && tile.connection.is_empty() && header(&tile.url).is_none()
                } else {
                    !tile.anonymous && tile.connection == login && header(&tile.url).is_some()
                },
            ));
        }
    }
    assert!(results.iter().all(|(_, _, kept)| *kept), "style discovery forgot its foreign hop: {results:?}");
}

/// Features reached through foreign capabilities keep the same opt-out as tile discovery.
#[test]
fn feature_group_plans_and_imports_keep_foreign_capabilities_anonymous() {
    #[cfg(not(target_arch = "wasm32"))]
    if super::isolate_process_test("tests::service_logins::feature_group_plans_and_imports_keep_foreign_capabilities_anonymous") {
        return;
    }
    for (document_crossed, items_crossed, authenticated) in
        [(false, false, false), (false, false, true), (true, false, true), (false, true, true)]
    {
        let crossed = document_crossed || items_crossed;
        let mut s = session(None);
        let a = "https://feature-lineage-a.invalid";
        let b = "https://feature-lineage-b.invalid";
        let bridge = if document_crossed { format!("{b}/bridge") } else { format!("{a}/bridge") };
        let items = if items_crossed { format!("{b}/items") } else { format!("{a}/items") };
        let login = new(&mut s, json!({"kind":"ogcapi", "address":a, "auth":if authenticated {"header"} else {"none"}, "keyName":"X-Key"}));
        if authenticated {
            s.run("connection.login", &json!({"id":login, "apiKey":"RGSYNTH_feature_lineage"})).unwrap();
        }
        let other = new(&mut s, json!({"kind":"ogcapi", "address":b, "auth":"header", "keyName":"X-Key"}));
        s.run("connection.login", &json!({"id":other, "apiKey":"RGSYNTH_feature_destination"})).unwrap();
        let link = |url: &str| json!({"links":[{"rel":"data", "type":"application/json", "href":url}]}).to_string();
        let mut params = json!({"url":format!("{a}/root"), "type":"ogcapi", "connection":login,
        "text":link(&bridge), "documents":[
            {"url":bridge,"text":link(&format!("{a}/returned"))},
            {"url":format!("{a}/returned"),"text":json!({"collections":[{"id":"roads", "links":[{"rel":"items", "type":"application/geo+json", "href":items}]}]}).to_string()}
        ]});
        let plan = s.run("service.groupPlan", &params).unwrap();
        let feature = &plan["layers"][0];
        assert_eq!(feature.get("anonymous").and_then(Value::as_bool).unwrap_or(false), crossed);
        let listed = s.run("service.layers", &params).unwrap();
        assert_eq!(listed["layers"][0].get("anonymous").and_then(Value::as_bool).unwrap_or(false), crossed);
        params["responses"] = json!([{"url":feature["offer"]["url"], "text":r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}]}"#}]);
        s.run("service.import", &params).unwrap();
        let layer = &s.map().layers[0];
        assert_eq!(layer.anonymous, crossed);
        assert_eq!(layer.connection.is_empty(), crossed);
        assert_eq!(header(layer.dataset.source.as_deref().unwrap()).is_none(), crossed || !authenticated);
        assert_eq!(layer.dataset.features.len(), 1);
        params["group"] = json!(s.map().service_groups[0].id);
        s.run("service.refresh", &params).unwrap();
        let layer = &s.map().layers[0];
        assert_eq!(layer.anonymous, crossed);
        assert_eq!(layer.connection.is_empty(), crossed);
        assert_eq!(header(layer.dataset.source.as_deref().unwrap()).is_none(), crossed || !authenticated);
        let saved = s.project.saved_text(None).unwrap().0;
        s.run("project.openText", &json!({"text":saved})).unwrap();
        let layer = &s.map().layers[0];
        assert_eq!(layer.anonymous, crossed);
        assert_eq!(layer.connection.is_empty(), crossed);
    }
}

/// A cloud-optimised GeoTIFF added by address from a saved web-service connection is a raster
/// layer read in place that records the connection, as the tile layer it was did, its address
/// bound to the connection's login (never carrying the key) and read with it; the login made
/// again, its address is bound to the new login and it is read again through it.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_cog_read_in_place_from_a_connection_records_it_and_follows_its_login() {
    let server = LoginFile::ranged(rustgis_data::raster::cog::example(512, 256));
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"ogcapi","address":format!("{}/collections",server.base),"auth":"header","keyName":"X-Key"}));
    s.run("connection.login", &json!({"id":id,"apiKey":"fixture-login-757","allowInsecure":true})).unwrap();
    let r = s.run("layer.addUrl", &json!({"url":format!("{}/scene.tif",server.base),"name":"Scene","connection":id})).unwrap();
    assert_eq!((r["inPlace"].as_bool(), r["width"].as_u64()), (Some(true), Some(512)), "{r}");
    let path = {
        let layer = &s.map().layers[0];
        let raster = layer.raster.as_ref().unwrap();
        assert!(raster.in_place && raster.data.is_some(), "{:?}", raster.error);
        assert_eq!(layer.connection, id);
        raster.path.clone().unwrap()
    };
    assert!(rustgis_data::login_address::is_wrapper(&path) && !path.contains("fixture-login-757"), "{path}");
    let carried = |server: &LoginFile| {
        let requests = server.requests.lock().unwrap();
        !requests.is_empty() && requests.iter().all(|r| r.to_ascii_lowercase().contains("\r\nx-key: fixture-login-757\r\n"))
    };
    assert!(carried(&server), "{:?}", server.requests.lock().unwrap());
    // Logged in again (a new binding): the layer's address follows it, and it is read through it.
    server.requests.lock().unwrap().clear();
    s.run("connection.login", &json!({"id":id,"apiKey":"fixture-login-757","allowInsecure":true})).unwrap();
    let layer = &s.map().layers[0];
    let raster = layer.raster.as_ref().unwrap();
    let again = raster.path.clone().unwrap();
    assert!(again != path && rustgis_data::login_address::is_wrapper(&again), "{again}");
    assert!(raster.data.is_some() && raster.error.is_none(), "{:?}", raster.error);
    assert_eq!(layer.connection, id);
    assert!(carried(&server), "{:?}", server.requests.lock().unwrap());
}

/// The query key rides on every request bound to the connection at its own origin, in place of a
/// copy of the parameter an address already had, and is in no saved text, notice or error.
#[test]
fn query_logins_send_the_key_only_to_their_origin_and_save_none() {
    let origin = "https://query757.example.org";
    let key = "query-sentinel/757 é";
    let encoded = "query-sentinel%2F757%20%C3%A9";
    let mut s = session(Some(Arc::new(MemorySecrets::new())));
    let id = new(&mut s, json!({"kind":"wmts","address":format!("{origin}/basemaps/v1/mosaics/wmts"),"auth":"query","keyName":"api_key"}));
    assert_eq!(s.connections.find(&id).map(|c| (c.auth.as_str(), c.key_name.as_str())), Some(("query", "api_key")));
    assert_eq!(
        s.run("connection.login", &json!({"id":id,"password":"not-a-key-757"})).unwrap_err(),
        "an API-key login takes an apiKey, not a password"
    );
    // Not a header: any characters travel, percent-encoded.
    s.run("connection.login", &json!({"id":id,"apiKey":key})).unwrap();
    assert!(logins::apply(&format!("{origin}/wmts")).unwrap().is_none(), "a raw URL is never authority to use the login");
    let address = bound(&mut s, &id, &format!("{origin}/tiles/1/2/3.png?layer=a&API_KEY=stale-757#top"));
    let applied = logins::apply(&address).unwrap().unwrap();
    assert_eq!(applied.url, format!("{origin}/tiles/1/2/3.png?layer=a&api_key={encoded}#top"));
    assert!(applied.header.is_none());
    assert!(!applied.url.contains("stale"), "the login's value replaces the address's own copy: {}", applied.url);
    for other in [
        "https://other757.example.org/wmts",
        "https://query757.example.org.evil.example/wmts",
        "https://query757.example.org:8443/wmts",
        "http://query757.example.org/wmts",
    ] {
        assert!(logins::apply(&bound(&mut s, &id, other)).unwrap().is_none(), "{other}");
        assert!(logins::apply(&logins::inherit(&address, other)).unwrap().is_none(), "{other}");
    }
    s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"name":"Mosaic","connection":id})).unwrap();
    let tile = s.map().tiles.iter().find(|t| t.name == "Mosaic").unwrap().url.clone();
    assert!(logins::apply(&tile).unwrap().unwrap().url.ends_with(&format!("{{z}}/{{x}}/{{y}}.png?api_key={encoded}")));
    // Nothing that is saved or shown holds the key, its encoding, or the runtime binding.
    s.notify(format!("server echoed {key} and {encoded}"), true);
    let error = s.run("map.addTiles", &json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),"kind":key})).unwrap_err();
    let (project, _) = s.project.saved_text(None).unwrap();
    let visible = format!(
        "{error}\n{}\n{}\n{project}\n{:?}\n{:?}",
        s.run("connection.list", &Value::Null).unwrap(),
        s.connections.to_json(),
        s.notices,
        s.database_logins
    );
    for private in [key, encoded, "query-sentinel", "rustgis-login"] {
        assert!(!visible.contains(private), "{private} reached a saved or shown text: {visible}");
    }
    assert!(visible.contains(r#""keyName":"api_key""#) || visible.contains(r#""keyName": "api_key""#));
    // A later session has the definition and no key: it asks for it.
    let mut later = session(None);
    later.connections = crate::connections::Connections::from_json(&s.connections.to_json());
    assert_eq!(later.run("connection.login", &json!({"id":id})).unwrap_err(), web::REQUIRED);
    // Another parameter name, like another origin, is another login.
    s.run("connection.edit", &json!({"id":id,"keyName":"token757"})).unwrap();
    assert!(logins::apply(&address).is_err());
    s.run("connection.login", &json!({"id":id,"apiKey":key})).unwrap();
    let renamed = bound(&mut s, &id, &format!("{origin}/wmts"));
    assert!(logins::apply(&renamed).unwrap().unwrap().url.ends_with(&format!("?token757={encoded}")));
    s.run("connection.logout", &json!({"id":id})).unwrap();
    assert!(logins::apply(&renamed).is_err());
    assert_eq!(logins::scrub(key), key, "a logged-out key is no longer held for redaction");
}

/// A key typed in the address is left out, said so, and its name (never its value) is offered;
/// an address with nothing to leave out says nothing.
#[test]
fn a_key_in_the_address_is_left_out_and_its_parameter_offered_as_the_login() {
    let origin = "https://offer757.example.org";
    let mut s = session(None);
    let mark = s.notice_mark();
    let made =
        s.run("connection.new", &json!({"kind":"wmts","address":format!("{origin}/wmts?api_key=typed-key-757&layer=a#frag")})).unwrap();
    // Said as a notice too, so the CLI and Python hear it: once, naming the parameter, never the key.
    let said = s.notices_since(mark);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].text.contains("(api_key)") && said[0].text.contains("API key in the query") && !said[0].text.contains("typed-key-757"));
    assert_eq!(made["stripped"], true);
    assert_eq!(made["strippedParameters"], json!(["api_key"]));
    assert_eq!(made["address"], format!("{origin}/wmts?layer=a"));
    assert!(!made.to_string().contains("typed-key-757"));
    // Already a query login: the parameter is the login, so nothing more is offered.
    let query = s
        .run(
            "connection.new",
            &json!({"kind":"wmts","name":"Query","address":format!("{origin}/q?zqoffer757=typed-custom-757&layer=a"),"auth":"query","keyName":"zqoffer757"}),
        )
        .unwrap();
    assert_eq!(query["stripped"], true);
    assert!(query.get("strippedParameters").is_none());
    assert_eq!(query["address"], format!("{origin}/q?layer=a"));
    assert!(!query.to_string().contains("typed-custom-757"));
    // Its name now marks any address that carries it as holding a key, saved or recorded.
    assert_eq!(crate::sources::redact(&format!("{origin}/other?ZQOffer757=typed-custom-757&x=1")), format!("{origin}/other?x=1"));
    assert!(crate::sources::holds_secret(&json!({"url":format!("{origin}/other?zqoffer757=typed-custom-757")})));
    // Clean addresses and ordinary parameters: no notice, nothing offered.
    let mark = s.notice_mark();
    let clean = s.run("connection.new", &json!({"kind":"wmts","name":"Clean","address":format!("{origin}/clean?layer=a&empty=")})).unwrap();
    assert!(s.notices_since(mark).is_empty(), "a clean address says nothing");
    assert!(clean.get("stripped").is_none() && clean.get("strippedParameters").is_none(), "{clean}");
    let plain = s
        .run(
            "connection.new",
            &json!({"kind":"wmts","name":"Plain query","address":format!("{origin}/p?layer=a&"),"auth":"query","keyName":"zqother757"}),
        )
        .unwrap();
    assert!(plain.get("stripped").is_none(), "{plain}");
    // A declared query key is a credential even when its name otherwise looks public.
    let mark = s.notice_mark();
    let custom = s.run("connection.new", &json!({"kind":"wmts","name":"Declared query key","address":format!("{origin}/declared?zqdeclared757=RGSYNTHdeclared757&layer=a"),"auth":"query","keyName":"zqdeclared757"})).unwrap();
    assert_eq!(custom["address"], format!("{origin}/declared?layer=a"));
    assert!(custom.get("omittedPublicParameterCount").is_none(), "{custom}");
    assert!(custom.get("omittedPublicParameters").is_none(), "{custom}");
    let edited_custom = s
        .run("connection.edit", &json!({"id":custom["id"],"address":format!("{origin}/declared?zqdeclared757=RGSYNTHedited757&layer=a")}))
        .unwrap();
    assert!(edited_custom.get("omittedPublicParameterCount").is_none(), "{edited_custom}");
    assert!(edited_custom.get("omittedPublicParameters").is_none(), "{edited_custom}");
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("public query component")));
    // An edit that moves a key into the address is the same.
    let edited = s.run("connection.edit", &json!({"id":clean["id"],"address":format!("{origin}/clean?token=edited-757")})).unwrap();
    assert_eq!(edited["stripped"], true);
    assert_eq!(edited["strippedParameters"], json!(["token"]));
    // A key written as a bare name (no `=`) is left out but never offered or quoted.
    let mark = s.notice_mark();
    let bare = s
        .run("connection.new", &json!({"kind":"wmts","name":"Bare","address":format!("{origin}/bare?apikey9f3cRGSYNTHbare757&layer=a")}))
        .unwrap();
    assert_eq!(bare["stripped"], true, "{bare}");
    assert!(bare.get("strippedParameters").is_none(), "{bare}");
    let said = format!("{bare}{:?}", s.notices_since(mark));
    assert!(!said.contains("RGSYNTHbare757"), "{said}");
    // Legacy credential-bearing ampersand pairs are dropped conservatively. Public options
    // lost alongside a key are counted separately, never offered as credential names.
    let legacy_mark = s.notice_mark();
    let legacy = s
        .run(
            "connection.new",
            &json!({"kind":"wmts","name":"Legacy separators","address":format!("{origin}/legacy?x=1;token=RGSYNTHlegacy757")}),
        )
        .unwrap();
    assert_eq!(legacy["stripped"], true);
    assert_eq!(legacy["strippedParameters"], json!(["token"]));
    assert_eq!(legacy["address"], format!("{origin}/legacy"));
    assert_eq!(legacy["omittedPublicParameterCount"], 1);
    assert_eq!(legacy["omittedPublicParameters"], json!(["x"]));
    let collateral: Vec<_> = s.notices_since(legacy_mark).iter().filter(|n| n.text.contains("public query component")).collect();
    assert_eq!(collateral.len(), 1);
    assert!(collateral[0].text.contains("Legacy separators") && collateral[0].text.contains("first names: x"));
    assert!(!collateral[0].text.contains("RGSYNTHlegacy757"));
    let repeated = s.run("connection.new", &json!({"kind":"wmts","name":"Repeated public option","address":format!("{origin}/legacy?x=1;token=RGSYNTHlegacyRepeated&x=1&style=light")})).unwrap();
    assert_eq!(repeated["address"], format!("{origin}/legacy?x=1&style=light"));
    assert_eq!(repeated["omittedPublicParameterCount"], 1);
    assert_eq!(repeated["omittedPublicParameters"], json!(["x"]));

    // Semicolon suffixes after a key may be its value, never public names or login hints.
    for (i, suffix) in ["RGSYNTHopaque757=x", "tokenRGSYNTHopaque757=x"].iter().enumerate() {
        let mark = s.notice_mark();
        let result = s
            .run(
                "connection.new",
                &json!({"kind":"wmts","name":format!("Private suffix {i}"),"address":format!("{origin}/private?token=p;{suffix}")}),
            )
            .unwrap();
        let shown = format!("{result}{:?}", s.notices_since(mark));
        assert!(!shown.contains("RGSYNTHopaque757"), "{shown}");
        assert!(result.get("omittedPublicParameters").is_none(), "{result}");
        assert_eq!(result["strippedParameters"], json!(["token"]));
    }
    let mark = s.notice_mark();
    let userinfo = s
        .run(
            "connection.new",
            &json!({"kind":"wmts","name":"Private authority","address":"https://u:p?RGSYNTHopaque757=x@host.example/wmts"}),
        )
        .unwrap();
    let shown = format!("{userinfo}{:?}", s.notices_since(mark));
    assert!(!shown.contains("RGSYNTHopaque757"), "{shown}");
    assert!(userinfo.get("omittedPublicParameters").is_none() && userinfo.get("strippedParameters").is_none(), "{userinfo}");

    // A custom key name is a redaction hint even before a method is chosen; the saved definition
    // must be as safe as its replay log. An explicit no-login method leaves ordinary data alone.
    let no_method = s
        .run("connection.new", &json!({"kind":"wmts","name":"Custom without method","address":format!("{origin}/custom?zqwithoutmethod757=RGSYNTHnomethod757&layer=a"),"keyName":"zqwithoutmethod757"}))
        .unwrap();
    assert_eq!(no_method["stripped"], true);
    assert_eq!(no_method["strippedParameters"], json!(["zqwithoutmethod757"]));
    assert_eq!(no_method["address"], format!("{origin}/custom?layer=a"));
    assert!(no_method.get("omittedPublicParameterCount").is_none());
    assert!(!s.connections.to_json().contains("RGSYNTHnomethod757"));
    let public_hint = s.run("connection.new", &json!({"kind":"wmts","name":"Public name used as key hint","address":format!("{origin}/custom?layer=public-code&style=light"),"keyName":"layer"})).unwrap();
    assert_eq!(public_hint["address"], format!("{origin}/custom?style=light"));
    assert_eq!(public_hint["strippedParameters"], json!(["layer"]));
    assert!(public_hint.get("omittedPublicParameterCount").is_none());
    let explicit_none = s
        .run("connection.new", &json!({"kind":"wmts","name":"Explicit no login","address":format!("{origin}/none?zqordinary757=public-code"),"keyName":"zqordinary757","auth":"none"}))
        .unwrap();
    assert!(explicit_none.get("stripped").is_none());
    assert_eq!(explicit_none["address"], format!("{origin}/none?zqordinary757=public-code"));
}

/// A key in a query login is sent on the wire to the origin (and only as the parameter the
/// connection names), and the saved definition and project hold none.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_query_login_authenticates_requests_to_its_server_without_saving_the_key() {
    use crate::render::rustgis_tiles::cache::{self, Payload, State, Want};
    let server = LoginFile::new(br#"{"queryLogin":true}"#.to_vec());
    let url = format!("{}/mosaics/wmts", server.base);
    let mut s = session(None);
    let id = new(&mut s, json!({"kind":"wmts","address":format!("{url}?api_key=fixture-query-757"),"auth":"query","keyName":"api_key"}));
    assert_eq!(s.connections.find(&id).unwrap().address, url);
    let fetch = |address: &str| {
        let started = std::time::Instant::now();
        loop {
            match cache::get(address, Want::Json) {
                State::Pending => {
                    assert!(started.elapsed() < std::time::Duration::from_secs(10), "query login fetch timed out");
                    std::thread::sleep(std::time::Duration::from_millis(2)); // test-wait: legacy polling for the query-login fetch
                }
                state => break state,
            }
        }
    };
    s.run("connection.login", &json!({"id":id,"apiKey":"fixture-query-757","allowInsecure":true})).unwrap();
    match fetch(&bound(&mut s, &id, &format!("{url}?SERVICE=WMTS"))) {
        State::Ready(payload) => match payload.as_ref() {
            Payload::Json(value) => assert_eq!(value["queryLogin"], true),
            _ => panic!("expected the authenticated JSON response"),
        },
        _ => panic!("the query login did not authenticate"),
    }
    assert!(matches!(fetch(&url), State::Failed(_)), "a bare URL stays anonymous");
    let requests = server.requests.lock().unwrap().clone();
    assert!(
        requests.iter().any(|r| r.starts_with("GET /mosaics/wmts?SERVICE=WMTS&api_key=fixture-query-757 HTTP/1.1\r\n")),
        "{requests:?}"
    );
    assert!(requests.iter().filter(|r| r.contains("fixture-query-757")).all(|r| r.matches("api_key=").count() == 1), "{requests:?}");
    let (project, _) = s.project.saved_text(None).unwrap();
    for saved in [s.connections.to_json(), project, format!("{:?}", s.notices)] {
        assert!(!saved.contains("fixture-query-757"), "{saved}");
    }
}

#[test]
fn query_login_parameters_never_panic() {
    let mut s = session(Some(Arc::new(MemorySecrets::new())));
    let id = new(&mut s, json!({"kind":"csw","address":"https://junkquery757.example.org/csw","auth":"query","keyName":"junk_key_757"}));
    let junk = [
        json!({"id":id,"auth":"query","keyName":"\u{0}","user":":"}),
        json!({"id":id,"auth":"QUERY ","keyName":"é"}),
        json!({"id":id,"auth":"query","keyName":"x".repeat(10_000),"address":"https://junkquery757.example.org/?%"}),
        json!({"id":id,"auth":"query","keyName":"a%41","apiKey":"\n"}),
        json!({"id":id,"apiKey":"\u{7f}"}),
        json!({"id":id,"apiKey":"x".repeat(5000)}),
        json!({"kind":"wms","address":"https://junkquery757.example.org/?;;&&=%zz#","auth":"query","keyName":"junk_key_757"}),
    ];
    for cmd in ["connection.new", "connection.edit", "connection.login", "connection.logout", "connection.test", "map.addTiles"] {
        for p in &junk {
            let _ = s.run(cmd, p);
        }
    }
}

#[test]
fn saved_typed_task_steps_remove_a_pasted_custom_query_key() {
    let mut s = session(None);
    let key = "RGSYNTH_typed_task_query";
    let step = |address: &str| json!({"title":"Connection","command":"connection.new","params":{"kind":"wmts","name":"Typed","address":address,"auth":"query","keyName":"zqx_replay757"}});
    s.run("task.new", &json!({"name":"Lossy","steps":[step(&format!("https://typed-task757.example/wmts?layer=a&zqx_replay757={key}"))]}))
        .unwrap();
    s.run("task.new", &json!({"name":"Clean","steps":[step("https://typed-task757.example/wmts?layer=a")]})).unwrap();
    let (project, _) = s.project.saved_text(None).unwrap();
    assert!(!project.contains(key), "{project}");
    assert!(project.contains("layer=a"), "ordinary request parameters remain: {project}");
    assert!(
        crate::sources::redact(&format!("https://typed-task757.example/wmts?zqx_replay757={key}")).contains(key),
        "the replay step did not register its name"
    );
}

#[test]
fn rejected_query_commands_never_log_their_pasted_custom_key() {
    let mut s = session(None);
    let address = |name: &str, value: &str| format!("https://rejected-log757.example/wmts?layer=a&{name}={value}");
    s.run("connection.new", &json!({"name":"Taken","kind":"wmts","address":"https://rejected-log757.example/taken"})).unwrap();
    let target =
        s.run("connection.new", &json!({"name":"Target","kind":"wmts","address":"https://rejected-log757.example/target"})).unwrap();
    let rejected_new = json!({"name":"Taken","kind":"wmts","address":address("zq_rejected_new757","RGSYNTH_rejected_new"),"auth":"query","keyName":"zq_rejected_new757"});
    let rejected_edit = json!({"id":target["id"],"name":"Taken","address":address("zq_rejected_edit757","RGSYNTH_rejected_edit"),"auth":"query","keyName":"zq_rejected_edit757"});
    assert!(s.run("connection.new", &rejected_new).is_err());
    assert!(s.run("connection.edit", &rejected_edit).is_err());
    s.run(
        "connection.new",
        &json!({"name":"Clean control","kind":"wmts","address":address("layer2","clean"),"auth":"query","keyName":"zq_clean_control757"}),
    )
    .unwrap();
    // An edit that leaves out `auth` keeps the saved query login: its keyName still names the key.
    let query = s
        .run("connection.new", &json!({"name":"Saved query","kind":"wmts","address":"https://rejected-log757.example/q","auth":"query","keyName":"zq_saved757"}))
        .unwrap();
    let no_auth = json!({"id":query["id"],"name":"Taken","address":address("zq_fresh_noauth757","RGSYNTH_rejected_noauth"),"keyName":"zq_fresh_noauth757"});
    assert!(s.run("connection.edit", &no_auth).is_err());
    // A database connection's address error never quotes a query login's key.
    let db_error = s
        .run(
            "connection.new",
            &json!({"kind":"postgres","address":address("zq_db757","RGSYNTH_rejected_db"),"auth":"query","keyName":"zq_db757"}),
        )
        .unwrap_err();
    assert!(!db_error.contains("RGSYNTH_rejected_db") && db_error.contains("layer=a"), "{db_error}");
    let entries: Vec<_> = s.log.entries().filter(|e| e.command.starts_with("connection.")).collect();
    let raw = serde_json::to_string(&entries).unwrap();
    for secret in ["RGSYNTH_rejected_new", "RGSYNTH_rejected_edit", "RGSYNTH_rejected_noauth", "RGSYNTH_rejected_db"] {
        assert!(!raw.contains(secret), "{secret}: {raw}");
    }
    assert!(raw.contains("layer=a") && raw.contains("layer2=clean"), "ordinary parameters stay in replayable log text: {raw}");
    for csv in [false, true] {
        let mut exported = Vec::new();
        s.log.write(&mut exported, csv).unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(
            !["RGSYNTH_rejected_new", "RGSYNTH_rejected_edit", "RGSYNTH_rejected_noauth", "RGSYNTH_rejected_db"]
                .iter()
                .any(|k| text.contains(k)),
            "csv={csv}: {text}"
        );
    }
    for key in ["zq_rejected_new757", "zq_rejected_edit757", "zq_fresh_noauth757", "zq_db757"] {
        let original = address(key, "RGSYNTH_unregistered");
        assert_eq!(crate::sources::redact(&original), original, "rejected command registered {key}");
    }
}

#[test]
fn later_query_add_and_edit_report_existing_saved_secret_removals() {
    for edit in [false, true] {
        for lossy in [false, true] {
            let key = format!("zqsessionrefresh757{}{}", usize::from(edit), usize::from(lossy));
            let endpoint = if lossy {
                format!("https://session-refresh757.example/wmts?{key}=RGSYNTH_late_session&layer=a")
            } else {
                "https://session-refresh757.example/wmts?layer=a".to_string()
            };
            let future = json!({"id":"future","name":"Future","kind":"future","address":endpoint,"choice":17});
            let safe =
                json!({"id":"safe","name":"Safe","kind":"future","address":"https://session-refresh757.example/wmts?layer=a","choice":23});
            let prefs = json!({
                "futureEndpoint":endpoint,"futureClean":{"folded":["c1"]},
                "connections":[
                    {"id":"c1","name":"Earlier plain","kind":"wmts","address":endpoint,"futureEndpoint":endpoint,"futureClean":31},
                    {"id":"c2","name":"Target","kind":"wmts","address":"https://session-refresh757.example/target"},
                    future,safe
                ]
            });
            let mut s = session(None);
            s.load_connections(&prefs.to_string());
            assert!(s.connections.to_json().contains("futureEndpoint"));
            if lossy {
                assert!(s.connections.to_json().contains("RGSYNTH_late_session"), "name is not recognized before acceptance");
            }
            let mark = s.notice_mark();
            let accepted = if edit {
                s.run("connection.edit", &json!({"id":"c2","auth":"query","keyName":key})).unwrap()
            } else {
                s.run(
                    "connection.new",
                    &json!({"name":"Query","kind":"wmts","address":"https://session-refresh757.example/target","auth":"query","keyName":key}),
                )
                .unwrap()
            };
            let notices = s.notices_since(mark);
            if lossy {
                assert_eq!(notices.len(), 2, "{notices:?}");
                assert!(notices.iter().any(|n| n.text.starts_with("1 saved connection address had a password, key or token removed")));
                assert!(notices.iter().any(|n| n.text.starts_with("3 entries of the saved connections were dropped")));
            } else {
                assert!(notices.is_empty(), "{notices:?}");
            }
            let saved = s.connections.to_json();
            let output: Value = serde_json::from_str(&saved).unwrap();
            assert_eq!(s.connections.find("c1").unwrap().address, "https://session-refresh757.example/wmts?layer=a");
            assert_eq!(output["futureClean"], prefs["futureClean"]);
            assert_eq!(output["connections"][0]["futureClean"], 31);
            assert!(output["connections"].as_array().unwrap().contains(&safe));
            if lossy {
                assert!(!format!("{saved}{accepted}{notices:?}").contains("RGSYNTH_late_session"));
                assert!(output.get("futureEndpoint").is_none());
                assert!(output["connections"][0].get("futureEndpoint").is_none());
                assert!(!output["connections"].as_array().unwrap().contains(&future));
            } else {
                assert_eq!(output["futureEndpoint"], prefs["futureEndpoint"]);
                assert_eq!(output["connections"][0]["futureEndpoint"], prefs["connections"][0]["futureEndpoint"]);
                assert!(output["connections"].as_array().unwrap().contains(&future));
            }
            // No repeated loss notice when the same accepted query definition is edited again.
            let mark = s.notice_mark();
            s.run("connection.edit", &json!({"id":accepted["id"],"name":"Renamed query"})).unwrap();
            assert!(s.notices_since(mark).is_empty());
        }
    }
}

#[test]
fn refused_literal_plus_query_rows_report_drops_without_losing_clean_unknowns() {
    for lossy in [false, true] {
        let endpoint = if lossy {
            "https://plus-session757.example/wmts?zq%2Bcustomsession757=RGSYNTH_refused_session_plus&layer=a"
        } else {
            "https://plus-session757.example/wmts?zqcustomsession757=dataset&layer=a"
        };
        let row = json!({
            "id":"plus","name":"Unsupported plus","kind":"wmts","address":endpoint,
            "auth":"query","keyName":"zq+customsession757","futureClean":{"choice":13}
        });
        let mut s = session(None);
        let mark = s.notice_mark();
        s.load_connections(&json!({"connections":[row]}).to_string());
        assert!(s.connections.connections.is_empty());
        let notices = s.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        let saved = s.connections.to_json();
        if lossy {
            assert!(notices[0].text.starts_with("1 entry of the saved connections was dropped"));
            assert!(!format!("{saved}{notices:?}").contains("RGSYNTH_refused_session_plus"));
        } else {
            assert!(notices[0].text.starts_with("1 saved connection couldn't be read"));
            assert!(!notices[0].text.contains("dropped"));
            let output: Value = serde_json::from_str(&saved).unwrap();
            assert_eq!(output["connections"][0], row);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_remembered_query_login_reconnects_on_the_wire_without_leaking_into_history_or_recording() {
    use rustgis_data::diagnostics::{Filter, History};
    use std::sync::mpsc::channel;
    use std::time::Duration;
    let server = LoginFile::new(Vec::new());
    let store = Arc::new(MemorySecrets::new());
    let url = format!("{}/mosaics/wmts?SERVICE=WMTS", server.base);
    let history = History::default();
    let _scope = history.enter();
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"wmts","address":url,"auth":"query","keyName":"api_key"}));
    // This key deliberately differs from the fixture's accepted one: History retains network
    // failures, not successful requests. The refusal must still have received the saved key.
    s.run("connection.login", &json!({"id":id,"apiKey":"remembered-refused-query-1979","remember":true,"allowInsecure":true})).unwrap();
    s.run("project.openText", &json!({"text":project(&server.base,&id)})).unwrap();
    let (saved_project, _) = s.project.saved_text(None).unwrap();
    let prefs = serde_json::Map::from_iter([(crate::connections::PREF.to_string(), Value::String(s.connections.to_json()))]);
    drop(s);
    let mut later = session(None);
    crate::prefs::use_saved_secrets(&mut later, &prefs, store);
    later.run("project.openText", &json!({"text":saved_project})).unwrap();
    assert_eq!(requests(&mut later), json!([]));
    let address = bound(&mut later, &id, &url);
    let applied = logins::apply(&address).unwrap().unwrap();
    assert_eq!(applied.url, format!("{url}&api_key=remembered-refused-query-1979"));
    assert!(applied.header.is_none());
    let (tx, rx) = channel();
    crate::render::rustgis_tiles::cache::head(&address, Box::new(move |result| tx.send(result).unwrap()));
    assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap().is_err());
    let network_requests = server.requests.lock().unwrap().clone();
    assert!(
        network_requests
            .iter()
            .any(|r| r.starts_with("HEAD /mosaics/wmts?SERVICE=WMTS&api_key=remembered-refused-query-1979 HTTP/1.1\r\n")),
        "{network_requests:?}"
    );
    let records = history.records();
    assert!(records.iter().any(|r| r.network.as_ref().is_some_and(|n| n.method == "HEAD" && n.status == Some(401))), "{records:?}");
    let mut recording = Vec::new();
    later.log.write(&mut recording, false).unwrap();
    let (project, _) = later.project.saved_text(None).unwrap();
    let public = format!(
        "{records:?}\n{}\n{}\n{}\n{project}\n{:?}",
        history.report(Filter::All),
        String::from_utf8(recording).unwrap(),
        later.connections.to_json(),
        later.notices
    );
    for private in ["remembered-refused-query-1979", "rustgis-login"] {
        assert!(!public.contains(private), "{private} reached History, recording or saved text: {public}");
    }
}
