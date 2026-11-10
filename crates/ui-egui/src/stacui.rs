//! STAC Search: an API's collections, a search over the map's extent (or anywhere) with dates and
//! a cloud limit, the items found with their footprints, and an asset of one added by its web
//! address. Requests go off the frame through the tile cache's downloads (a thread natively,
//! fetch on the web). A Planetary Computer asset is signed first, with a token fetched the same
//! way. A `?stac=` link (an item's or a collection's address) opens the dialog on it.
//!
//! A static catalog (a `catalog.json` or `collection.json` rather than an API) is browsed: its
//! child catalogs and collections are picked from a list, and Find Items walks the tree from the
//! one picked, several documents at a time, filtering the items found here (see
//! `rustgis_tiles::stac::catalog`). Web Data's services are presets of the dialog ([`Preset`]).

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::{
    cache,
    stac::{
        self, Item,
        catalog::{self, Child, Walk},
    },
};
use serde_json::json;

use crate::{RustGisApp, Slot, i18n};

type Answer = Slot<Result<Vec<u8>, String>>;

/// What is being fetched.
enum Asked {
    /// An API's collections, by the address as typed.
    Collections(String),
    /// A search, by its address (which the answer's links are resolved against).
    Search(String),
    /// A token for an asset that needs signing: the token service's address, the asset's and the
    /// layer's name and saved connection, captured when Add was requested.
    Token { token_url: String, href: String, name: String, connection: Option<String> },
    /// An item's or a collection's document (a `?stac=` link), by its address.
    Link(String),
    /// A static catalog's document, for its children, by its address.
    Children(String),
}

/// Planet's imagery of disasters on Source Cooperative, a static catalog of events.
pub const PLANET: &str = "https://data.source.coop/planet/disasterdata/catalog.json";
/// The Portolan registry: a static catalog whose children are the publishers' catalogs.
pub const PORTOLAN: &str = "https://raw.githubusercontent.com/portolan-sdi/portolan-registry/refs/heads/main/exports/catalogs.json";
/// Vantor's (formerly Maxar's) open data: a static catalog of events.
pub const VANTOR: &str = "https://vantor-opendata.s3.amazonaws.com/events/catalog.json";

/// The STAC APIs and static catalogs the dialog offers by name: (label, address).
pub const APIS: [(&str, &str); 5] = [
    ("Earth Search", stac::EARTH_SEARCH),
    ("Planetary Computer", stac::PLANETARY_COMPUTER),
    ("Planet Open Data", PLANET),
    ("Portolan", PORTOLAN),
    ("Vantor Open Data", VANTOR),
];

/// A Web Data service that is STAC Search opened on one API or catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Planet,
    Portolan,
    Sentinel2,
    Vantor,
    UsgsLidar,
}

impl Preset {
    pub const ALL: [Preset; 5] = [Preset::Planet, Preset::Portolan, Preset::Sentinel2, Preset::Vantor, Preset::UsgsLidar];

    /// Its command's suffix (`ui.stac.planet`…).
    pub fn id(self) -> &'static str {
        match self {
            Preset::Planet => "planet",
            Preset::Portolan => "portolan",
            Preset::Sentinel2 => "sentinel2",
            Preset::Vantor => "vantor",
            Preset::UsgsLidar => "usgsLidar",
        }
    }

    pub fn from_id(id: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(id))
    }

    /// The dialog's title.
    pub fn title(self) -> &'static str {
        match self {
            Preset::Planet => "Planet Open Data",
            Preset::Portolan => "Portolan",
            Preset::Sentinel2 => "Sentinel-2 Explorer",
            Preset::Vantor => "Vantor Open Data",
            Preset::UsgsLidar => "USGS LiDAR",
        }
    }

    /// What the dialog says about the service.
    fn hint(self) -> &'static str {
        match self {
            Preset::Planet => {
                "Planet's imagery of disasters (CC BY-NC), a static STAC catalog on Source Cooperative: pick an event, then Find Items."
            }
            Preset::Portolan => {
                "Open data catalogs registered with Portolan: pick a publisher's catalog, then Find Items for its collections."
            }
            Preset::Sentinel2 => {
                "Sentinel-2 scenes (Collection 1, Level-2A) from Earth Search, clearest first: add the true colour image (visual) or a band."
            }
            Preset::Vantor => "Vantor's open imagery of disasters: pick an event and a phase (before or after it), then Find Items.",
            Preset::UsgsLidar => {
                "USGS 3DEP LiDAR point clouds (COPC) from the Planetary Computer, where the map is: add a tile's data as a point cloud."
            }
        }
    }

    /// Its API's or catalog's address.
    fn address(self) -> &'static str {
        match self {
            Preset::Planet => PLANET,
            Preset::Portolan => PORTOLAN,
            Preset::Vantor => VANTOR,
            Preset::Sentinel2 => stac::EARTH_SEARCH,
            Preset::UsgsLidar => stac::PLANETARY_COMPUTER,
        }
    }
}

/// The dialog's choices and results.
pub struct State {
    /// Add as Time Series (#559): the band to follow the time slider with, and its range as typed.
    pub cube_asset: String,
    pub cube_range: String,
    pub api: String,
    /// The saved connection this dialog was opened on, by id (see `connectionui::picked`).
    pub connection: Option<String>,
    /// The API or catalog address whose collections or children last landed (what Save as Connection keeps).
    pub listed: String,
    /// Collections the API offers: id and title.
    pub collections: Vec<(String, String)>,
    pub collection: String,
    pub from: String,
    pub to: String,
    pub use_view: bool,
    pub max_cloud: String,
    /// The order of an API's items (`eo:cloud_cover`…); empty for the API's own.
    pub sortby: String,
    /// The Web Data service the dialog was opened as.
    pub preset: Option<Preset>,
    /// A static catalog: the node Find Items walks from (empty for the catalog itself), its
    /// title, the nodes above it (title, address) and its children.
    pub node: String,
    pub node_title: String,
    pub trail: Vec<(String, String)>,
    pub children: Vec<Child>,
    /// `pre`, `post` or empty: an event's imagery before or after it.
    pub phase: String,
    /// A static catalog's walk, and the documents being fetched for it.
    walk: Option<(Walk, Vec<(String, Answer)>)>,
    pub items: Vec<Item>,
    /// The area the search whose items are listed asked for (none: anywhere, or items from a
    /// link), and the one the search under way asks for, which takes its place when its items land.
    searched_area: Option<[f64; 4]>,
    asked_area: Option<[f64; 4]>,
    /// The asset chosen for each item.
    pub chosen: Vec<String>,
    /// The item whose properties are shown.
    pub details: Option<usize>,
    /// Whether `info` is a search's count (kept when a catalog's children land after it).
    counted: bool,
    /// Items' preview pictures, by address.
    thumbs: std::collections::HashMap<String, Thumb>,
    /// The footprints layer of the last search.
    footprints: Option<u32>,
    waiting: Option<(Asked, Answer)>,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            cube_asset: String::new(),
            cube_range: String::new(),
            api: stac::EARTH_SEARCH.to_string(),
            connection: None,
            listed: String::new(),
            collections: Vec::new(),
            collection: "sentinel-2-c1-l2a".into(),
            from: String::new(),
            to: String::new(),
            use_view: true,
            max_cloud: "20".into(),
            sortby: String::new(),
            preset: None,
            node: String::new(),
            node_title: String::new(),
            trail: Vec::new(),
            children: Vec::new(),
            phase: String::new(),
            walk: None,
            items: Vec::new(),
            searched_area: None,
            asked_area: None,
            chosen: Vec::new(),
            details: None,
            counted: false,
            thumbs: Default::default(),
            footprints: None,
            waiting: None,
            error: None,
            info: None,
        }
    }
}

