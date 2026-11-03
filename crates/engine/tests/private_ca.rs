//! Linux's platform verifier reads SSL_CERT_FILE and SSL_CERT_DIR. Each case gets its own process
//! and ephemeral PKI, never a change to the machine's root store or the suite's process environment.
#![cfg(target_os = "linux")]

#[path = "../../../tests/support/ogc.rs"]
mod ogc;

use rustgis_engine::render::rustgis_tiles::{cache, template};
use rustgis_engine::{Project, Session};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_CASE: &str = "RUSTGIS_PRIVATE_CA_CASE";
const CHILD_PKI: &str = "RUSTGIS_PRIVATE_CA_PKI";
const PIXEL: [u8; 4] = [17, 83, 149, 255];

#[test]
fn private_ca_platform_trust() {
    let pki = Pki::new();
    for case in ["trusted", "untrusted"] {
        let trust = if case == "untrusted" { "unrelated.pem" } else { "ca.pem" };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "private_ca_child", "--ignored", "--nocapture"])
            .env(CHILD_CASE, case)
            .env(CHILD_PKI, &pki.root)
            .env("SSL_CERT_FILE", pki.root.join(trust))
            .env("SSL_CERT_DIR", pki.root.join("empty-roots"));
        // Do not send this local fixture through a developer's or CI's proxy.
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            command.env_remove(name);
        }
        command.env("NO_PROXY", "localhost,127.0.0.1").env("no_proxy", "localhost,127.0.0.1");
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("private CA case {case} exceeded its 90-second deadline");
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "private CA case {case}: {status}");
    }
}

// Ignored in ordinary runs; only the entry above supplies an isolated trust environment.
#[test]
#[ignore = "subprocess helper for private_ca_platform_trust"]
fn private_ca_child() {
    let Ok(case) = std::env::var(CHILD_CASE) else { return };
    let root = PathBuf::from(std::env::var_os(CHILD_PKI).unwrap());
    assert!(matches!(case.as_str(), "trusted" | "untrusted"), "unknown private CA case {case}");
    let server = Server::new(&root);
    rustgis_engine::plugins::store::set_dir(Some(root.join(format!("plugins-{case}"))));
    let mut session = Session::new(Project::default());
    let trusted = case == "trusted";
    for (kind, name) in [("wms", "private_map"), ("wfs", "p:places")] {
        let url = format!("{}/{kind}", server.base);
        let tested = session.run("connection.test", &json!({"kind": kind, "address": url}));
        let listed = session.run("service.layers", &json!({"url": url, "type": kind}));
        if trusted {
            assert_layer(&tested.unwrap()["result"], name);
            let listed = listed.unwrap();
            assert_layer(&listed, name);
            let added = session.run("service.add", &json!({"url": url, "type": kind, "layer": name})).unwrap();
            if kind == "wms" {
                let index = usize::try_from(added["index"].as_u64().unwrap()).unwrap();
                let template = &session.map().tiles[index].url;
                let url = template::expand(template, template::TileId { z: 0, x: 0, y: 0 }, 256, false, &[]);
                assert_image(&url, true);
                assert_eq!(session.map().tiles.len(), 1);
            } else {
                let id = u32::try_from(added["layer"].as_u64().unwrap()).unwrap();
                let extent = session.map().layer(id).unwrap().extent();
                assert_eq!((extent.min_x, extent.min_y, extent.max_x, extent.max_y), (10.0, 50.0, 10.0, 50.0));
                let dataset = &session.map().layer(id).unwrap().dataset;
                assert_eq!(dataset.features.len(), 1);
                let field = dataset.field_index("name").unwrap();
                assert_eq!(dataset.value(0, field), &rustgis_engine::data::Value::Text("Test Place".into()));
            }
        } else {
            assert!(tested.is_err(), "{case}: connection.test accepted {kind}");
            assert!(listed.is_err(), "{case}: service.layers accepted {kind}");
        }
    }
    // The workers use a shared agent, separate from capabilities and feature downloads.
    // Check them in both cases, not only the successful map request above.
    assert_image(&format!("{}/tile.png", server.base), trusted);
    if trusted {
        let before = server.requests();
        assert!(cache::download_blocking(&format!("{}/redirect", server.base)).is_err());
        let after = server.requests();
        assert_eq!(&after[before.len()..], &["/redirect"]);
        // The public-only downloader must still refuse the loopback address, even with a
        // certificate the platform trusts. Its resolver must not reach this TLS server.
        assert!(cache::download_public_within(&format!("{}/tile.png", server.base), 1024 * 1024, Duration::from_secs(3)).is_err());
        assert_eq!(server.requests(), after);
        assert!(after.iter().any(|p| p.starts_with("/wms?") && p.to_ascii_lowercase().contains("request=getmap")));
        assert!(after.iter().any(|p| p.starts_with("/wfs?") && p.to_ascii_lowercase().contains("request=getfeature")));
    } else {
        // TLS must reject the certificate before an HTTP request is sent. No incidental
        // ureq/rustls/OS error wording is part of the assertion.
        assert!(server.requests().is_empty(), "{case}: HTTP reached the server: {:?}", server.requests());
    }
}

