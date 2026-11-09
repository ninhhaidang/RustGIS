//! The identify pop-up links web addresses and shows a picture fetched from one, through the
//! tile cache's downloads (here a fake fetcher, as the browser installs its own, which keeps
//! every request: nothing reaches the network). A picture from RustGIS's asset server is fetched
//! as the pop-up opens; one from anywhere else only once the user clicks Show Picture.

use std::sync::{Arc, Mutex, MutexGuard};

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use rustgis_ui_egui::RustGisApp;
use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache;
use serde_json::{Value, json};

/// The tile cache has one fetcher for the process: these tests take turns with it.
static TURN: Mutex<()> = Mutex::new(());

struct Turn(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for Turn {
    fn drop(&mut self) {
        cache::clear_fetcher();
    }
}

fn turn() -> Turn {
    Turn(TURN.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Downloads asked for, by address; each waits for the test to answer it.
type Asked = Arc<Mutex<Vec<(String, cache::Done)>>>;

/// Installs a fetcher that keeps every download for the test to answer.
fn fake_fetcher() -> Asked {
    let asked: Asked = Arc::default();
    let keep = asked.clone();
    cache::set_fetcher(move |url, done| {
        // Map tiles aren't part of these tests.
        if cache::split_download(&url).1 {
            keep.lock().unwrap().push((url, done));
        } else {
            done(Err("no tiles here".into()));
        }
    });
    asked
}

/// Answers the downloads of addresses containing `part`; how many there were.
fn answer(asked: &Asked, part: &str, with: impl Fn() -> Result<Vec<u8>, String>) -> usize {
    let mut list = asked.lock().unwrap();
    let (these, rest): (Vec<_>, Vec<_>) = list.drain(..).partition(|(u, _)| u.contains(part));
    *list = rest;
    drop(list);
    let n = these.len();
    for (_, done) in these {
        done(with());
    }
    n
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbaImage::from_pixel(w, h, image::Rgba([40, 90, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn webp(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbaImage::from_pixel(w, h, image::Rgba([200, 90, 40, 255]))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::WebP)
        .unwrap();
    out
}

/// The World Atlas with a point at 0,0 whose properties are `props`, and its pop-up open.
fn popup(props: Value) -> egui_kittest::Harness<'static, RustGisApp> {
    let mut app = RustGisApp::with_sample();
    app.session.map_mut().tiles.clear();
    let feature = json!({"type": "Feature", "properties": props, "geometry": {"type": "Point", "coordinates": [0, 0]}});
    app.run(
        "layer.addDataText",
        json!({"fileName": "places.geojson", "name": "Places", "text": json!({"type": "FeatureCollection", "features": [feature]}).to_string()}),
    )
    .unwrap();
    app.run("map.zoomToExtent", json!({"extent": [-5, -4, 5, 4]})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(2);
    h.state_mut().run("ui.identify", json!({"lon": 0, "lat": 0})).unwrap();
    h.run_steps(3);
    h
}

/// The addresses asked for so far, without the download marker.
fn asked_urls(asked: &Asked) -> Vec<String> {
    asked.lock().unwrap().iter().map(|(u, _)| cache::split_download_limit(u).0.to_string()).collect()
}

/// Closes the pop-up and identifies the point again.
fn reopen(h: &mut egui_kittest::Harness<'static, RustGisApp>) {
    h.state_mut().map.popup = None;
    h.run_steps(2);
    h.state_mut().run("ui.identify", json!({"lon": 0, "lat": 0})).unwrap();
    h.run_steps(3);
}

fn has_label(h: &egui_kittest::Harness<'_, RustGisApp>, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

#[test]
fn a_web_address_is_a_link_and_other_text_is_not() {
    let _turn = turn();
    let asked = fake_fetcher();
    let h = popup(json!({
        "name": "Goddard",
        "website": "https://www.nasa.gov/goddard/",
        "note": "see https://www.nasa.gov/ for more",
        "script": "javascript:alert(1)",
        "local": "file:///etc/passwd",
        "inline": "data:text/html,<b>hi</b>",
        "bare": "www.nasa.gov/goddard",
        "page": "HTTP://EXAMPLE.ORG/A"
    }));
    // Links, opened in the browser.
    h.get_by_role_and_label(Role::Link, "https://www.nasa.gov/goddard/");
    h.get_by_role_and_label(Role::Link, "HTTP://EXAMPLE.ORG/A");
    // Text, as before.
    for text in [
        "see https://www.nasa.gov/ for more",
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,<b>hi</b>",
        "www.nasa.gov/goddard",
    ] {
        assert!(h.query_by_role_and_label(Role::Link, text).is_none(), "{text} is a link");
        h.get_by_label(text);
    }
    // No picture field, no picture address: nothing fetched, nothing offered, nothing said.
    assert!(asked.lock().unwrap().is_empty());
    assert!(!has_label(&h, "Picture unavailable"));
    assert!(!has_label(&h, "Loading picture"));
    assert!(h.query_by_label("Show Picture").is_none());
}

#[test]
fn a_link_opens_in_a_new_tab_and_can_be_copied() {
    let _turn = turn();
    let _asked = fake_fetcher();
    let mut h = popup(json!({"name": "Goddard", "website": "https://www.nasa.gov/goddard/"}));
    h.get_by_role_and_label(Role::Link, "https://www.nasa.gov/goddard/").click();
    h.step();
    let opened = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::OpenUrl(o) => Some((o.url.clone(), o.new_tab)),
        _ => None,
    });
    assert_eq!(opened, Some(("https://www.nasa.gov/goddard/".to_string(), true)));
    h.run_steps(2);
    h.get_by_role_and_label(Role::Link, "https://www.nasa.gov/goddard/").click_secondary();
    h.run_steps(2);
    h.get_by_label("Copy Link").click();
    h.step();
    let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(copied.as_deref(), Some("https://www.nasa.gov/goddard/"));
}

#[test]
fn a_picture_from_the_asset_server_shows_by_itself() {
    let _turn = turn();
    let asked = fake_fetcher();
    let url = "https://assets.rustgis.app/images/explore/galileo-earth.webp?size=medium";
    let mut h = popup(json!({
        "name": "Galileo",
        "image_url": url,
        "image_page_url": "https://images.example.org/details/earth",
        "image_credit": "NASA/JPL"
    }));
    // Asked for once, for the picture field only, with the pop-up's byte budget; a spinner
    // meanwhile, and no button.
    h.run_steps(3);
    assert_eq!(asked_urls(&asked), [url]);
    let budget = asked.lock().unwrap().first().map(|(u, _)| cache::split_download_limit(u).1);
    assert_eq!(budget, Some(Some(4 << 20)));
    assert!(has_label(&h, "Loading picture"));
    assert!(h.query_by_label("Show Picture").is_none());
    assert!(h.query_by_label("Picture").is_none());
    assert_eq!(answer(&asked, "galileo-earth.webp", || Ok(webp(64, 48))), 1);
    h.run_steps(3);
    h.get_by_label("Picture");
    assert!(!has_label(&h, "Loading picture"));
    // Its address, the page and the credit stay in the rows; the page isn't fetched.
    h.get_by_role_and_label(Role::Link, url);
    h.get_by_role_and_label(Role::Link, "https://images.example.org/details/earth");
    h.get_by_label("NASA/JPL");
    // Closed and opened again: kept, not fetched again.
    reopen(&mut h);
    h.get_by_label("Picture");
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn a_picture_from_another_server_is_fetched_only_on_click() {
    let _turn = turn();
    let asked = fake_fetcher();
    let url = "https://images.example.org/galileo/earth.jpg";
    let mut h = popup(json!({"name": "Galileo", "photo": url, "other": "https://images.example.org/other.png"}));
    // Offered with the server it would ask, frame after frame, and nothing asked.
    h.run_steps(10);
    assert!(asked.lock().unwrap().is_empty(), "fetched before the click: {:?}", asked_urls(&asked));
    h.get_by_label("from images.example.org");
    assert!(h.query_by_label("Picture").is_none());
    assert!(!has_label(&h, "Loading picture"));
    // Opened again: still only offered.
    reopen(&mut h);
    h.run_steps(5);
    assert!(asked.lock().unwrap().is_empty());
    // Clicked: that one picture is asked for, once.
    h.get_by_label("Show Picture").click();
    h.run_steps(3);
    assert_eq!(asked_urls(&asked), [url]);
    assert!(has_label(&h, "Loading picture"));
    assert!(h.query_by_label("Show Picture").is_none());
    assert_eq!(answer(&asked, "earth.jpg", || Ok(png(40, 30))), 1);
    h.run_steps(3);
    h.get_by_label("Picture");
    h.get_by_role_and_label(Role::Link, url);
    // Kept for the session: opened again, shown without asking or fetching again.
    reopen(&mut h);
    h.get_by_label("Picture");
    assert!(h.query_by_label("Show Picture").is_none());
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn look_alikes_of_the_asset_server_wait_for_a_click() {
    let _turn = turn();
    let asked = fake_fetcher();
    for (address, host) in [
        ("https://assets.rustgis.app.evil.example/a.png", "assets.rustgis.app.evil.example"),
        ("https://xassets.rustgis.app/a.png", "xassets.rustgis.app"),
        ("https://cdn.assets.rustgis.app/a.png", "cdn.assets.rustgis.app"),
        ("https://assets.rustgis.app:8443/a.png", "assets.rustgis.app:8443"),
        ("https://assets.rustgis.app./a.png", "assets.rustgis.app."),
        ("https://evil.example/assets.rustgis.app/a.png", "evil.example"),
    ] {
        let mut h = popup(json!({"name": "Look-alike", "image": address}));
        h.run_steps(5);
        assert!(asked.lock().unwrap().is_empty(), "{address} fetched by itself");
        h.get_by_label("Show Picture");
        h.get_by_label(&format!("from {host}"));
    }
    // A user name before the host isn't a link at all, so there's no picture to show or fetch.
    for address in ["https://assets.rustgis.app@evil.example/a.png", "https://evil.example@assets.rustgis.app/a.png"] {
        let mut h = popup(json!({"name": "Look-alike", "image": address}));
        h.run_steps(5);
        assert!(h.query_by_label("Show Picture").is_none(), "{address}");
        assert!(h.query_by_role_and_label(Role::Link, address).is_none(), "{address}");
        h.get_by_label(address);
    }
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn a_picture_that_cannot_be_fetched_says_why_once() {
    let _turn = turn();
    let asked = fake_fetcher();
    let url = "https://assets.rustgis.app/images/missing.png";
    let mut h = popup(json!({"name": "Lost", "photo": url}));
    h.run_steps(2);
    assert_eq!(answer(&asked, "missing.png", || Err("http status: 404".into())), 1);
    h.run_steps(3);
    h.get_by_label("Picture unavailable: http status: 404");
    // The address stays a link, and isn't fetched again frame after frame.
    h.get_by_role_and_label(Role::Link, url);
    h.run_steps(10);
    assert!(asked.lock().unwrap().is_empty(), "fetched again");
    h.get_by_label("Picture unavailable: http status: 404");
}

#[test]
fn a_picture_too_large_says_so() {
    let _turn = turn();
    let asked = fake_fetcher();
    // More bytes than the budget: the fetcher stops reading and says so (as the native one does).
    let mut h = popup(json!({"name": "Heavy", "photo": "https://assets.rustgis.app/images/heavy.jpg"}));
    h.run_steps(2);
    assert_eq!(answer(&asked, "heavy.jpg", || Err(format!("larger than {} bytes", 4 << 20))), 1);
    h.run_steps(3);
    h.get_by_label("Picture unavailable: larger than 4194304 bytes");
    // More pixels than are decoded: refused from its header, saying how large.
    let mut h = popup(json!({"name": "Wide", "photo": "https://assets.rustgis.app/images/wide.png"}));
    h.run_steps(2);
    assert_eq!(answer(&asked, "wide.png", || Ok(png(3000, 20))), 1);
    h.run_steps(3);
    h.get_by_label("Picture unavailable: 3000 × 20 pixels is larger than 2048 × 2048");
    assert!(h.query_by_label("Picture").is_none());
}

#[test]
fn an_answer_that_is_not_a_picture_says_so() {
    let _turn = turn();
    let asked = fake_fetcher();
    let mut h = popup(json!({"name": "Page", "thumbnail": "https://images.example.org/not-a-picture"}));
    h.run_steps(2);
    h.get_by_label("Show Picture").click();
    h.run_steps(2);
    assert_eq!(answer(&asked, "not-a-picture", || Ok(b"<html>Sign in</html>".to_vec())), 1);
    h.run_steps(3);
    h.get_by_label("Picture unavailable: not a picture");
    assert!(h.query_by_label("Picture").is_none());
}

#[test]
fn a_picture_in_a_format_not_read_here_says_so() {
    let _turn = turn();
    let asked = fake_fetcher();
    let mut h = popup(json!({"name": "Scan", "image": "https://assets.rustgis.app/scan"}));
    h.run_steps(2);
    let mut bmp = b"BM".to_vec();
    bmp.resize(64, 0);
    assert_eq!(answer(&asked, "scan", || Ok(bmp.clone())), 1);
    h.run_steps(3);
    h.get_by_label("Picture unavailable: BMP pictures aren't supported");
    assert!(!has_label(&h, "not a picture"));
}

#[test]
fn a_plain_http_picture_does_not_hide_an_https_one() {
    let _turn = turn();
    let asked = fake_fetcher();
    // `photo` comes before `thumbnail`, but only the thumbnail can be fetched.
    let url = "https://assets.rustgis.app/images/p.jpg";
    let mut h = popup(json!({"name": "Old and new", "photo": "http://old.example/p", "thumbnail": url}));
    h.run_steps(3);
    assert_eq!(asked_urls(&asked), [url]);
    assert!(!has_label(&h, "only https:// pictures are fetched"));
    assert_eq!(answer(&asked, "p.jpg", || Ok(png(20, 10))), 1);
    h.run_steps(3);
    h.get_by_label("Picture");
    // The plain address stays a link.
    h.get_by_role_and_label(Role::Link, "http://old.example/p");
}

#[test]
fn local_and_plain_http_pictures_are_never_fetched() {
    let _turn = turn();
    let asked = fake_fetcher();
    for (address, why) in [
        ("http://assets.rustgis.app/a.jpg", "only https:// pictures are fetched"),
        ("http://images.example.org/a.jpg", "only https:// pictures are fetched"),
        ("https://192.168.1.10/a.jpg", "pictures aren't fetched from this computer or the local network"),
        ("https://localhost/a.jpg", "pictures aren't fetched from this computer or the local network"),
        ("https://169.254.169.254/a.jpg", "that address is never fetched"),
    ] {
        let mut h = popup(json!({"name": "Here", "image": address}));
        h.run_steps(3);
        h.get_by_label(&format!("Picture unavailable: {why}"));
        // Nothing to click: it won't be fetched even when asked.
        assert!(h.query_by_label("Show Picture").is_none(), "{address}");
        h.get_by_role_and_label(Role::Link, address);
    }
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn a_picture_kept_in_a_value_wins_over_one_on_the_web() {
    use base64::Engine;
    let _turn = turn();
    let asked = fake_fetcher();
    for (kind, bytes) in [("png", png(16, 12)), ("webp", webp(16, 12))] {
        let data = format!("data:image/{kind};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
        let mut h = popup(json!({"name": "Both", "thumbnail": data, "image_url": "https://assets.rustgis.app/images/web.jpg"}));
        h.run_steps(4);
        h.get_by_label("Picture");
        assert!(asked.lock().unwrap().is_empty(), "the web picture was fetched beside a {kind} one");
        assert!(!has_label(&h, "Loading picture"));
        assert!(h.query_by_label("Show Picture").is_none());
        h.get_by_role_and_label(Role::Link, "https://assets.rustgis.app/images/web.jpg");
        // The value shows as the picture, not as its text.
        assert!(!has_label(&h, &format!("data:image/{kind}")), "{kind}");
    }
}

#[test]
fn an_address_in_capitals_is_fetched_with_its_scheme_in_lowercase() {
    let _turn = turn();
    let asked = fake_fetcher();
    // The downloads tell a redirect from https down to http by the scheme as written.
    let mut h = popup(json!({"name": "Loud", "image": "HTTPS://ASSETS.RUSTGIS.APP/IMAGES/A.PNG"}));
    h.run_steps(3);
    assert_eq!(asked_urls(&asked), ["https://ASSETS.RUSTGIS.APP/IMAGES/A.PNG"]);
    assert_eq!(answer(&asked, "A.PNG", || Ok(png(8, 8))), 1);
    h.run_steps(3);
    h.get_by_label("Picture");
    h.get_by_role_and_label(Role::Link, "HTTPS://ASSETS.RUSTGIS.APP/IMAGES/A.PNG");
}

#[test]
fn pictures_go_with_an_arcgis_sign_out() {
    let _turn = turn();
    let asked = fake_fetcher();
    let photo = "https://popup1465.example.org/server/rest/services/Sites/FeatureServer/0/1/attachments/2";
    let mut h = popup(json!({"name": "Site", "photo": photo}));
    // Signed in with a token, and the server's picture asked for and shown.
    let root = h.state_mut().run("arcgis.token", json!({"url": photo, "token": "popup1465"})).unwrap()["server"].clone();
    h.run_steps(2);
    h.get_by_label("Show Picture").click();
    h.run_steps(2);
    assert_eq!(answer(&asked, "attachments/2", || Ok(png(8, 8))), 1);
    h.run_steps(3);
    h.get_by_label("Picture");
    // Not signed out: kept.
    reopen(&mut h);
    h.get_by_label("Picture");
    assert!(asked.lock().unwrap().is_empty());
    // Signed out: what was read with the token goes, as the cached tiles do; the picture waits to
    // be asked for again, and nothing is fetched by itself.
    let out = h.state_mut().run("arcgis.signOut", json!({"portal": root})).unwrap();
    assert_eq!(out["signedOut"], 1);
    reopen(&mut h);
    h.run_steps(5);
    assert!(h.query_by_label("Picture").is_none());
    h.get_by_label("Show Picture");
    assert!(asked.lock().unwrap().is_empty(), "{:?}", asked_urls(&asked));
}