fn fetch(url: &str) -> Answer {
    let slot: Answer = Slot::default();
    let land = slot.clone();
    cache::download(url, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
    slot
}

/// Reads with the selected API's login only; a catalog's foreign links stay anonymous.
fn fetch_service(app: &mut RustGisApp, url: &str) -> Answer {
    let connection = selected_connection(app);
    let base = app.ui.stac.api.clone();
    match crate::serviceui::request_from(app, connection.as_deref(), &base, url) {
        Ok(url) => fetch(&url),
        Err(e) => Slot::new(std::sync::Mutex::new(Some(Err(crate::connectionui::translated(&e))))),
    }
}

/// An item's preview picture: being fetched, shown, or not to be had.
enum Thumb {
    Loading(Answer),
    Ready(egui::TextureHandle),
    Failed,
}

/// The most preview pictures kept (more are dropped and fetched again when shown).
const MAX_THUMBS: usize = 256;
/// The longest side of a preview picture, in pixels.
const THUMB_SIDE: u32 = 128;
/// The most bytes a preview picture may have.
const THUMB_BYTES: usize = 2 << 20;
/// The most preview pictures decoded in one frame (the rest wait for the next).
const THUMBS_PER_FRAME: usize = 4;

/// Lands preview pictures: decoded (bounded in size) and made textures.
fn poll_thumbs(app: &mut RustGisApp, ctx: &egui::Context) {
    let mut loading = false;
    let mut decoded = 0;
    for (url, t) in app.ui.stac.thumbs.iter_mut() {
        let Thumb::Loading(slot) = t else { continue };
        if decoded >= THUMBS_PER_FRAME {
            loading = true;
            break;
        }
        let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            loading = true;
            continue;
        };
        decoded += 1;
        *t = match r.and_then(|b| crate::mapview::decode_picture(&b)) {
            Ok(img) => {
                let (w, h) = (img.width().max(1), img.height().max(1));
                let k = (THUMB_SIDE as f32 / w.max(h) as f32).min(1.0);
                let (tw, th) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
                let small = image::imageops::thumbnail(&img, tw, th);
                let color = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], small.as_raw());
                Thumb::Ready(ctx.load_texture(format!("stac_thumb:{url}"), color, egui::TextureOptions::LINEAR))
            }
            Err(_) => Thumb::Failed,
        };
    }
    if loading {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
}

/// Starts fetching the preview pictures of the items found (those on the web an item may point
/// to, not ones that need signing).
fn want_thumbs(app: &mut RustGisApp) {
    let connection = selected_connection(app);
    let base = app.ui.stac.api.clone();
    let (session, st) = (&mut app.session, &mut app.ui.stac);
    if st.thumbs.len() > MAX_THUMBS {
        st.thumbs.clear();
    }
    for item in st.items.iter().take(MAX_THUMBS / 2) {
        let Some(a) = item.thumbnail() else { continue };
        if st.thumbs.contains_key(&a.href) {
            continue;
        }
        let ok = !stac::needs_signing(&a.href)
            && rustgis_engine::render::rustgis_tiles::address::check(
                &a.href,
                Some(item.origin.as_deref().unwrap_or("https://item.invalid/")),
            )
            .is_ok();
        let address = if ok {
            use rustgis_engine::render::rustgis_tiles::logins;
            if logins::origin(&base).is_some_and(|origin| logins::origin(&a.href).as_deref() == Some(origin.as_str())) {
                rustgis_engine::service_logins::request(session, connection.as_deref(), &a.href).ok()
            } else {
                Some(logins::inner(&a.href).to_string())
            }
        } else {
            None
        };
        let picture = address.map(|address| {
            let slot: Answer = Slot::default();
            let land = slot.clone();
            cache::download_limited(&address, THUMB_BYTES, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
            Thumb::Loading(slot)
        });
        st.thumbs.insert(a.href.clone(), picture.unwrap_or(Thumb::Failed));
    }
}

/// Lands collections and search results.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    poll_thumbs(app, ctx);
    poll_walk(app, ctx);
    let Some((_, slot)) = &app.ui.stac.waiting else { return };
    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some((asked, _)) = app.ui.stac.waiting.take() else { return };
    let st = &mut app.ui.stac;
    st.error = None;
    let previous = st.info.take();
    match asked {
        Asked::Collections(api) => match r.and_then(|b| stac::parse_collections(&b)) {
            Ok(c) => {
                st.info = Some(i18n::fmt("{n} collections", &[("n", &c.len().to_string())]));
                st.collections = c.into_iter().map(|c| (c.id, c.title)).collect();
                st.listed = api;
            }
            Err(e) => st.error = Some(e),
        },
        Asked::Search(url) => match r.and_then(|b| Ok((stac::parse_items(&b, &url)?, stac::parse_matched(&b)))) {
            Ok(((items, next), matched)) => {
                let n = items.len().to_string();
                st.info = Some(match matched.filter(|m| *m as usize > items.len()) {
                    Some(m) => i18n::fmt("Showing {n} of {m} items (narrow the search for others)", &[("n", &n), ("m", &m.to_string())]),
                    None if next.is_some() => i18n::fmt("Showing {n} items (more match: narrow the search)", &[("n", &n)]),
                    None => i18n::fmt("Showing {n} items", &[("n", &n)]),
                });
                st.details = None;
                st.counted = true;
                st.chosen = items.iter().map(|i| i.pick(None).map(|a| a.key.clone()).unwrap_or_default()).collect();
                st.items = items;
                st.searched_area = st.asked_area;
                show_footprints(app);
            }
            Err(e) => st.error = Some(e),
        },
        Asked::Token { token_url, href, name, connection } => match r.and_then(|b| stac::parse_sas(&b)) {
            Ok(token) => {
                stac::remember_token(&token_url, &token);
                add_url_from(app, &stac::sign(&href, &token), &name, connection.as_deref());
            }
            Err(e) => st.error = Some(e),
        },
        Asked::Link(url) => landed_link(app, &url, r),
        Asked::Children(url) => match r.and_then(|b| catalog::parse_children(&b, &url)) {
            // A search's count (or a walk's progress), landed first, is kept.
            Ok(c) if st.walk.is_some() || (st.counted && !st.items.is_empty()) => {
                st.info = previous;
                st.children = c;
                if url == st.api.trim() {
                    st.listed = url.clone();
                }
            }
            Ok(c) => {
                st.info = Some(if c.is_empty() {
                    tl!("No catalogs below this one: Find Items lists its items").to_string()
                } else {
                    i18n::fmt("{n} catalogs and collections", &[("n", &c.len().to_string())])
                });
                st.children = c;
                if url == st.api.trim() {
                    st.listed = url.clone();
                }
            }
            Err(e) => {
                st.children.clear();
                st.error = Some(e);
            }
        },
    }
}