fn assert_layer(listed: &Value, name: &str) {
    let layers = listed["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 1, "{listed}");
    assert_eq!(layers[0]["name"], name);
}

fn assert_image(url: &str, trusted: bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match cache::get(url, cache::Want::Image) {
            cache::State::Ready(payload) => {
                assert!(trusted, "untrusted HTTPS image was accepted");
                let cache::Payload::Image(image) = &*payload else { panic!("image fetch returned another payload") };
                assert_eq!((image.width(), image.height()), (256, 256));
                assert!(image.data().as_chunks::<4>().0.iter().all(|pixel| *pixel == PIXEL), "decoded PNG pixels differ");
                return;
            }
            cache::State::Failed(error) => {
                assert!(!trusted, "HTTPS tile worker failed: {error}");
                return;
            }
            cache::State::Pending => {
                assert!(Instant::now() < deadline, "HTTPS tile worker did not settle within ten seconds: {url}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

struct Pki {
    root: PathBuf,
}

impl Pki {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rustgis-private-ca-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let pki = Self { root };
        std::fs::create_dir(pki.root.join("empty-roots")).unwrap();
        pki.write("index", "");
        pki.write("serial", "01\n");
        pki.write(
            "ca.cnf",
            "[ca]\ndefault_ca=issuer\n[issuer]\ndatabase=index\nserial=serial\nnew_certs_dir=.\ncertificate=ca.pem\nprivate_key=ca.key\ndefault_md=sha256\npolicy=names\nunique_subject=no\n[names]\ncommonName=supplied\n[root]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n",
        );
        pki.openssl(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=RustGIS ephemeral private CA",
            "-keyout",
            "ca.key",
            "-out",
            "ca.csr",
        ]);
        // Explicit wide dates avoid clock skew near generation time and fixtures that expire soon.
        pki.openssl(&[
            "ca",
            "-batch",
            "-selfsign",
            "-config",
            "ca.cnf",
            "-in",
            "ca.csr",
            "-out",
            "ca.pem",
            "-startdate",
            "20200101000000Z",
            "-enddate",
            "21200101000000Z",
            "-extensions",
            "root",
        ]);
        pki.openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "36500",
            "-subj",
            "/CN=Unrelated ephemeral CA",
            "-keyout",
            "unrelated.key",
            "-out",
            "unrelated.pem",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ]);
        pki.openssl(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
        ]);
        pki.openssl(&["pkcs8", "-topk8", "-nocrypt", "-in", "server.key", "-outform", "DER", "-out", "server-key.der"]);
        pki.write(
            "server.ext",
            "[server]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n",
        );
        pki.openssl(&[
            "ca",
            "-batch",
            "-config",
            "ca.cnf",
            "-in",
            "server.csr",
            "-out",
            "server.pem",
            "-startdate",
            "20200101000000Z",
            "-enddate",
            "21200101000000Z",
            "-extfile",
            "server.ext",
            "-extensions",
            "server",
        ]);
        pki.openssl(&["x509", "-in", "server.pem", "-outform", "DER", "-out", "server.der"]);
        pki
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root.join(name), text).unwrap();
    }

    fn openssl(&self, args: &[&str]) {
        let output =
            Command::new("openssl").current_dir(&self.root).args(args).output().expect("Linux regression requires openssl on PATH");
        assert!(output.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Server {
    base: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(root: &Path) -> Self {
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(std::fs::read(root.join("server-key.der")).unwrap()));
        let certs = vec![CertificateDer::from(std::fs::read(root.join("server.der")).unwrap())];
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("https://localhost:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, stopping, origin) = (requests.clone(), stop.clone(), base.clone());
        let mut pixels = tiny_skia::Pixmap::new(256, 256).unwrap();
        pixels.fill(tiny_skia::Color::from_rgba8(PIXEL[0], PIXEL[1], PIXEL[2], PIXEL[3]));
        let png = pixels.encode_png().unwrap();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("HTTPS accept: {e}"),
                };
                serve(socket, config.clone(), &origin, &png, &log);
            }
        });
        Self { base, stop, requests, worker: Some(worker) }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            // Every accepted connection has a bounded read/write timeout.
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn serve(socket: TcpStream, config: Arc<rustls::ServerConfig>, base: &str, png: &[u8], log: &Mutex<Vec<String>>) {
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    let connection = rustls::ServerConnection::new(config).unwrap();
    let mut stream = rustls::StreamOwned::new(connection, socket);
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    while bytes.len() < 16384 && !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
        let Ok(n) = stream.read(&mut buffer) else { return };
        if n == 0 {
            return;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    if !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
        return;
    }
    let request = String::from_utf8_lossy(&bytes);
    let target = request.split_whitespace().nth(1).unwrap_or("");
    log.lock().unwrap().push(target.into());
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query.to_ascii_lowercase();
    let parameter = |key: &str| query.split('&').find_map(|p| p.strip_prefix(&format!("{key}=")));
    let (status, content_type, body, extra) = match (path, parameter("request")) {
        ("/wms", Some("getcapabilities")) => (200, "text/xml", wms(base).into_bytes(), ""),
        ("/wfs", Some("getcapabilities")) => (200, "text/xml", wfs(base).into_bytes(), ""),
        ("/wms", Some("getmap"))
            if parameter("layers") == Some("private_map") && parameter("width") == Some("256") && parameter("height") == Some("256") =>
        {
            (200, "image/png", png.to_vec(), "")
        }
        ("/wfs", Some("getfeature")) if parameter("typenames").is_some_and(|s| s == "p%3aplaces" || s == "p:places") => {
            (200, "application/geo+json", ogc::fixture_features().as_bytes().to_vec(), "")
        }
        ("/tile.png", None) => (200, "image/png", png.to_vec(), ""),
        ("/redirect", None) => (302, "text/plain", Vec::new(), "Location: file:///rustgis-private-ca-must-not-be-read\r\n"),
        _ => (400, "text/plain", b"unexpected fixture request".to_vec(), ""),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .and_then(|_| stream.write_all(&body))
    .and_then(|_| stream.flush());
}

fn wms(base: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
  <Service><Name>WMS</Name><Title>Private CA maps</Title><OnlineResource xlink:type="simple" xlink:href="{base}/wms"/></Service>
  <Capability>
    <Request>
      <GetCapabilities><Format>text/xml</Format><DCPType><HTTP><Get><OnlineResource xlink:type="simple" xlink:href="{base}/wms"/></Get></HTTP></DCPType></GetCapabilities>
      <GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:type="simple" xlink:href="{base}/wms"/></Get></HTTP></DCPType></GetMap>
    </Request>
    <Exception><Format>XML</Format></Exception>
    <Layer><Title>Private CA maps</Title><CRS>EPSG:3857</CRS>
      <EX_GeographicBoundingBox><westBoundLongitude>-180</westBoundLongitude><eastBoundLongitude>180</eastBoundLongitude><southBoundLatitude>-85</southBoundLatitude><northBoundLatitude>85</northBoundLatitude></EX_GeographicBoundingBox>
      <Layer><Name>private_map</Name><Title>Private map</Title></Layer>
    </Layer>
  </Capability>
</WMS_Capabilities>"#
    )
}

