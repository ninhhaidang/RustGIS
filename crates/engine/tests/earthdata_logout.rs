//! Forgetting the Earthdata Login token forgets what was read with it, as signing out of ArcGIS
//! does: the cache keeps answers by address, and an answer read with the token would otherwise
//! still be served after it is gone.
//!
//! Its own test binary: it replaces the process's fetcher and empties the shared cache.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rustgis_engine::render::rustgis_tiles::cache::{self, State, Want};
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};

/// A file on an Earthdata data host, which the token goes to, and a document elsewhere.
const DATA: &str = "https://data.lpdaac.earthdatacloud.nasa.gov/lp-prod-protected/granule/meta.json";
const ELSEWHERE: &str = "https://tiles.example.org/style.json";

/// Asks the cache for `url` until it settles.
fn read(url: &str) -> State {
    let start = Instant::now();
    loop {
        match cache::get(url, Want::Json) {
            State::Pending if start.elapsed() < Duration::from_secs(30) => std::thread::sleep(Duration::from_millis(2)),
            other => return other,
        }
    }
}

#[test]
fn earthdata_logout_forgets_what_was_read_with_the_token() {
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = asked.clone();
    cache::set_fetcher(move |url, done| {
        seen.lock().unwrap_or_else(|e| e.into_inner()).push(url);
        done(Ok(br#"{"granule": "read with the token"}"#.to_vec()))
    });
    let mut s = Session::new(Project::default());
    let fetched = || asked.lock().unwrap_or_else(|e| e.into_inner()).len();

    // A token kept until the logout, and one forgotten on expiry before it (its JWT says it
    // expired in 1970): what either read goes at the logout.
    // An unsigned placeholder with an explicit expiry claim, never a usable credential.
    let expired = format!("unsigned.{}.unsigned", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":0}"#));
    for (round, token) in ["placeholder", expired.as_str()].into_iter().enumerate() {
        let status = s.run("earthdata.login", &json!({"token": token})).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(status["token"], Value::Bool(true));
        // Addresses of the round's own, so nothing read before it answers from the cache.
        let (data, elsewhere) = (format!("{DATA}?round={round}"), format!("{ELSEWHERE}?round={round}"));
        for url in [&data, &elsewhere] {
            assert!(matches!(read(url), State::Ready(_)), "{url}");
            assert!(cache::peek(url).is_some(), "{url}");
        }
        assert_eq!(fetched(), 2 + 3 * round);
        if round == 1 {
            let status = s.run("earthdata.status", &Value::Null).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!((&status["token"], &status["expired"]), (&Value::Bool(false), &Value::Bool(true)));
        }

        let status = s.run("earthdata.logout", &Value::Null).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(status["token"], Value::Bool(false));
        // Gone with the token: asked for again, they are fetched again.
        for url in [&data, &elsewhere] {
            assert!(cache::peek(url).is_none(), "{token}: {url} was kept after the logout");
        }
        assert!(matches!(read(&data), State::Ready(_)));
        assert_eq!(fetched(), 3 + 3 * round);
    }
    cache::clear_fetcher();
}