/// Drives a static catalog's walk: lands the documents fetched, asks for the next ones (a few at
/// a time) and, once it is over, shows the items found.
fn poll_walk(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some((mut walk, mut fetching)) = app.ui.stac.walk.take() else { return };
    let mut i = 0;
    while let Some((url, slot)) = fetching.get(i) {
        let landed = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        match landed {
            Some(r) => {
                walk.land(url, r);
                fetching.swap_remove(i);
            }
            None => i += 1,
        }
    }
    while fetching.len() < catalog::AT_ONCE {
        let Some(url) = walk.next_url() else { break };
        let slot = fetch_service(app, &url);
        fetching.push((url, slot));
    }
    if !walk.finished() {
        app.ui.stac.info = Some(i18n::fmt(
            "Reading the catalog: {n} documents read, {m} items found…",
            &[("n", &walk.reads.to_string()), ("m", &walk.items().len().to_string())],
        ));
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        app.ui.stac.walk = Some((walk, fetching));
        return;
    }
    let st = &mut app.ui.stac;
    let n = walk.items().len().to_string();
    st.info = Some(if walk.complete() {
        i18n::fmt("Showing {n} items", &[("n", &n)])
    } else if walk.reads >= catalog::MAX_READS {
        i18n::fmt(
            "Showing {n} items (stopped after reading {r} documents: pick a catalog further down)",
            &[("n", &n), ("r", &walk.reads.to_string())],
        )
    } else if walk.failed > 0 && walk.items().len() < 50 {
        i18n::fmt("Showing {n} items ({f} documents couldn't be read)", &[("n", &n), ("f", &walk.failed.to_string())])
    } else if walk.cut && walk.items().len() < 50 {
        i18n::fmt("Showing {n} items (some catalogs were too deep or too large to read: pick a catalog further down)", &[("n", &n)])
    } else {
        i18n::fmt("Showing {n} items (more match: narrow the search)", &[("n", &n)])
    });
    st.details = None;
    st.counted = true;
    let items = walk.into_items();
    st.chosen = items.iter().map(|i| i.pick(None).map(|a| a.key.clone()).unwrap_or_default()).collect();
    st.items = items;
    st.searched_area = st.asked_area;
    show_footprints(app);
}

/// Opens the dialog as a Web Data service (`ui.stac.<preset>`; `ui.stac` alone is STAC Search),
/// set up for it: a static catalog's children are fetched. `{"catalog": address}` browses to a
/// catalog or collection below it, `{"phase": "pre" | "post"}` sets the phase and
/// `{"search": true}` finds items at once.
pub fn open_preset(app: &mut RustGisApp, preset: Option<Preset>, p: &serde_json::Value) -> Result<(), String> {
    app.run("ui.dialog", serde_json::json!({"name": "stac"}))?;
    if let Some(preset) = preset
        && shown_preset(&app.ui.stac) != Some(preset)
    {
        // A fresh dialog, keeping the last footprints (removed when the next ones come).
        let footprints = app.ui.stac.footprints.take();
        app.ui.stac = State { footprints, preset: Some(preset), api: preset.address().to_string(), ..State::default() };
        let st = &mut app.ui.stac;
        match preset {
            Preset::Planet | Preset::Portolan | Preset::Vantor => {
                (st.collection, st.max_cloud, st.use_view) = (String::new(), String::new(), false);
            }
            Preset::Sentinel2 => {
                st.collection = "sentinel-2-c1-l2a".into();
                st.sortby = "eo:cloud_cover".into();
                st.from = days_ago(90);
            }
            Preset::UsgsLidar => {
                st.collection = "3dep-lidar-copc".into();
                st.max_cloud = String::new();
            }
        }
        if catalog::is_static(&app.ui.stac.api) {
            browse(app, None);
        }
    } else if preset.is_none() {
        app.ui.stac.preset = None;
    }
    // A catalog or collection below the static catalog's top, by its address.
    if let Some(url) = p.get("catalog").and_then(serde_json::Value::as_str).map(str::trim).filter(|u| !u.is_empty()) {
        rustgis_engine::render::rustgis_tiles::address::check(url, None)?;
        if !catalog::is_static(&app.ui.stac.api) {
            return Err("catalog is a static STAC catalog's catalog.json or collection.json".into());
        }
        browse(app, Some((catalog::label_of(url), url.to_string())));
    }
    if let Some(phase) = p.get("phase").and_then(serde_json::Value::as_str) {
        app.ui.stac.phase = match phase.trim().to_ascii_lowercase().as_str() {
            "pre" => "pre".into(),
            "post" => "post".into(),
            _ => String::new(),
        };
    }
    if p.get("search").and_then(serde_json::Value::as_bool) == Some(true) {
        find_items(app);
    }
    Ok(())
}