fn wfs(base: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<wfs:WFS_Capabilities version="2.0.0" xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:ows="http://www.opengis.net/ows/1.1" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:p="urn:rustgis:private-ca">
  <ows:ServiceIdentification><ows:Title>Private CA features</ows:Title><ows:ServiceType>WFS</ows:ServiceType><ows:ServiceTypeVersion>2.0.0</ows:ServiceTypeVersion></ows:ServiceIdentification>
  <ows:OperationsMetadata>
    <ows:Operation name="GetCapabilities"><ows:DCP><ows:HTTP><ows:Get xlink:href="{base}/wfs"/></ows:HTTP></ows:DCP></ows:Operation>
    <ows:Operation name="GetFeature"><ows:DCP><ows:HTTP><ows:Get xlink:href="{base}/wfs"/></ows:HTTP></ows:DCP><ows:Parameter name="outputFormat"><ows:AllowedValues><ows:Value>application/json</ows:Value></ows:AllowedValues></ows:Parameter></ows:Operation>
  </ows:OperationsMetadata>
  <wfs:FeatureTypeList><wfs:FeatureType><wfs:Name>p:places</wfs:Name><wfs:Title>Private places</wfs:Title><wfs:DefaultCRS>urn:ogc:def:crs:OGC:1.3:CRS84</wfs:DefaultCRS><ows:WGS84BoundingBox><ows:LowerCorner>9 49</ows:LowerCorner><ows:UpperCorner>11 51</ows:UpperCorner></ows:WGS84BoundingBox></wfs:FeatureType></wfs:FeatureTypeList>
</wfs:WFS_Capabilities>"#
    )
}
