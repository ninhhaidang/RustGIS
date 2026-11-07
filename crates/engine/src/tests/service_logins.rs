//! Service logins belong to a session/connection, never an origin-global request context.
use super::*;
use crate::{render::rustgis_tiles::logins, service_logins as web};
use rustgis_data::secrets::{MemorySecrets, SecretError, SecretStore};
use std::sync::Arc;

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
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
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
        std::thread::sleep(std::time::Duration::from_millis(5));
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
    assert_eq!(web::request(&mut s, Some(&id), "https://scope757.example.org/wms").unwrap_err(), web::REQUIRED);
}

#[test]
fn web_login_methods_and_encoded_basic_size_are_checked() {
    let mut s = session(None);
    for bad in [
        json!({"kind":"wms","address":"https://m757.example.org","auth":"digest"}),
        json!({"kind":"wms","address":"https://m757.example.org","auth":"query","keyName":"key"}),
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
    let store = Arc::new(MemorySecrets::failing(SecretError::Denied));
    let mut s = session(Some(store.clone()));
    let id = new(&mut s, json!({"kind":"ogcapi","address":"https://memory757.example.org/ogc","auth":"header","keyName":"X-Key"}));
    assert_eq!(s.run("connection.login", &json!({"id":id,"apiKey":"memory-sentinel-757","remember":true})).unwrap_err(), web::SESSION_ONLY);
    assert!(!s.database_logins.has_session_password(&id));
    s.run("connection.login", &json!({"id":id,"apiKey":"memory-sentinel-757"})).unwrap();
    let address = bound(&mut s, &id, "https://memory757.example.org/features");
    assert_eq!(header(&address).unwrap().1, "memory-sentinel-757");
    let duplicate = s.run("connection.duplicate", &json!({"id":id,"name":"Another account"})).unwrap();
    assert_eq!(duplicate["loggedIn"], false);
    assert_eq!(s.run("connection.test", &json!({"id":duplicate["id"],"probe":true})).unwrap_err(), web::REQUIRED);
    s.run("connection.edit", &json!({"id":id,"name":"Renamed","address":"https://memory757.example.org/v2"})).unwrap();
    assert_eq!(header(&address).unwrap().1, "memory-sentinel-757");
    // A stale remembered marker cannot opt a service into credential-store access.
    let mut prefs: Value = serde_json::from_str(&s.connections.to_json()).unwrap();
    prefs["connections"][0]["remembered"] = json!(true);
    let mut later = session(Some(store));
    later.connections = crate::connections::Connections::from_json(&prefs.to_string());
    assert!(!later.connections.find(&id).unwrap().remembered);
    assert_eq!(later.run("connection.login", &json!({"id":id})).unwrap_err(), web::REQUIRED);
    s.run("connection.edit", &json!({"id":id,"address":"https://changed-memory757.example.org/ogc"})).unwrap();
    assert!(logins::apply(&address).is_err());
    s.run("connection.login", &json!({"id":id,"apiKey":"fake004"})).unwrap();
    let replacement = bound(&mut s, &id, "https://changed-memory757.example.org/features");
    s.run("connection.remove", &json!({"id":id})).unwrap();
    assert!(logins::apply(&replacement).is_err());
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
    for cmd in
        ["connection.new", "connection.edit", "connection.login", "connection.logout", "connection.test", "map.addTiles", "service.add"]
    {
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
    assert_eq!(s.run("service.refresh", &json!({"group":5,"text":"not a document"})).unwrap_err(), web::REQUIRED);
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