/// The day `n` days before today (UTC), as `YYYY-MM-DD`.
fn days_ago(n: u64) -> String {
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (now / 86_400).saturating_sub(n) as i64;
    // Days to a civil date (proleptic Gregorian), after Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Adds an asset by its address, signing it first when it needs it (a token in hand is used; one
/// still to fetch is fetched off the frame, and the asset added when it lands).
fn add_signed(app: &mut RustGisApp, href: &str, name: &str) {
    if !stac::needs_signing(href) {
        return add_url(app, href, name);
    }
    match stac::sas_token_url(href) {
        Ok(token_url) => match stac::cached_token(&token_url) {
            Some(token) => add_url(app, &stac::sign(href, &token), name),
            None => {
                let slot = fetch(&token_url);
                app.ui.stac.info = Some(tl!("Signing the asset (Planetary Computer)…").to_string());
                let connection = selected_connection(app);
                app.ui.stac.waiting = Some((Asked::Token { token_url, href: href.to_string(), name: name.to_string(), connection }, slot));
            }
        },
        Err(e) => app.ui.stac.error = Some(e),
    }
}

fn add_url(app: &mut RustGisApp, href: &str, name: &str) {
    let connection = selected_connection(app);
    add_url_from(app, href, name, connection.as_deref());
}

fn selected_connection(app: &RustGisApp) -> Option<String> {
    crate::connectionui::picked(&app.session.connections.connections, app.ui.stac.connection.as_deref(), &app.ui.stac.api)
        .map(|c| c.id.clone())
}

fn add_url_from(app: &mut RustGisApp, href: &str, name: &str, connection: Option<&str>) {
    let mut p = json!({"url": href, "name": name, "zoom": true});
    if let Some(connection) = connection {
        p["connection"] = json!(connection);
    }
    if let Err(e) = app.run("ui.addUrl", p) {
        app.ui.stac.error = Some(e);
    }
}

/// Adds an item's asset (the one named, else the default) under the item's name.
fn add_item_asset(app: &mut RustGisApp, item: &Item, key: Option<&str>) {
    match item.asset_href(key) {
        Ok((key, href)) => add_signed(app, &href, &format!("{} {key}", item.id)),
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// Opens the dialog on an item's or a collection's address (a `?stac=` link), fetching it off
/// the frame.
pub fn open_link(app: &mut RustGisApp, url: &str) -> Result<(), String> {
    let url = url.trim();
    rustgis_engine::render::rustgis_tiles::address::check(url, None)?;
    app.run("ui.dialog", serde_json::json!({"name": "stac"}))?;
    let address = rustgis_engine::service_logins::request(&mut app.session, None, url)?;
    let slot = fetch(&address);
    app.ui.stac.error = None;
    app.ui.stac.info = Some(i18n::fmt("Opening {url}…", &[("url", url)]));
    app.ui.stac.waiting = Some((Asked::Link(url.to_string()), slot));
    Ok(())
}

/// A linked document: an item is shown and its default asset added; a collection is chosen in the
/// dialog, with its API, ready to search.
fn landed_link(app: &mut RustGisApp, url: &str, r: Result<Vec<u8>, String>) {
    let bytes = match r {
        Ok(b) => b,
        Err(e) => {
            app.ui.stac.error = Some(e);
            return;
        }
    };
    let kind = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| Some((v.get("type")?.as_str()?.to_string(), v.get("id").and_then(|i| i.as_str()).map(str::to_string))));
    // The API: the address before `/collections/`, else the document's root (or parent) link.
    let api = stac::api_of(&bytes, url);
    // Without one, the dialog keeps its API, and says which it is.
    let unknown =
        |app: &RustGisApp| i18n::fmt("The link doesn't say its STAC API: Find Items searches {api}", &[("api", &app.ui.stac.api)]);
    match kind {
        Some((t, _)) if t == "Feature" => match stac::parse_item(&bytes, url) {
            Ok(mut item) => {
                item.url.get_or_insert_with(|| url.to_string());
                app.ui.stac.collection = item.collection.clone();
                app.ui.stac.chosen = vec![item.pick(None).map(|a| a.key.clone()).unwrap_or_default()];
                app.ui.stac.items = vec![item.clone()];
                app.ui.stac.searched_area = None;
                let found = api.is_some();
                if let Some(api) = api {
                    app.ui.stac.api = api;
                }
                app.ui.stac.info = Some(i18n::fmt("Item {id}", &[("id", &item.id)]));
                if !found {
                    app.ui.stac.info = Some(format!("{} · {}", i18n::fmt("Item {id}", &[("id", &item.id)]), unknown(app)));
                }
                show_footprints(app);
                add_item_asset(app, &item, None);
            }
            Err(e) => app.ui.stac.error = Some(e),
        },
        Some((t, Some(id))) if t == "Collection" => {
            let found = api.is_some();
            if let Some(api) = api {
                app.ui.stac.api = api;
            }
            app.ui.stac.collection = id.chars().take(200).collect();
            app.ui.stac.info = Some(i18n::fmt("Collection {id}: choose dates and Find Items", &[("id", &app.ui.stac.collection)]));
            if !found {
                app.ui.stac.info = Some(format!("{} · {}", app.ui.stac.info.clone().unwrap_or_default(), unknown(app)));
            }
        }
        _ => {
            app.ui.stac.error =
                Some(i18n::fmt("{url}: not a STAC item or collection", &[("url", &rustgis_engine::sources::address_in_message(url))]))
        }
    }
}

/// Replaces the last search's footprints with this one's, and frames them.
fn show_footprints(app: &mut RustGisApp) {
    if let Some(old) = app.ui.stac.footprints.take()
        && app.session.map().layer(old).is_some()
    {
        let _ = app.run("layer.remove", json!({"layer": old}));
    }
    let fc = rustgis_engine::render::rustgis_tiles::stac::footprints_geojson(&app.ui.stac.items);
    let added = if fc["features"].as_array().is_none_or(Vec::is_empty) {
        Ok(None)
    } else {
        app.run("stac.addFootprints", json!({"footprints": fc, "name": "STAC footprints"}))
            .map(|r| r["layer"].as_u64().and_then(|i| u32::try_from(i).ok()))
    };
    match added {
        Ok(Some(id)) => {
            app.ui.stac.footprints = Some(id);
            if let Some(e) = app.session.map().layer(id).map(|l| l.extent()) {
                app.session.zoom_to(&e);
            }
        }
        Ok(None) => {}
        Err(e) => app.ui.stac.error = Some(e),
    }
    app.map.invalidate();
}

/// Moves a static catalog's browsing to a node (`None`: the catalog itself) and fetches its
/// children.
fn browse(app: &mut RustGisApp, to: Option<(String, String)>) {
    let st = &mut app.ui.stac;
    // A walk of the last node is dropped: its items aren't this one's.
    st.walk = None;
    match to {
        Some((title, url)) => {
            let above = std::mem::replace(&mut st.node_title, title);
            let above_url = std::mem::replace(&mut st.node, url);
            st.trail.push((above, above_url));
        }
        None => (st.node, st.node_title, st.trail) = (String::new(), String::new(), Vec::new()),
    }
    fetch_children(app);
}

/// Goes back up a static catalog to the node at `depth` in the trail.
fn browse_up(app: &mut RustGisApp, depth: usize) {
    let st = &mut app.ui.stac;
    st.walk = None;
    let Some((title, url)) = st.trail.get(depth).cloned() else { return };
    st.trail.truncate(depth);
    (st.node_title, st.node) = (title, url);
    fetch_children(app);
}

/// Fetches the children of the static catalog's node (or of the catalog itself).
fn fetch_children(app: &mut RustGisApp) {
    let st = &mut app.ui.stac;
    st.children.clear();
    st.error = None;
    let url = if st.node.is_empty() { st.api.trim().to_string() } else { st.node.clone() };
    if let Err(e) = rustgis_engine::render::rustgis_tiles::address::check(&url, None) {
        st.error = Some(e);
        return;
    }
    st.info = Some(tl!("Reading the catalog…").to_string());
    st.counted = false;
    let slot = fetch_service(app, &url);
    app.ui.stac.waiting = Some((Asked::Children(url), slot));
}

/// The preset the dialog is showing, if it is still on its API or catalog.
fn shown_preset(st: &State) -> Option<Preset> {
    st.preset.filter(|p| st.api.trim().trim_end_matches('/') == p.address())
}

/// The dialog's title: the Web Data service's, or STAC Search.
pub fn title(app: &RustGisApp) -> &'static str {
    shown_preset(&app.ui.stac).map(Preset::title).unwrap_or("STAC Search")
}

/// Points the panel at another API, dropping what the last one listed and what is still on its
/// way from it (a catalog walk among them), wherever the switch comes from.
pub(crate) fn switch_api(st: &mut State, url: &str) {
    st.api = url.to_string();
    drop_listing(st);
}

/// Forgets what was listed for the last API or catalog, and what is still on its way from it: a
/// late answer would otherwise land under the new one. An asset being signed or a link being
/// opened was asked for already and goes on.
fn drop_listing(st: &mut State) {
    st.collections.clear();
    st.items.clear();
    st.collection.clear();
    (st.node, st.node_title) = (String::new(), String::new());
    st.trail.clear();
    st.children.clear();
    // What Save as Connection would keep: listed again before it is offered.
    st.listed.clear();
    if st.waiting.as_ref().is_some_and(|(a, _)| matches!(a, Asked::Collections(_) | Asked::Search(_) | Asked::Children(_))) {
        st.waiting = None;
    }
    st.walk = None;
    // Previews are read through the last API's login: another one shows its own, read afresh.
    st.thumbs.clear();
    // The last listing's status ("Showing 10 items") isn't the new API's; an asset being signed
    // or a link being opened keeps its own.
    if st.waiting.is_none() {
        st.info = None;
    }
}

/// The dialog.
pub fn dialog(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let busy = app.ui.stac.waiting.is_some() || app.ui.stac.walk.is_some();
    let is_static = catalog::is_static(&app.ui.stac.api);
    let mut list = false;
    let mut search = false;
    let mut go_to: Option<(String, String)> = None;
    let mut go_up: Option<usize> = None;
    let mut api_changed = false;
    if let Some(p) = shown_preset(&app.ui.stac) {
        ui.label(egui::RichText::new(tl!(p.hint())).weak());
    }
    let connections = crate::sourcespane::connections_snapshot(app);
    let current = crate::connectionui::picked(&connections, app.ui.stac.connection.as_deref(), &app.ui.stac.api);
    egui::Grid::new("stac").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let st = &mut app.ui.stac;
        ui.label(if is_static { tl!("STAC catalog") } else { tl!("STAC API") });
        ui.horizontal(|ui| {
            let named = APIS.iter().find(|(_, u)| *u == st.api.trim_end_matches('/')).map(|(l, _)| tl!(*l).to_string());
            egui::ComboBox::from_id_salt("stac_api")
                .selected_text(current.map(|c| c.name.clone()).or(named).unwrap_or_else(|| tl!("Other").to_string()))
                .width((ui.available_width() * 0.4).clamp(90.0, 150.0))
                .show_ui(ui, |ui| {
                    for (label, url) in APIS {
                        if ui.selectable_label(st.api == url, tl!(label)).clicked() && st.api != url {
                            switch_api(st, url);
                            api_changed = true;
                        }
                    }
                    if connections.iter().any(|c| c.kind == "stac") {
                        ui.separator();
                    }
                    for c in connections.iter().filter(|c| c.kind == "stac") {
                        let selected = current.is_some_and(|x| x.id == c.id);
                        if ui.selectable_label(selected, &c.name).on_hover_text(&c.address).clicked() && !selected {
                            switch_api(st, &c.address);
                            st.connection = Some(c.id.clone());
                            api_changed = true;
                        }
                    }
                })
                .response
                .on_hover_text(tl!("Catalog"));
            // A catalog typed in is browsed with Browse, from its top.
            if ui.add(egui::TextEdit::singleline(&mut st.api).desired_width(ui.available_width().max(80.0))).changed() {
                drop_listing(st);
            }
        });
        ui.end_row();
        if is_static {
            ui.label(tl!("Catalog"));
            ui.horizontal(|ui| {
                let shown = if st.node.is_empty() { tl!("all of it").to_string() } else { st.node_title.clone() };
                // Its text is cut to the room left beside Browse.
                let w = (ui.available_width() - 110.0).max(120.0);
                ui.allocate_ui(vec2(w, 30.0), |ui| {
                    egui::ComboBox::from_id_salt("stac_node")
                        .truncate()
                        .selected_text(shown)
                        .width(w)
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            for (i, (title, _)) in st.trail.iter().enumerate() {
                                let title = if title.is_empty() { tl!("all of it").to_string() } else { title.clone() };
                                if ui.selectable_label(false, format!("↑ {title}")).clicked() {
                                    go_up = Some(i);
                                }
                            }
                            for c in &st.children {
                                if ui.selectable_label(false, &c.title).on_hover_text(&c.url).clicked() {
                                    go_to = Some((c.title.clone(), c.url.clone()));
                                }
                            }
                        })
                        .response
                        .on_hover_text(tl!("Catalog"));
                });
                list = ui.add_enabled(!busy, egui::Button::new(tl!("Browse"))).clicked();
            });
            ui.end_row();
        } else {
            ui.label(tl!("Collection"));
            ui.horizontal(|ui| {
                if st.collections.is_empty() {
                    ui.add(
                        egui::TextEdit::singleline(&mut st.collection)
                            .hint_text(tl!("any"))
                            .desired_width((ui.available_width() - 110.0).max(100.0)),
                    );
                } else {
                    let shown = if st.collection.is_empty() { tl!("any").to_string() } else { st.collection.clone() };
                    let w = (ui.available_width() - 110.0).max(100.0);
                    ui.allocate_ui(vec2(w, 30.0), |ui| {
                        egui::ComboBox::from_id_salt("stac_collection")
                            .truncate()
                            .selected_text(shown)
                            .width(w)
                            .height(320.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut st.collection, String::new(), tl!("any"));
                                for (id, title) in &st.collections {
                                    let label = if title.is_empty() { id.clone() } else { format!("{id} — {title}") };
                                    ui.selectable_value(&mut st.collection, id.clone(), label);
                                }
                            })
                            .response
                            .on_hover_text(tl!("Collection"));
                    });
                }
                list = ui.add_enabled(!busy, egui::Button::new(tl!("Collections"))).clicked();
            });
            ui.end_row();
        }
        ui.label(tl!("Dates"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut st.from)
                    .hint_text("2024-06-01")
                    .desired_width(((ui.available_width() - 30.0) / 2.0).clamp(70.0, 110.0)),
            );
            let input_label = ui.label("–");
            ui.add(egui::TextEdit::singleline(&mut st.to).hint_text("2024-06-30").desired_width(ui.available_width().clamp(70.0, 110.0)))
                .labelled_by(input_label.id);
        });
        ui.end_row();
        let input_label = ui.label(tl!("Most cloud cover (%)"));
        ui.add(egui::TextEdit::singleline(&mut st.max_cloud).hint_text(tl!("any")).desired_width(ui.available_width().clamp(70.0, 110.0)))
            .labelled_by(input_label.id);
        ui.end_row();
        if is_static && (st.preset == Some(Preset::Vantor) || !st.phase.is_empty()) {
            ui.label(tl!("Phase"));
            ui.horizontal(|ui| {
                for (v, label) in [("", "Any"), ("pre", "Before the event"), ("post", "After the event")] {
                    ui.radio_value(&mut st.phase, v.to_string(), tl!(label));
                }
            });
            ui.end_row();
        }
        ui.label("");
        ui.checkbox(&mut st.use_view, tl!("Only where the map is"));
        ui.end_row();
    });
    let st = &app.ui.stac;
    let keepable = !st.listed.is_empty()
        && st.listed == st.api.trim()
        && crate::connectionui::picked(&connections, st.connection.as_deref(), &st.listed).is_none();
    let mut keep = false;
    ui.horizontal(|ui| {
        search = ui.add_enabled(!busy, egui::Button::new(tl!("Find Items"))).clicked();
        if busy {
            ui.spinner();
        }
        if keepable {
            keep = crate::connectionui::keep_button(ui);
        }
    });
    // A listing dropped by the switch no longer holds the new catalog's browsing up.
    let busy_now = app.ui.stac.waiting.is_some() || app.ui.stac.walk.is_some();
    if api_changed && catalog::is_static(&app.ui.stac.api) && !busy_now {
        browse(app, None);
    } else if let Some(to) = go_to {
        browse(app, Some(to));
    } else if let Some(i) = go_up {
        browse_up(app, i);
    } else if list && is_static {
        fetch_children(app);
    } else if list {
        match stac::api(Some(app.ui.stac.api.as_str())) {
            Ok(api) => {
                let typed = app.ui.stac.api.trim().to_string();
                let slot = fetch_service(app, &stac::collections_url(&api));
                app.ui.stac.waiting = Some((Asked::Collections(typed), slot));
            }
            Err(e) => app.ui.stac.error = Some(e),
        }
    }
    if search {
        find_items(app);
    }
    if keep {
        match crate::connectionui::keep(app, json!({"kind": "stac", "address": app.ui.stac.listed.clone()})) {
            Ok(id) => app.ui.stac.connection = Some(id),
            Err(e) => app.ui.stac.error = Some(e),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    cube_row(app, ui, busy);
    let t = crate::theme::Tokens::get(ui.ctx());
    if let Some(e) = &app.ui.stac.error {
        ui.label(egui::RichText::new(e).color(t.red));
    }
    if let Some(i) = &app.ui.stac.info {
        ui.label(egui::RichText::new(i).weak());
    }
    results(app, ui);
}

/// Add as Time Series (opengeos/RustGIS#559): when an API's items span several dates, a band of
/// them as a data cube on the time slider (`stac.addCube`), with an optional fixed range.
#[cfg(not(target_arch = "wasm32"))]
fn cube_row(app: &mut RustGisApp, ui: &mut egui::Ui, busy: bool) {
    let st = &app.ui.stac;
    if catalog::is_static(&st.api) || st.collection.trim().is_empty() {
        return;
    }
    let mut days: Vec<&str> = st.items.iter().filter_map(|i| i.datetime.get(..10)).collect();
    days.sort_unstable();
    days.dedup();
    if days.len() < 2 {
        return;
    }
    let (first, last) = (days.first().map(|d| d.to_string()).unwrap_or_default(), days.last().map(|d| d.to_string()).unwrap_or_default());
    // The GeoTIFF bands any item has, in the order first met.
    let mut keys: Vec<String> = Vec::new();
    for a in st.items.iter().flat_map(|i| &i.assets).filter(|a| a.media_type.contains("tiff")) {
        if !keys.contains(&a.key) {
            keys.push(a.key.clone());
        }
    }
    if keys.is_empty() {
        return;
    }
    let connection = selected_connection(app);
    let st = &mut app.ui.stac;
    if !keys.contains(&st.cube_asset) {
        st.cube_asset = keys.first().cloned().unwrap_or_default();
    }
    let mut add = false;
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("stac_cube_asset")
            .selected_text(st.cube_asset.as_str())
            .width(110.0)
            .show_ui(ui, |ui| {
                for k in &keys {
                    ui.selectable_value(&mut st.cube_asset, k.clone(), k.as_str());
                }
            })
            .response
            .on_hover_text(tl!("Asset"));
        ui.add(egui::TextEdit::singleline(&mut st.cube_range).hint_text(tl!("range: 0, 0.9")).desired_width(90.0)).on_hover_text(tl!(
            "The values shown from black to white, the same for every date and tile (the band's own units). Empty: each file's own stretch."
        ));
        add = ui
            .add_enabled(!busy, egui::Button::new(tl!("Add as Time Series")))
            .on_hover_text(tl!("A tile layer per grid tile, each following the time slider through its dates."))
            .clicked();
    });
    if !add {
        return;
    }
    let p = match cube_params(st, first, last, connection) {
        Ok(p) => p,
        Err(e) => {
            st.error = Some(e);
            return;
        }
    };
    match app.run("stac.addCube", p) {
        Ok(r) => {
            app.ui.stac.error = None;
            app.ui.stac.info = Some(i18n::fmt(
                "{layers} tiles, {dates} dates: the time slider steps through them",
                &[("layers", &r["layers"].to_string()), ("dates", &r["dates"].to_string())],
            ));
            app.map.invalidate();
        }
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// The `stac.addCube` parameters for the dialog as it stands: its API, collection, band, dates,
/// area, cloud limit and display range, and the connection picked for the API (so the cube's
/// requests use that login, not the origin's last-selected one). `first` and `last` are the
/// dates the items span, used where no date is typed.
#[cfg(not(target_arch = "wasm32"))]
fn cube_params(st: &State, first: String, last: String, connection: Option<String>) -> Result<serde_json::Value, String> {
    let mut p = json!({
        "url": st.api, "collection": st.collection.trim(), "asset": st.cube_asset,
        "start": if st.from.trim().is_empty() { first } else { st.from.trim().to_string() },
        "end": if st.to.trim().is_empty() { last } else { st.to.trim().to_string() },
    });
    if let Some(connection) = connection {
        p["connection"] = json!(connection);
    }
    // The area the search asked for, so the cube covers what it found (every page of it), not the
    // view now; a search anywhere, the area of the items listed.
    let area = st.searched_area.or_else(|| {
        st.items.iter().filter_map(|i| i.bbox).fold(None, |a: Option<[f64; 4]>, [w, s, e, n]| {
            Some(a.map_or([w, s, e, n], |[aw, as_, ae, an]| [aw.min(w), as_.min(s), ae.max(e), an.max(n)]))
        })
    });
    if let Some(b) = area {
        p["bbox"] = json!(b);
    }
    if let Ok(c) = st.max_cloud.trim().parse::<f64>() {
        p["maxCloud"] = json!(c);
    }
    let typed: Vec<&str> = st.cube_range.split([',', ';', ' ']).map(str::trim).filter(|v| !v.is_empty()).collect();
    let range: Vec<f64> = typed.iter().filter_map(|v| v.parse::<f64>().ok()).collect();
    match (typed.len(), range.len()) {
        (0, _) => {}
        (2, 2) => p["range"] = json!(range),
        _ => return Err(tl!("The range is two numbers, low and high (0, 0.9)").to_string()),
    }
    Ok(p)
}

/// Finds items: an API's search, or a walk of a static catalog from the node picked.
fn find_items(app: &mut RustGisApp) {
    let st = &app.ui.stac;
    // A catalog's children still coming don't hold a search up.
    if st.walk.is_some() || st.waiting.as_ref().is_some_and(|(a, _)| !matches!(a, Asked::Children(_))) {
        return;
    }
    let datetime = match (st.from.trim(), st.to.trim()) {
        ("", "") => None,
        (a, "") => Some(format!("{a}/..")),
        ("", b) => Some(format!("../{b}")),
        (a, b) => Some(format!("{a}/{b}")),
    };
    let is_static = catalog::is_static(&st.api);
    let mut p = json!({"limit": 50});
    if !is_static {
        p["collections"] = json!(st.collection.trim());
    }
    if let Some(d) = datetime {
        p["datetime"] = json!(d);
    }
    if let Ok(c) = st.max_cloud.trim().parse::<f64>() {
        p["maxCloud"] = json!(c);
    }
    // A preset's sort order, only while the dialog is still on the preset's API (another API
    // may not sort).
    if !st.sortby.trim().is_empty() && shown_preset(st).is_some() {
        p["sortby"] = json!(st.sortby.trim());
    }
    if st.use_view {
        p["bbox"] = json!("view");
    }
    let search = rustgis_engine::cmd::stac::search_params(&app.session, &p);
    app.ui.stac.asked_area = search.as_ref().ok().and_then(|s| s.bbox);
    app.ui.stac.error = None;
    if is_static {
        let st = &app.ui.stac;
        let root = if st.node.is_empty() { st.api.trim().to_string() } else { st.node.clone() };
        let walk = rustgis_engine::render::rustgis_tiles::address::check(&root, None)
            .and(search)
            .and_then(|s| Ok(Walk::new(&root, catalog::Filter::from_search(&s, Some(st.phase.as_str()))?, s.limit)));
        match walk {
            Ok(w) => {
                app.ui.stac.info = Some(tl!("Reading the catalog…").to_string());
                app.ui.stac.walk = Some((w, Vec::new()));
            }
            Err(e) => app.ui.stac.error = Some(e),
        }
        return;
    }
    let url = stac::api(Some(app.ui.stac.api.as_str())).and_then(|api| search.and_then(|s| stac::search_url(&api, &s)));
    match url {
        Ok(url) => {
            let slot = fetch_service(app, &url);
            app.ui.stac.waiting = Some((Asked::Search(url), slot));
        }
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// The items found, as cards: the preview picture, the item's id, its collection, date and cloud
/// cover, and Zoom, the asset to add, Add and Details (its properties).
fn results(app: &mut RustGisApp, ui: &mut egui::Ui) {
    if app.ui.stac.items.is_empty() {
        return;
    }
    want_thumbs(app);
    let (mut zoom, mut add, mut details) = (None, None, None);
    // A window starts from its last size: without a floor the list would get egui's 64 px back.
    let tall = (app.ui.stac.items.len() as f32 * 96.0 + if app.ui.stac.details.is_some() { 200.0 } else { 0.0 }).min(440.0);
    egui::ScrollArea::vertical().max_height(440.0).min_scrolled_height(tall).show(ui, |ui| {
        let st = &mut app.ui.stac;
        for (i, item) in st.items.iter().enumerate() {
            egui::Frame::group(ui.style()).inner_margin(6.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    match item.thumbnail().and_then(|a| st.thumbs.get(&a.href)) {
                        Some(Thumb::Ready(t)) => {
                            let size = t.size_vec2();
                            let k = 64.0 / size.x.max(size.y).max(1.0);
                            ui.add(egui::Image::new(t).fit_to_exact_size(size * k));
                        }
                        Some(Thumb::Loading(_)) => {
                            ui.add_sized([64.0, 64.0], egui::Spinner::new());
                        }
                        _ => {}
                    }
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&item.id).strong()).truncate());
                        let date = item.datetime.get(..10).unwrap_or(&item.datetime);
                        let cloud = item.cloud.map(|c| format!("☁ {c:.0}%"));
                        let line: Vec<&str> = [item.collection.as_str(), date, cloud.as_deref().unwrap_or("")]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect();
                        if !line.is_empty() {
                            ui.add(egui::Label::new(egui::RichText::new(line.join(" · ")).weak().small()).truncate());
                        }
                        ui.horizontal_wrapped(|ui| {
                            if item.bbox.is_some() && ui.button(tl!("Zoom")).clicked() {
                                zoom = Some(i);
                            }
                            if let Some(key) = st.chosen.get_mut(i) {
                                egui::ComboBox::from_id_salt(("stac_asset", i))
                                    .selected_text(key.clone())
                                    .width(90.0)
                                    .show_ui(ui, |ui| {
                                        for a in &item.assets {
                                            let label =
                                                if a.title.is_empty() { a.key.clone() } else { format!("{} — {}", a.key, a.title) };
                                            ui.selectable_value(key, a.key.clone(), label);
                                        }
                                    })
                                    .response
                                    .on_hover_text(tl!("Asset"));
                            }
                            if ui.button(tl!("Add")).clicked() {
                                add = Some(i);
                            }
                            if ui.selectable_label(st.details == Some(i), tl!("Details")).clicked() {
                                details = Some(i);
                            }
                        });
                    });
                });
                if st.details == Some(i) {
                    properties(ui, i, item);
                }
            });
        }
    });
    if let Some(i) = details {
        app.ui.stac.details = if app.ui.stac.details == Some(i) { None } else { Some(i) };
    }
    if let Some(e) = zoom.and_then(|i| app.ui.stac.items.get(i)).and_then(|i| i.bbox) {
        app.session.zoom_to(&rustgis_engine::geom::Extent::new(e[0], e[1], e[2], e[3]));
        app.map.invalidate();
    }
    if let Some(i) = add {
        let Some(item) = app.ui.stac.items.get(i).cloned() else { return };
        let key = app.ui.stac.chosen.get(i).cloned().unwrap_or_default();
        add_item_asset(app, &item, Some(key.as_str()).filter(|k| !k.is_empty()));
    }
}

