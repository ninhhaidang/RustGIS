//! Shared, test-only HTTP fixture server. Exact routes, recorded requests, bounded shutdown.
#![cfg(test)]
#![allow(dead_code)]

use std::io::{Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub const ROOT: &str = "/ogc/user/test-token";
pub const CATALOG: &str = "/ogc/user/test-token/catalog";
pub const ITEMS: &str = "/ogc/user/test-token/collections/places/items";
#[path = "ogc.rs"]
mod fixtures;
pub use fixtures::*;
pub const LINE_ITEMS: &str = "/ogc/user/test-token/collections/routes/items";

#[derive(Clone)]
pub struct Route {
    /// The path answered, or a path and query (`/query?resultOffset=2`) to answer that request
    /// alone (a page of a layer that comes in pages).
    pub path: String,
    /// The answer, in which `{base}` is the server's address.
    pub body: String,
    /// An answer of bytes, sent as they are instead (a protocol buffer page, say).
    pub bytes: Option<Vec<u8>>,
    pub status: u16,
    pub delay: Duration,
    /// More headers to answer with (a firewall's, say).
    pub headers: Vec<(String, String)>,
}

impl Route {
    pub fn new(path: &str, body: &str) -> Self {
        Self { path: path.into(), body: body.into(), bytes: None, status: 200, delay: Duration::ZERO, headers: Vec::new() }
    }

    /// A route answering `bytes` as they are (a protocol buffer page, say).
    pub fn bytes(path: &str, bytes: Vec<u8>) -> Self {
        Self { bytes: Some(bytes), ..Self::new(path, "") }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub path: String,
    pub query: Vec<(String, String)>,
}

pub struct Server {
    pub base: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    routes: Arc<Mutex<Vec<Route>>>,
}

impl Server {
    pub fn fixture() -> Self {
        Self::new(vec![
            Route::new(ROOT, fixture_landing()),
            Route::new(CATALOG, fixture_collections()),
            Route::new(ITEMS, fixture_features()),
        ])
    }

    pub fn group_fixture() -> Self {
        Self::new(vec![
            Route::new(ROOT, fixture_landing()),
            Route::new(CATALOG, fixture_group_collections()),
            Route::new(ITEMS, fixture_features()),
            Route::new(LINE_ITEMS, fixture_lines()),
        ])
    }

    pub fn set(&self, path: &str, body: &str) {
        let mut routes = self.routes.lock().unwrap();
        if let Some(route) = routes.iter_mut().find(|r| r.path == path) {
            route.body = body.into();
            route.bytes = None;
        } else {
            routes.push(Route::new(path, body));
        }
    }

    pub fn new(routes: Vec<Route>) -> Self {
        let routes = Arc::new(Mutex::new(routes));
        let serving = routes.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, stopping, origin) = (requests.clone(), stop.clone(), base.clone());
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("accept: {e}"),
                };
                // Accepted sockets can inherit the listener's nonblocking mode. Wait for the
                // request headers instead of treating WouldBlock as an empty request (404).
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                while bytes.len() < 8192 && !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = stream.read(&mut buffer) else { break };
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let request = String::from_utf8_lossy(&bytes);
                let target = request.split_whitespace().nth(1).unwrap_or("/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                log.lock().unwrap().push(Request {
                    path: path.into(),
                    query: query
                        .split('&')
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            let (key, value) = s.split_once('=').unwrap_or((s, ""));
                            (key.into(), value.into())
                        })
                        .collect(),
                });
                let route = {
                    let routes = serving.lock().unwrap();
                    routes.iter().find(|r| r.path == target).or_else(|| routes.iter().find(|r| r.path == path)).cloned()
                };
                let (status, body, extra) = match route {
                    Some(r) => {
                        let until = Instant::now() + r.delay;
                        while Instant::now() < until && !stopping.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        let extra: String = r.headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                        let body = r.bytes.unwrap_or_else(|| r.body.replace("{base}", &origin).into_bytes());
                        (r.status, body, extra)
                    }
                    None => (404, Vec::new(), String::new()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        Self { base, requests, stop, worker: Some(worker), routes }
    }

    pub fn root(&self) -> String {
        format!("{}{ROOT}", self.base)
    }
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn assert_fixture_requests(&self, paths: &[&str]) {
        let requests = self.requests();
        assert_eq!(requests.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), paths);
        for r in requests {
            let values = |key: &str| r.query.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
            if r.path == ROOT || r.path == CATALOG {
                assert_eq!(values("f"), ["json"]);
            }
            if r.path == CATALOG || r.path == ITEMS {
                assert_eq!(values("key"), ["fixture-key"]);
            }
            if r.path == ITEMS {
                assert_eq!(values("limit"), ["10000"]);
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