/// An item's properties, a row each (the first hundred, long values cut).
fn properties(ui: &mut egui::Ui, i: usize, item: &Item) {
    let Some(props) = item.properties.as_ref().and_then(|p| p.as_object()) else {
        ui.label(egui::RichText::new(tl!("No properties")).weak());
        return;
    };
    egui::Grid::new(("stac_props", i)).num_columns(2).striped(true).spacing(vec2(10.0, 2.0)).show(ui, |ui| {
        for (k, v) in props.iter().take(100) {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let short: String = text.chars().take(160).collect();
            ui.label(egui::RichText::new(k).small());
            ui.add(egui::Label::new(egui::RichText::new(short).small().weak()).wrap());
            ui.end_row();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RustGisApp;

    #[test]
    fn opening_a_stac_api_from_the_tree_drops_the_last_apis_catalog_walk() {
        let mut app = RustGisApp::with_sample();
        let old = "https://old-walk757.example.org/catalog.json";
        app.ui.stac.api = old.into();
        app.ui.stac.walk = Some((Walk::new(old, catalog::Filter::default(), 10), Vec::new()));
        app.ui.stac.thumbs.insert("https://new-walk757.example.org/preview.png".into(), Thumb::Failed);
        let source = rustgis_engine::sources::Source {
            kind: "stac".into(),
            name: "New API".into(),
            address: "https://new-walk757.example.org/stac".into(),
            extra: json!({"api": "https://new-walk757.example.org/stac"}),
        };
        crate::sourcespane::open(&mut app, &source).unwrap();
        assert!(app.ui.stac.walk.is_none(), "the old catalog's documents must not land in the new listing");
        assert_eq!(app.ui.stac.api, "https://new-walk757.example.org/stac");
        assert!(app.ui.stac.thumbs.is_empty(), "a preview read through the last API's login isn't shown for the next");
    }

    #[test]
    fn a_deferred_token_add_keeps_its_original_connection_when_selection_changes() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        let api = "http://127.0.0.1:9/stac";
        let one = app.run("connection.new", json!({"kind":"stac", "name":"One", "address":api})).unwrap();
        let two = app.run("connection.new", json!({"kind":"stac", "name":"Two", "address":api})).unwrap();
        app.ui.stac.api = api.into();
        app.ui.stac.connection = one["id"].as_str().map(str::to_string);
        let asked = Asked::Token {
            token_url: "https://snapshot-token.invalid/token".into(),
            href: "http://127.0.0.1:9/points.geojson".into(),
            name: "Original asset".into(),
            connection: selected_connection(&app),
        };
        let slot = Slot::default();
        *slot.lock().unwrap() = Some(Ok(br#"{"token":"sig=snapshot"}"#.to_vec()));
        app.ui.stac.waiting = Some((asked, slot));
        app.ui.stac.connection = two["id"].as_str().map(str::to_string);
        poll(&mut app, &egui::Context::default());
        let (_, _, params, _) = app.ui.downloads.last().unwrap();
        assert_eq!(params["connection"], one["id"], "token completion must not re-read the picker");
        assert_ne!(params["connection"], two["id"]);
    }

    /// The cube's request names the connection picked for the API, so it never falls back to the
    /// origin's last-selected login (possibly another connection's).
    #[test]
    fn the_time_series_request_names_the_picked_connection() {
        let st = State { api: "https://stac.example.org".into(), collection: "c".into(), ..State::default() };
        let p = cube_params(&st, "2024-01-01".into(), "2024-02-01".into(), Some("picked".into())).unwrap();
        assert_eq!(p["connection"], "picked");
        assert!(cube_params(&st, "a".into(), "b".into(), None).unwrap().get("connection").is_none());
    }

    #[test]
    fn presets_open_the_dialog_set_up() {
        for p in Preset::ALL {
            assert_eq!(Preset::from_id(p.id()), Some(p));
        }
        assert_eq!(Preset::from_id("nope"), None);
        // The dialog opens on Collection 1: the legacy `sentinel-2-l2a` declares an offset that many of its files lack.
        assert_eq!(State::default().collection, "sentinel-2-c1-l2a");
        let d = days_ago(0);
        assert!(d.len() == 10 && d.starts_with("20"), "{d}");
        let mut app = RustGisApp::with_sample();
        assert!(app.run("ui.stac.nope", json!({})).is_err());
        // An API's preset fetches nothing until Find Items.
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        assert_eq!(app.ui.web_data, "stac");
        assert_eq!((app.ui.stac.collection.as_str(), app.ui.stac.sortby.as_str()), ("sentinel-2-c1-l2a", "eo:cloud_cover"));
        assert_eq!(title(&app), "Sentinel-2 Explorer");
        assert!(app.ui.stac.waiting.is_none() && app.ui.stac.use_view);
        app.run("ui.stac.usgsLidar", json!({})).unwrap();
        assert_eq!((app.ui.stac.api.as_str(), app.ui.stac.collection.as_str()), (stac::PLANETARY_COMPUTER, "3dep-lidar-copc"));
        // Typing another address leaves the service: plain STAC Search again.
        app.ui.stac.api = "https://example.org/stac/v1".into();
        assert_eq!(title(&app), "STAC Search");
        app.run("ui.stac", json!({})).unwrap();
        assert_eq!(app.ui.stac.preset, None);
        // A static catalog's search walks it; bad dates are refused before anything is read.
        app.ui.stac.api = VANTOR.into();
        app.ui.stac.from = "soon".into();
        find_items(&mut app);
        assert!(app.ui.stac.walk.is_none() && app.ui.stac.error.is_some());
    }

    /// An answer asked of one API that lands after another was picked isn't shown under it; a
    /// link being opened goes on.
    #[test]
    fn a_late_answer_from_the_previous_api_is_dropped() {
        let mut app = RustGisApp::with_sample();
        let ctx = egui::Context::default();
        let late: Answer = Slot::default();
        app.ui.stac.api = "https://one.example/stac".into();
        app.ui.stac.waiting = Some((Asked::Collections("https://one.example/stac".into()), late.clone()));
        switch_api(&mut app.ui.stac, "https://two.example/stac");
        assert!(app.ui.stac.waiting.is_none());
        *late.lock().unwrap() = Some(Ok(br#"{"collections":[{"id":"old","title":"Old"}]}"#.to_vec()));
        poll(&mut app, &ctx);
        assert!(app.ui.stac.collections.is_empty());
        assert_eq!(app.ui.stac.api, "https://two.example/stac");

        // Listed, then away and back: Save as Connection waits for a listing of its own, and
        // the last listing's status goes with it.
        app.ui.stac.listed = "https://two.example/stac".into();
        app.ui.stac.info = Some("Showing 10 items".into());
        switch_api(&mut app.ui.stac, "https://one.example/stac");
        switch_api(&mut app.ui.stac, "https://two.example/stac");
        assert!(app.ui.stac.listed.is_empty());
        assert_eq!(app.ui.stac.info, None);

        app.ui.stac.waiting = Some((Asked::Link("https://two.example/item.json".into()), Slot::default()));
        switch_api(&mut app.ui.stac, "https://three.example/stac");
        assert!(matches!(app.ui.stac.waiting, Some((Asked::Link(_), _))), "a link already asked for goes on");
    }

    /// A static catalog picked while a listing of the last API is pending is browsed at once:
    /// the dropped listing doesn't hold it up.
    #[test]
    fn a_catalog_picked_during_a_pending_listing_is_browsed() {
        let _turn = crate::fetching::turn();
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        // Nothing listens on port 9 of the loopback address: the browse is refused, harmlessly.
        app.run("connection.new", json!({"kind":"stac","name":"Local catalog","address":"http://127.0.0.1:9/catalog.json"})).unwrap();
        app.ui.stac.api = "https://one.example/stac".into();
        app.ui.stac.waiting = Some((Asked::Collections("https://one.example/stac".into()), Slot::default()));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(|ui, app: &mut RustGisApp| dialog(app, ui), app);
        h.step();
        h.step();
        h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("Other")).unwrap().click();
        // The list opens on the frame after the click.
        h.step();
        h.step();
        h.get_by_label("Local catalog").click();
        h.step();
        assert_eq!(h.state().ui.stac.api, "http://127.0.0.1:9/catalog.json");
        assert!(matches!(&h.state().ui.stac.waiting, Some((Asked::Children(url), _)) if url == "http://127.0.0.1:9/catalog.json"));
    }
}
