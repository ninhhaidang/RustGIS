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
    address::{self, Origin, Reach},
    cache,
    stac::{
        self, Item,
        catalog::{self, Child, Walk},
    },
};
use serde_json::json;

use crate::{RustGisApp, Slot, i18n};
use rustgis_engine::cmd::stac_area::{Loaded, Prepared};
use rustgis_engine::data::Geometry;
use rustgis_engine::geom::Coord;

type Answer = Slot<Result<cache::Fetched, String>>;
type BytesAnswer = Slot<Result<Vec<u8>, String>>;

/// What is being fetched.
enum Asked {
    /// An API's collections, by the address as typed.
    Collections(String),
    /// A search, by its address (which the answer's links are resolved against), and the same
    /// search as a GET in the address when it went as a POST and fits in one: asked if the server
    /// refuses the POST.
    Search(String, Option<String>),
    /// A token for an asset that needs signing: the token service's address, the asset's, the
    /// layer's name, saved connection, supplying document origin and reach, captured when Add was
    /// requested.
    Token { token_url: String, href: String, name: String, connection: Option<String>, from: String, public: bool, opened: u64 },
    /// An item's or a collection's document (a `?stac=` link), by its address.
    Link { url: String, opened: u64 },
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
    /// The search area built so far: each shape drawn, the selected features, a file's features
    /// and the map's extent add a part. Merged by the search (the highest dimension present is
    /// kept), and widened by `buffer` in the Distance units when it is points or lines. While
    /// there is one it replaces "where the map is".
    pub parts: Vec<Geometry>,
    pub buffer: String,
    /// The shape being drawn on the map (`point`, `line`, `polygon` or `rectangle`).
    pub drawing: Option<&'static str>,
    /// The features being read and merged on a thread of their own (a file's, or the selection's
    /// copy), by what they come from; Cancel drops the answer.
    loading: Option<(String, Slot<Result<Loaded, String>>)>,
    /// A search whose area is being made (merged and widened) on a thread of its own, with the
    /// search to send once it lands; Clear drops it.
    preparing: Option<Preparing>,
    pub max_cloud: String,
    /// Whether the dates and the cloud limit are used in the search: off, the typed values stay
    /// in their boxes but are not sent (a collection without `eo:cloud_cover` finds nothing
    /// with a limit).
    pub use_dates: bool,
    pub use_cloud: bool,
    /// The order of an API's items (`eo:cloud_cover`…); empty for the API's own.
    pub sortby: String,
    /// The Web Data service the dialog was opened as.
    pub preset: Option<Preset>,
    /// A static catalog: the node Find Items walks from (empty for the catalog itself), its
    /// title, the nodes above it (title, address) and its children.
    pub node: String,
    pub node_title: String,
    pub trail: Vec<(String, String)>,
    node_reach: Option<Reach>,
    trail_reach: Vec<Option<Reach>>,
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
    /// The sort the search under way was sent with (the dropdown may change while it waits).
    asked_sort: Option<String>,
    /// Whether `sortby` is the one a preset set up and the user has not touched the dropdown
    /// since: only that sort goes with the preset (see [`drop_preset_sort`]).
    sort_is_default: bool,
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
    /// Whether `api` came from a document's links (an opened item or collection) rather than from
    /// the user: a request body goes to such an address only on the internet.
    api_from_document: bool,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl State {
    /// The typed dates (from, to), empty when the dates are switched off.
    fn dates(&self) -> (&str, &str) {
        if self.use_dates { (self.from.trim(), self.to.trim()) } else { ("", "") }
    }

    /// The cloud limit to send: none when it is switched off or left blank (any cloud cover), an
    /// error when what is typed is not a number from 0 to 100.
    fn cloud_limit(&self) -> Result<Option<f64>, String> {
        let typed = self.max_cloud.trim();
        if !self.use_cloud || typed.is_empty() {
            return Ok(None);
        }
        match typed.parse::<f64>() {
            Ok(c) if (0.0..=100.0).contains(&c) => Ok(Some(c)),
            _ => Err(tl!("The most cloud cover is a number from 0 to 100").to_string()),
        }
    }
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
            parts: Vec::new(),
            buffer: String::new(),
            drawing: None,
            loading: None,
            preparing: None,
            max_cloud: "20".into(),
            use_dates: false,
            use_cloud: true,
            sortby: String::new(),
            preset: None,
            node: String::new(),
            node_title: String::new(),
            trail: Vec::new(),
            node_reach: None,
            trail_reach: Vec::new(),
            children: Vec::new(),
            phase: String::new(),
            walk: None,
            items: Vec::new(),
            searched_area: None,
            asked_area: None,
            asked_sort: None,
            sort_is_default: false,
            chosen: Vec::new(),
            details: None,
            counted: false,
            thumbs: Default::default(),
            footprints: None,
            waiting: None,
            api_from_document: false,
            error: None,
            info: None,
        }
    }
}

fn fetch(url: &str, from_document: bool) -> Answer {
    let slot: Answer = Slot::default();
    let land = slot.clone();
    let done = Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r));
    if from_document {
        cache::download_public_with_origin(url, done);
    } else {
        cache::download_with_origin(url, done);
    }
    slot
}

/// Reads with the selected API's login only; a catalog's foreign links stay anonymous.
fn fetch_service(app: &mut RustGisApp, url: &str, from_document: bool) -> Answer {
    let from_document = from_document || app.ui.stac.api_from_document;
    let connection = selected_connection(app);
    let base = app.ui.stac.api.clone();
    match crate::serviceui::request_from(app, connection.as_deref(), &base, url) {
        Ok(url) => fetch(&url, from_document),
        Err(e) => Slot::new(std::sync::Mutex::new(Some(Err(crate::connectionui::translated(&e))))),
    }
}

/// Like [`fetch_service`], POSTing `body` (a search by shape).
fn post_service(app: &mut RustGisApp, url: &str, body: String) -> Answer {
    let connection = selected_connection(app);
    let base = app.ui.stac.api.clone();
    match crate::serviceui::request_from(app, connection.as_deref(), &base, url) {
        Ok(url) => {
            let slot: Answer = Slot::default();
            let land = slot.clone();
            let public = app.ui.stac.api_from_document;
            let done = Box::new(move |r| {
                *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            });
            cache::post_json_with_origin(&url, body, public, done);
            slot
        }
        Err(e) => Slot::new(std::sync::Mutex::new(Some(Err(crate::connectionui::translated(&e))))),
    }
}

/// An item's preview picture: being fetched, shown, or not to be had.
enum Thumb {
    Loading(BytesAnswer),
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
    let mut refused = 0usize;
    for item in st.items.iter().take(MAX_THUMBS / 2) {
        let Some(a) = item.thumbnail() else { continue };
        if st.thumbs.contains_key(&a.href) {
            continue;
        }
        let allowed = item.check_href(&a.href).is_ok();
        if !allowed {
            refused += 1;
        }
        let ok = !stac::needs_signing(&a.href) && allowed;
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
        let public = item.origin_reach == Reach::Public;
        let picture = address.map(|address| {
            let slot: BytesAnswer = Slot::default();
            let land = slot.clone();
            let done = Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r));
            if public {
                cache::download_public_limited(&address, THUMB_BYTES, done);
            } else {
                cache::download_limited(&address, THUMB_BYTES, done);
            }
            Thumb::Loading(slot)
        });
        st.thumbs.insert(a.href.clone(), picture.unwrap_or(Thumb::Failed));
    }
    if refused > 0 {
        session.notify(i18n::fmt("{n} preview links were refused by the document's network reach", &[("n", &refused.to_string())]), false);
    }
}

/// Lands collections and search results.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    poll_thumbs(app, ctx);
    poll_walk(app, ctx);
    poll_loading(app, ctx);
    poll_preparing(app, ctx);
    stop_stale_drawing(app);
    let pending = match &app.ui.stac.waiting {
        Some((Asked::Link { url, opened }, _)) => Some((*opened, url.clone())),
        Some((Asked::Token { href, opened, .. }, _)) => Some((*opened, href.clone())),
        _ => None,
    };
    if let Some((opened, address)) = pending
        && !app.service_read_current(opened, &address, None)
    {
        app.ui.stac.waiting = None;
        app.ui.stac.info = None;
        return;
    }
    let Some((_, slot)) = &app.ui.stac.waiting else { return };
    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some((asked, _)) = app.ui.stac.waiting.take() else { return };
    // A server that refuses a POST search is asked again in the address, when the shape fits.
    if let Asked::Search(_, Some(get)) = &asked
        && r.is_err()
    {
        let get = get.clone();
        let slot = fetch_service(app, &get, false);
        app.ui.stac.waiting = Some((Asked::Search(get, None), slot));
        return;
    }
    let st = &mut app.ui.stac;
    st.error = None;
    let previous = st.info.take();
    match asked {
        Asked::Collections(api) => match r.and_then(|f| stac::parse_collections(&f.bytes)) {
            Ok(c) => {
                st.info = Some(i18n::fmt("{n} collections", &[("n", &c.len().to_string())]));
                st.collections = c.into_iter().map(|c| (c.id, c.title)).collect();
                st.listed = api;
            }
            Err(e) => st.error = Some(e),
        },
        Asked::Search(url, _) => {
            match r.and_then(|f| {
                let origin = if f.origin.url().is_empty() { Origin::named(&url) } else { f.origin };
                Ok((
                    stac::parse_items_from_origin(&f.bytes, &origin)?,
                    stac::parse_matched(&f.bytes),
                    stac::has_next(&f.bytes),
                    stac::next_refused(&f.bytes, &origin),
                ))
            }) {
                Ok(((items, next), matched, has_next, refused)) => {
                    let n = items.len().to_string();
                    // Logical order until the line is whole: `fmt` puts a finished message in display
                    // form, so it runs once, over the count and the note together.
                    let count = match matched.filter(|m| !refused && *m as usize > items.len()) {
                        Some(m) => {
                            i18n::fmt_raw("Showing {n} of {m} items (narrow the search for others)", &[("n", &n), ("m", &m.to_string())])
                        }
                        // A POST search's `next` link has no address to give, so `next` is none for it.
                        None if !refused && (next.is_some() || has_next) => {
                            i18n::fmt_raw("Showing {n} items (more match: narrow the search)", &[("n", &n)])
                        }
                        None => i18n::fmt_raw("Showing {n} items", &[("n", &n)]),
                    };
                    // An API that can't sort may ignore the sort instead of refusing it: say so.
                    // The sort that was sent, not the dropdown's value now.
                    let asked = st.asked_sort.as_deref();
                    st.info = Some(match sort_ignored_note(&items, asked).or_else(|| sort_unchecked_note(&items, asked)) {
                        Some(note) => i18n::fmt("{count}. {note}", &[("count", &count), ("note", &note)]),
                        None => i18n::fmt("{count}", &[("count", &count)]),
                    });
                    if refused {
                        let note =
                            tl!("The next page link was refused: it points where this search may not read, so only this page is loaded.");
                        st.info = Some(i18n::fmt("{count}. {note}", &[("count", st.info.as_deref().unwrap_or(&count)), ("note", note)]));
                    }
                    st.details = None;
                    st.counted = true;
                    st.chosen = items.iter().map(|i| i.pick(None).map(|a| a.key.clone()).unwrap_or_default()).collect();
                    st.items = items;
                    st.searched_area = st.asked_area;
                    show_footprints(app);
                }
                Err(e) => st.error = Some(e),
            }
        }
        Asked::Token { token_url, href, name, connection, from, public, .. } => {
            let signed = r.and_then(|f| stac::parse_sas(&f.bytes)).and_then(|token| {
                stac::remember_token(&token_url, &token);
                let signed = stac::sign(&href, &token);
                address::check_reach(&signed, rustgis_engine::cmd::layer::origin_ceiling(Some(&from), public))?;
                Ok(signed)
            });
            match signed {
                Ok(signed) => add_url_from(app, &signed, &name, connection.as_deref(), &from, public),
                Err(e) => app.ui.stac.error = Some(e),
            }
        }
        Asked::Link { url, .. } => landed_link(app, &url, r),
        Asked::Children(url) => match r.and_then(|f| Ok((f.origin.reach(), catalog::parse_children_noted(&f.bytes, &f.origin)?))) {
            Ok((reach, listing)) => {
                if listing.refused > 0 {
                    app.session.notify(
                        i18n::fmt("{n} catalog links were refused by the document's network reach", &[("n", &listing.refused.to_string())]),
                        false,
                    );
                }
                if listing.skipped > 0 {
                    app.session.notify(
                        i18n::fmt(
                            "{n} catalog links were left out because their addresses are missing, invalid or not HTTP(S)",
                            &[("n", &listing.skipped.to_string())],
                        ),
                        false,
                    );
                }
                if listing.omitted > 0 {
                    app.session.notify(
                        i18n::fmt(
                            "{n} catalog links were left out because the listing limit was reached",
                            &[("n", &listing.omitted.to_string())],
                        ),
                        false,
                    );
                }
                let c = listing.links;
                st.node_reach = Some(reach);
                // A search's count (or a walk's progress), landed first, is kept.
                st.info = if st.walk.is_some() || (st.counted && !st.items.is_empty()) {
                    previous
                } else {
                    Some(if c.is_empty() {
                        tl!("No catalogs below this one: Find Items lists its items").to_string()
                    } else {
                        i18n::fmt("{n} catalogs and collections", &[("n", &c.len().to_string())])
                    })
                };
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
                walk.land_fetched(url, r);
                fetching.swap_remove(i);
            }
            None => i += 1,
        }
    }
    let ancestor_public = app.ui.stac.trail_reach.iter().flatten().any(|r| *r == Reach::Public);
    while fetching.len() < catalog::AT_ONCE {
        let is_root = walk.reads == 0 && fetching.is_empty();
        let Some((url, parent_public)) = walk.next_request() else { break };
        let from_document = if is_root {
            app.ui.stac.api_from_document || ancestor_public
        } else {
            app.ui.stac.api_from_document || ancestor_public || walk.root_reach() == Some(Reach::Public) || parent_public
        };
        let slot = fetch_service(app, &url, from_document);
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
    if walk.refused() > 0 {
        app.session.notify(
            i18n::fmt("{n} catalog links were refused by the document's network reach", &[("n", &walk.refused().to_string())]),
            false,
        );
    }
    if walk.skipped() > 0 {
        app.session.notify(
            i18n::fmt(
                "{n} catalog links were left out because their addresses are missing, invalid or not HTTP(S)",
                &[("n", &walk.skipped().to_string())],
            ),
            false,
        );
    }
    let st = &mut app.ui.stac;
    let n = walk.items().len().to_string();
    // Refused and skipped links have their own notices above: the stops they don't say come first.
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
    } else if walk.refused() > 0 {
        i18n::fmt("Showing {n} items ({f} catalog links were refused)", &[("n", &n), ("f", &walk.refused().to_string())])
    } else if walk.skipped() > 0 {
        i18n::fmt("Showing {n} items ({s} catalog links were skipped)", &[("n", &n), ("s", &walk.skipped().to_string())])
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
                (st.sortby, st.sort_is_default) = (PRESET_SORT.into(), true);
                st.from = days_ago(90);
                st.use_dates = true;
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
        // A preset's own sort goes with the preset, unless a search is under way with it (its area
        // being made, or sent and not answered): `keeps_preset_sort` is the one rule for that, so
        // the dropdown and the next search follow it.
        app.ui.stac.preset = None;
        keep_preset_sort_for_its_api(&mut app.ui.stac);
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
    let (y, m, d) = rustgis_engine::data::date::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Adds an asset by its address, signing it first when it needs it (a token in hand is used; one
/// still to fetch is fetched off the frame, and the asset added when it lands).
fn add_signed(app: &mut RustGisApp, href: &str, name: &str, from: &str, public: bool) {
    let ceiling = rustgis_engine::cmd::layer::origin_ceiling(Some(from), public);
    if let Err(e) = address::check_reach(href, ceiling) {
        app.ui.stac.error = Some(e);
        return;
    }
    if !stac::needs_signing(href) {
        return add_url(app, href, name, from, public);
    }
    match stac::sas_token_url(href) {
        Ok(token_url) => match stac::cached_token(&token_url) {
            Some(token) => {
                let signed = stac::sign(href, &token);
                if let Err(e) = address::check_reach(&signed, ceiling) {
                    app.ui.stac.error = Some(e);
                    return;
                }
                add_url(app, &signed, name, from, public);
            }
            None => {
                if let Err(e) = address::check_reach(&token_url, ceiling) {
                    app.ui.stac.error = Some(e);
                    return;
                }
                let slot = fetch(&token_url, public);
                app.ui.stac.info = Some(tl!("Signing the asset (Planetary Computer)…").to_string());
                let connection = selected_connection(app);
                app.ui.stac.waiting = Some((
                    Asked::Token {
                        token_url,
                        href: href.to_string(),
                        name: name.to_string(),
                        connection,
                        from: from.to_string(),
                        public,
                        opened: app.session.opened,
                    },
                    slot,
                ));
            }
        },
        Err(e) => app.ui.stac.error = Some(e),
    }
}

fn add_url(app: &mut RustGisApp, href: &str, name: &str, from: &str, public: bool) {
    let connection = selected_connection(app);
    add_url_from(app, href, name, connection.as_deref(), from, public);
}

fn selected_connection(app: &RustGisApp) -> Option<String> {
    crate::connectionui::picked(&app.session.connections.connections, app.ui.stac.connection.as_deref(), &app.ui.stac.api)
        .map(|c| c.id.clone())
}

fn add_url_from(app: &mut RustGisApp, href: &str, name: &str, connection: Option<&str>, from: &str, public: bool) {
    let mut p = json!({"url": href, "name": name, "zoom": true, "from": from, "fromDocument": public});
    if let Some(connection) = connection {
        p["connection"] = json!(connection);
    }
    let reach = if public { Reach::Public } else { Reach::Local };
    let result = Origin::with_observed_reach(from, reach)
        .and_then(|origin| app.session.remember_document_asset(href, origin))
        .and_then(|()| app.run("ui.addUrl", p));
    if let Err(e) = result {
        app.ui.stac.error = Some(e);
    }
}

/// The web document an item's asset links were read from, taking observed document provenance over
/// self-reported links.
fn item_document_origin(app: &RustGisApp, item: &Item) -> String {
    item.origin
        .as_deref()
        .or(item.url.as_deref())
        .or_else(|| {
            let api = app.ui.stac.api.trim();
            (!api.is_empty()).then_some(api)
        })
        .unwrap_or("https://item.invalid/")
        .to_string()
}

/// Adds an item's asset (the one named, else the default) under the item's name.
fn add_item_asset(app: &mut RustGisApp, item: &Item, key: Option<&str>) {
    let from = item_document_origin(app, item);
    let public = item.origin_reach == Reach::Public;
    match item.asset_href(key) {
        Ok((key, href)) => add_signed(app, &href, &format!("{} {key}", item.id), &from, public),
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// Opens the dialog on an item's or a collection's address (a `?stac=` link), fetching it off
/// the frame.
pub fn open_link(app: &mut RustGisApp, url: &str) -> Result<(), String> {
    let url = url.trim();
    address::check(url, None)?;
    app.run("ui.dialog", serde_json::json!({"name": "stac"}))?;
    let address = rustgis_engine::service_logins::request(&mut app.session, None, url)?;
    let slot = fetch(&address, false);
    app.ui.stac.error = None;
    app.ui.stac.info = Some(i18n::fmt("Opening {url}…", &[("url", url)]));
    app.ui.stac.waiting = Some((Asked::Link { url: url.to_string(), opened: app.session.opened }, slot));
    Ok(())
}

/// A linked document: an item is shown and its default asset added; a collection is chosen in the
/// dialog, with its API, ready to search.
fn landed_link(app: &mut RustGisApp, requested_url: &str, r: Result<cache::Fetched, String>) {
    let fetched = match r {
        Ok(f) => f,
        Err(e) => {
            app.ui.stac.error = Some(e);
            return;
        }
    };
    let effective_url = fetched.origin.url();
    let bytes = &fetched.bytes;
    let kind = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| Some((v.get("type")?.as_str()?.to_string(), v.get("id").and_then(|i| i.as_str()).map(str::to_string))));
    // The API: the address before `/collections/`, else the document's root (or parent) link.
    let api = stac::api_of(bytes, effective_url);
    // Without one, the dialog keeps its API, and says which it is.
    let unknown =
        |app: &RustGisApp| i18n::fmt("The link doesn't say its STAC API: Find Items searches {api}", &[("api", &app.ui.stac.api)]);
    match kind {
        Some((t, _)) if t == "Feature" => match stac::parse_item_from_origin(bytes, &fetched.origin) {
            Ok(mut item) => {
                item.url.get_or_insert_with(|| effective_url.to_string());
                app.ui.stac.collection = item.collection.clone();
                app.ui.stac.chosen = vec![item.pick(None).map(|a| a.key.clone()).unwrap_or_default()];
                app.ui.stac.items = vec![item.clone()];
                app.ui.stac.searched_area = None;
                let found = api.is_some();
                if let Some(api) = api {
                    app.ui.stac.api = api;
                    app.ui.stac.api_from_document = true;
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
                app.ui.stac.api_from_document = true;
            }
            app.ui.stac.collection = id.chars().take(200).collect();
            app.ui.stac.info = Some(i18n::fmt("Collection {id}: choose dates and Find Items", &[("id", &app.ui.stac.collection)]));
            if !found {
                app.ui.stac.info = Some(format!("{} · {}", app.ui.stac.info.clone().unwrap_or_default(), unknown(app)));
            }
        }
        _ => {
            app.ui.stac.error = Some(i18n::fmt(
                "{url}: not a STAC item or collection",
                &[("url", &rustgis_engine::sources::address_in_message(requested_url))],
            ))
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
            let above_reach = st.node_reach.take();
            st.trail.push((above, above_url));
            st.trail_reach.push(above_reach);
        }
        None => {
            (st.node, st.node_title, st.node_reach) = (String::new(), String::new(), None);
            st.trail.clear();
            st.trail_reach.clear();
        }
    }
    fetch_children(app);
}

/// Goes back up a static catalog to the node at `depth` in the trail.
fn browse_up(app: &mut RustGisApp, depth: usize) {
    let st = &mut app.ui.stac;
    st.walk = None;
    let Some((title, url)) = st.trail.get(depth).cloned() else { return };
    let reach = st.trail_reach.get(depth).copied().flatten();
    st.trail.truncate(depth);
    st.trail_reach.truncate(depth);
    (st.node_title, st.node, st.node_reach) = (title, url, reach);
    fetch_children(app);
}

/// Fetches the children of the static catalog's node (or of the catalog itself).
fn fetch_children(app: &mut RustGisApp) {
    let st = &mut app.ui.stac;
    st.children.clear();
    st.error = None;
    let is_root = st.node.is_empty();
    let url = if is_root { st.api.trim().to_string() } else { st.node.clone() };
    let ancestor_public = st.trail_reach.iter().flatten().any(|r| *r == Reach::Public);
    let from_document = if is_root { st.api_from_document } else { st.api_from_document || ancestor_public };
    let ceiling = if from_document { Reach::Public } else { Reach::Local };
    if let Err(e) = address::check_reach(&url, ceiling) {
        st.error = Some(e);
        return;
    }
    st.info = Some(tl!("Reading the catalog…").to_string());
    st.counted = false;
    let slot = fetch_service(app, &url, from_document);
    app.ui.stac.waiting = Some((Asked::Children(url), slot));
}

/// The orders the dialog offers an API's items in: the sort field (`-` first for descending) and its label.
const SORTS: [(&str, &str); 4] =
    [("", "API's own order"), ("-datetime", "Newest first"), ("datetime", "Oldest first"), ("eo:cloud_cover", "Least cloud first")];

/// The sort the Sentinel-2 preset starts with.
const PRESET_SORT: &str = "eo:cloud_cover";

/// Drops the sort a preset set up, if the user has not picked one since: a sort the user chose
/// (even the same one) is theirs and stays.
fn drop_preset_sort(st: &mut State) {
    if std::mem::take(&mut st.sort_is_default) {
        st.sortby.clear();
    }
}

/// A preset's own sort goes only to the preset's API (another may reject it): dropped when the
/// dialog is on another API. Typing in the API field doesn't call this, so deleting a character
/// of the address and typing it back keeps the sort; it is judged when a search is sent and when
/// another API is picked.
fn keep_preset_sort_for_its_api(st: &mut State) {
    if !keeps_preset_sort(st) {
        drop_preset_sort(st);
    }
}

/// Whether a preset's own sort still stands: the dialog is on the preset's API, or a search is
/// having its area made and will go out with that sort, whatever the dialog is switched to
/// meanwhile. The dropdown, and what drops the sort, follow this one rule so they never disagree.
fn keeps_preset_sort(st: &State) -> bool {
    shown_preset(st).is_some() || st.preparing.is_some() || matches!(st.waiting, Some((Asked::Search(..), _)))
}

/// The sort a search sends: the one picked, none for the API's own order or a static catalog.
fn sort_of(st: &State, is_static: bool) -> Option<&str> {
    Some(st.sortby.trim()).filter(|b| !b.is_empty() && !is_static)
}

/// What to say when the items an API answered are visibly not in the order asked for: an API
/// without the sort extension may ignore a sort rather than refuse it. None when they are in
/// order or it can't be told.
fn sort_ignored_note(items: &[stac::Item], sortby: Option<&str>) -> Option<String> {
    (stac::sort_followed(items, sortby) == Some(false))
        .then(|| i18n::fmt_raw("The API did not sort them by {sort}: they are in its own order", &[("sort", sortby.unwrap_or("").trim())]))
}

/// What to say when a sort was asked for and the items can't show whether the API followed it
/// (dates that can't be compared, or a field that isn't compared): the order is the API's, and
/// not known to be the one asked for. None for a page of one, or no sort.
fn sort_unchecked_note(items: &[stac::Item], sortby: Option<&str>) -> Option<String> {
    stac::sort_unchecked(items, sortby).then(|| {
        i18n::fmt_raw("The order by {sort} could not be checked: it is the API's own order", &[("sort", sortby.unwrap_or("").trim())])
    })
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
    st.api_from_document = false;
    drop_listing(st);
    keep_preset_sort_for_its_api(st);
}

/// Forgets what was listed for the last API or catalog, and what is still on its way from it: a
/// late answer would otherwise land under the new one. An asset being signed or a link being
/// opened was asked for already and goes on.
fn drop_listing(st: &mut State) {
    st.collections.clear();
    st.items.clear();
    st.collection.clear();
    (st.node, st.node_title, st.node_reach) = (String::new(), String::new(), None);
    st.trail.clear();
    st.trail_reach.clear();
    st.children.clear();
    // What Save as Connection would keep: listed again before it is offered.
    st.listed.clear();
    if st.waiting.as_ref().is_some_and(|(a, _)| matches!(a, Asked::Collections(_) | Asked::Search(..) | Asked::Children(_))) {
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

/// Whether a search is being made, sent or walked: what it asked for is fixed by then, so the
/// buttons that ask again, and the sort that was sent, stay as they are until it is done.
fn busy(st: &State) -> bool {
    st.waiting.is_some() || st.walk.is_some() || st.preparing.is_some()
}

/// The dialog.
pub fn dialog(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let busy = busy(&app.ui.stac);
    let is_static = catalog::is_static(&app.ui.stac.api);
    let mut list = false;
    let mut search = false;
    let mut go_to: Option<(String, String)> = None;
    let mut go_up: Option<usize> = None;
    let mut api_changed = false;
    let mut area_ask: Option<AreaAsk> = None;
    let distance_unit = app.forms.distance_units.clone();
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
                st.api_from_document = false;
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
        let dates_label = ui.checkbox(&mut st.use_dates, tl!("Dates")).on_hover_text(tl!("Search only items in these dates"));
        ui.add_enabled_ui(st.use_dates, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut st.from)
                        .hint_text("2024-06-01")
                        .desired_width(((ui.available_width() - 30.0) / 2.0).clamp(70.0, 110.0)),
                )
                .labelled_by(dates_label.id);
                let input_label = ui.label("–");
                ui.add(
                    egui::TextEdit::singleline(&mut st.to).hint_text("2024-06-30").desired_width(ui.available_width().clamp(70.0, 110.0)),
                )
                .labelled_by(input_label.id);
            })
        });
        ui.end_row();
        let cloud_label = ui
            .checkbox(&mut st.use_cloud, tl!("Most cloud cover (%)"))
            .on_hover_text(tl!("Turn off for collections without eo:cloud_cover"));
        ui.add_enabled(
            st.use_cloud,
            egui::TextEdit::singleline(&mut st.max_cloud).hint_text(tl!("any")).desired_width(ui.available_width().clamp(70.0, 110.0)),
        )
        .labelled_by(cloud_label.id);
        ui.end_row();
        if !is_static {
            // A static catalog is walked in its own order, so it has no sort.
            ui.label(tl!("Sort by"));
            let current = if st.sort_is_default && !keeps_preset_sort(st) { String::new() } else { st.sortby.trim().to_string() };
            let shown = SORTS.iter().find(|(v, _)| *v == current).map_or(current.clone(), |(_, l)| tl!(l).to_string());
            // Not while a search is being made: its sort is already chosen, and a pick now would
            // show in the dropdown without being the one sent.
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("stac_sortby").selected_text(shown).show_ui(ui, |ui| {
                    for (v, label) in SORTS {
                        if ui.selectable_value(&mut st.sortby, v.to_string(), tl!(label)).clicked() {
                            st.sort_is_default = false;
                        }
                    }
                });
            });
            ui.end_row();
        }
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
        ui.add_enabled(st.parts.is_empty(), egui::Checkbox::new(&mut st.use_view, tl!("Only where the map is")));
        ui.end_row();
        ui.label(tl!("Search area"));
        ui.vertical(|ui| area_buttons(ui, st, &mut area_ask));
        ui.end_row();
        let input_label = ui.label(i18n::fmt("Widen points and lines by ({unit})", &[("unit", i18n::t(&distance_unit))]));
        ui.add_enabled(
            !st.parts.is_empty(),
            egui::TextEdit::singleline(&mut st.buffer).hint_text("0").desired_width(ui.available_width().clamp(70.0, 110.0)),
        )
        .labelled_by(input_label.id);
        ui.end_row();
    });
    if let Some(ask) = area_ask {
        area_action(app, ask);
    }
    let st = &app.ui.stac;
    let keepable = !st.listed.is_empty()
        && st.listed == st.api.trim()
        && crate::connectionui::picked(&connections, st.connection.as_deref(), &st.listed).is_none();
    let mut keep = false;
    ui.horizontal(|ui| {
        let area_loading = app.ui.stac.loading.is_some();
        search = ui.add_enabled(!busy && !area_loading, egui::Button::new(tl!("Find Items"))).clicked();
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
                let slot = fetch_service(app, &stac::collections_url(&api), false);
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
    let (from, to) = st.dates();
    let mut p = json!({
        "url": st.api, "collection": st.collection.trim(), "asset": st.cube_asset,
        "start": if from.is_empty() { first } else { from.to_string() },
        "end": if to.is_empty() { last } else { to.to_string() },
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
    if let Some(c) = st.cloud_limit()? {
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

/// What the Search area buttons asked for.
#[derive(Clone, Copy, PartialEq)]
enum AreaAsk {
    /// Draw this shape on the map (again: stop drawing it).
    Draw(&'static str),
    Selected,
    Extent,
    #[cfg(not(target_arch = "wasm32"))]
    File,
    Clear,
}

/// The most parts a search area holds.
const MAX_PARTS: usize = 1000;

/// The shapes drawn on the map, with their buttons' labels.
const DRAW: [(&str, &str); 4] = [("point", "Draw point"), ("line", "Draw line"), ("polygon", "Draw polygon"), ("rectangle", "Draw extent")];

/// The Search area's buttons and what it holds now.
fn area_buttons(ui: &mut egui::Ui, st: &mut State, ask: &mut Option<AreaAsk>) {
    let reading = st.loading.as_ref().map(|(what, _)| what.clone());
    ui.horizontal_wrapped(|ui| {
        for (shape, label) in DRAW {
            if ui.selectable_label(st.drawing == Some(shape), tl!(label)).clicked() {
                *ask = Some(AreaAsk::Draw(shape));
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(reading.is_none(), egui::Button::new(tl!("Selected features"))).clicked() {
            *ask = Some(AreaAsk::Selected);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if ui.add_enabled(reading.is_none(), egui::Button::new(tl!("Load file…"))).clicked() {
            *ask = Some(AreaAsk::File);
        }
        if ui.button(tl!("Current extent")).clicked() {
            *ask = Some(AreaAsk::Extent);
        }
        let any = !st.parts.is_empty() || st.drawing.is_some() || reading.is_some() || st.preparing.is_some();
        if ui.add_enabled(any, egui::Button::new(tl!("Clear"))).clicked() {
            *ask = Some(AreaAsk::Clear);
        }
    });
    if let Some(what) = reading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(i18n::fmt("Reading {what}…", &[("what", &what)]));
            if ui.button(tl!("Cancel")).clicked() {
                st.loading = None;
            }
        });
    } else if let Some(shape) = st.drawing {
        let hint = match shape {
            "point" => tl!("Click the map · Esc to stop"),
            "rectangle" => tl!("Drag a box on the map · Esc to stop"),
            _ => tl!("Click to add points · double-click to finish · Esc to stop"),
        };
        ui.label(egui::RichText::new(hint).weak());
    } else if !st.parts.is_empty() {
        ui.label(egui::RichText::new(summary(&st.parts)).weak());
    }
}

/// What a search area holds, by kind.
fn summary(parts: &[Geometry]) -> String {
    let count = |f: fn(&Geometry) -> bool| parts.iter().filter(|g| f(g)).count().to_string();
    i18n::fmt(
        "Search area: {points} point, {lines} line and {polygons} polygon part(s)",
        &[
            ("points", &count(|g| matches!(g, Geometry::Point(_) | Geometry::MultiPoint(_)))),
            ("lines", &count(|g| matches!(g, Geometry::Line(_)))),
            ("polygons", &count(|g| matches!(g, Geometry::Polygon(_)))),
        ],
    )
}

fn area_action(app: &mut RustGisApp, ask: AreaAsk) {
    app.ui.stac.error = None;
    match ask {
        AreaAsk::Draw(shape) => draw(app, shape),
        AreaAsk::Selected => add_selected(app),
        AreaAsk::Extent => add_extent(app),
        #[cfg(not(target_arch = "wasm32"))]
        AreaAsk::File => pick_file(app),
        AreaAsk::Clear => clear_area(app),
    }
}

/// Starts drawing `shape` on the map with the graphic tool of that shape (the drawn shape is taken
/// by [`take_drawn`] instead of becoming a graphic), or stops when it is the one being drawn.
fn draw(app: &mut RustGisApp, shape: &'static str) {
    if app.ui.stac.drawing == Some(shape) {
        stop_drawing(app);
        return;
    }
    let tool = match shape {
        "point" => "graphicPoint",
        "line" => "graphicLine",
        "polygon" => "graphicPolygon",
        _ => "graphicRectangle",
    };
    app.map.cancel_sketch();
    if app.run("tool.set", json!({"tool": tool})).is_ok() {
        app.ui.stac.drawing = Some(shape);
    }
}

fn stop_drawing(app: &mut RustGisApp) {
    app.ui.stac.drawing = None;
    app.map.cancel_sketch();
    let _ = app.run("tool.set", json!({"tool": "explore"}));
}

fn clear_area(app: &mut RustGisApp) {
    if app.ui.stac.drawing.is_some() {
        stop_drawing(app);
    }
    app.ui.stac.parts.clear();
    app.ui.stac.loading = None;
    app.ui.stac.preparing = None;
}

/// Adds a part, within [`MAX_PARTS`] (said when it is full).
fn push_part(app: &mut RustGisApp, g: Geometry) {
    let st = &mut app.ui.stac;
    if st.parts.len() >= MAX_PARTS {
        st.error = Some(i18n::fmt("A search area holds at most {n} shapes: Clear it to start again", &[("n", &MAX_PARTS.to_string())]));
        return;
    }
    st.parts.push(g);
}

/// The selected features of the active layer, merged on a thread of their own.
fn add_selected(app: &mut RustGisApp) {
    match rustgis_engine::cmd::stac_area::selected_geometries(&app.session, &json!({})) {
        Ok((geometries, notes)) => {
            let what = tl!("the selected features").to_string();
            start_loading(app, what, move || rustgis_engine::cmd::stac_area::merge_loaded(&geometries, notes.into()));
        }
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// The map's extent as a polygon.
fn add_extent(app: &mut RustGisApp) {
    match rustgis_engine::cmd::stac::view(&app.session) {
        Ok(b) => push_part(app, rectangle(b)),
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// A box `[west, south, east, north]` as a polygon.
fn rectangle([w, s, e, n]: [f64; 4]) -> Geometry {
    Geometry::Polygon(vec![vec![vec![Coord::new(w, s), Coord::new(e, s), Coord::new(e, n), Coord::new(w, n), Coord::new(w, s)]]])
}

/// A file of features, read and merged on a thread of their own.
#[cfg(not(target_arch = "wasm32"))]
fn pick_file(app: &mut RustGisApp) {
    let Some(path) = crate::filedialog::FileDialog::new().pick_file() else { return };
    let path = path.display().to_string();
    let name = std::path::Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
    start_loading(app, name, move || rustgis_engine::cmd::stac_area::load_file(&path));
}

/// Runs `job` on a thread of its own and takes its answer in [`poll_loading`]; in the browser, which
/// has one thread, here and now.
fn start_loading(app: &mut RustGisApp, what: String, job: impl FnOnce() -> Result<Loaded, String> + Send + 'static) {
    if cfg!(target_arch = "wasm32") {
        land(app, job());
        return;
    }
    let slot: Slot<Result<Loaded, String>> = Slot::default();
    let out = slot.clone();
    let spawned = std::thread::Builder::new().name("stac-area".into()).spawn(move || {
        let answer = job();
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
    });
    match spawned {
        Ok(_) => app.ui.stac.loading = Some((what, slot)),
        Err(e) => app.ui.stac.error = Some(i18n::fmt("Couldn't start reading: {e}", &[("e", &e.to_string())])),
    }
}

/// What the thread made: its notices, then the part (or why there is none).
fn land(app: &mut RustGisApp, answer: Result<Loaded, String>) {
    match answer {
        Ok(loaded) => {
            for n in loaded.notices {
                // An ordinary note warns; a loss says its code too.
                match n.loss {
                    Some(_) => app.session.say(n),
                    None => app.session.warn(n.text),
                }
            }
            push_part(app, loaded.geometry);
        }
        Err(e) => app.ui.stac.error = Some(e),
    }
}

fn poll_loading(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some((_, slot)) = &app.ui.stac.loading else { return };
    let Some(answer) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    app.ui.stac.loading = None;
    land(app, answer);
    ctx.request_repaint();
}

/// Drawing the area stops when another tool is picked, or when the STAC pane is no longer shown
/// (the tool goes back to Explore, so a later shape isn't taken for an area nobody sees).
fn stop_stale_drawing(app: &mut RustGisApp) {
    if app.ui.stac.drawing.is_none() {
        return;
    }
    // Another graphic tool picked from the ribbon is not the shape being drawn here, so a later
    // shape of the drawn kind isn't taken for the area.
    let drawing = app.ui.stac.drawing;
    if !matches!(app.session.tool, rustgis_engine::Tool::Graphic(shape) if Some(shape) == drawing) {
        app.ui.stac.drawing = None;
    } else if app.ui.right != Some(crate::RightPane::WebData) || app.ui.web_data != "stac" {
        stop_drawing(app);
    }
}

/// A longitude within the globe and a latitude clamped to it: a click on a repeated copy of the
/// world is the same place.
fn on_globe(c: Coord) -> Coord {
    Coord::new((c.x + 180.0).rem_euclid(360.0) - 180.0, c.y.clamp(-90.0, 90.0))
}

/// Whether a drawn line (or a polygon, whose last point joins its first) goes over the 180°
/// meridian: two points that are near on the map but far apart once each is wrapped into -180 to
/// 180. Wrapped on its own, each vertex would send the shape the long way round the world.
fn crosses_dateline(raw: &[Coord], closed: bool) -> bool {
    let first = raw.first().into_iter().filter(|_| closed);
    let ends = raw.iter().chain(first).collect::<Vec<_>>();
    ends.windows(2).any(|w| (on_globe(*w[0]).x - on_globe(*w[1]).x).abs() > 180.0 && (w[0].x - w[1].x).abs() <= 180.0)
}

/// The box dragged from corner `a` to corner `b`, as longitudes on the (maybe repeated) world map.
/// A box that crosses the dateline is two boxes, one each side of it, rather than one that spans
/// the world the other way; one 360° wide or more is the whole globe.
fn box_between(a: Coord, b: Coord) -> Geometry {
    let (south, north) = (a.y.min(b.y).clamp(-90.0, 90.0), a.y.max(b.y).clamp(-90.0, 90.0));
    let width = (a.x - b.x).abs();
    if width >= 360.0 {
        return rectangle([-180.0, south, 180.0, north]);
    }
    let west = on_globe(Coord::new(a.x.min(b.x), 0.0)).x;
    let east = west + width;
    if east <= 180.0 {
        return rectangle([west, south, east, north]);
    }
    let (Geometry::Polygon(mut first), Geometry::Polygon(second)) =
        (rectangle([west, south, 180.0, north]), rectangle([-180.0, south, east - 360.0, north]))
    else {
        return rectangle([west, south, 180.0, north]);
    };
    first.extend(second);
    Geometry::Polygon(first)
}

/// Takes a shape drawn with a graphic tool while the area is being drawn: `None` when it isn't
/// (the graphic is added as usual), `Some(true)` when it was taken, `Some(false)` when it was too
/// small to be a shape (the tool stays for another try).
pub(crate) fn take_drawn(app: &mut RustGisApp, shape: &str, pts: &[Coord]) -> Option<bool> {
    if app.ui.stac.drawing.is_none_or(|d| d != shape) {
        return None;
    }
    let raw: Vec<Coord> = pts.iter().copied().filter(|c| c.is_finite()).collect();
    if matches!(shape, "line" | "polygon") && crosses_dateline(&raw, shape == "polygon") {
        app.ui.stac.error =
            Some(tl!("A line or polygon can't cross the 180° meridian: draw it in two parts, one on each side").to_string());
        return Some(false);
    }
    let pts: Vec<Coord> = raw.iter().copied().map(on_globe).collect();
    let geometry = match shape {
        "point" => pts.first().map(|c| Geometry::Point(*c)),
        "line" if pts.len() >= 2 => Some(Geometry::Line(vec![pts])),
        "polygon" if pts.len() >= 3 => {
            let mut ring = pts;
            ring.extend(ring.first().copied());
            Some(Geometry::Polygon(vec![vec![ring]]))
        }
        // The corners as dragged (not wrapped), so a box across the dateline keeps its width.
        "rectangle" => match (raw.first(), raw.get(1)) {
            (Some(a), Some(b)) if (a.x - b.x).abs() > 1e-9 && (a.y - b.y).abs() > 1e-9 => Some(box_between(*a, *b)),
            _ => None,
        },
        _ => None,
    };
    let Some(g) = geometry else { return Some(false) };
    push_part(app, g);
    // A kept tool stays on for the next shape, which joins the area too.
    if !app.session.keep_tool {
        app.ui.stac.drawing = None;
    }
    Some(true)
}

/// Draws the search area on the map while the STAC pane is open.
pub(crate) fn paint_area(app: &RustGisApp, p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    if app.ui.stac.parts.is_empty() || app.ui.right != Some(crate::RightPane::WebData) || app.ui.web_data != "stac" {
        return;
    }
    let stroke = egui::Stroke::new(2.5, color);
    let line = |cs: &[Coord]| -> Vec<egui::Pos2> { cs.iter().filter_map(|c| crate::mapview::lonlat_to_screen(app, rect, *c)).collect() };
    for g in &app.ui.stac.parts {
        match g {
            Geometry::Point(c) => paint_dot(p, &line(&[*c]), color),
            Geometry::MultiPoint(cs) => paint_dot(p, &line(cs), color),
            Geometry::Line(parts) => {
                for part in parts {
                    p.add(egui::Shape::line(line(part), stroke));
                }
            }
            Geometry::Polygon(polygons) => {
                for ring in polygons.iter().flatten() {
                    p.add(egui::Shape::closed_line(line(ring), stroke));
                }
            }
        }
    }
}

fn paint_dot(p: &egui::Painter, at: &[egui::Pos2], color: egui::Color32) {
    for pos in at {
        p.circle_filled(*pos, 5.0, color);
        p.circle_stroke(*pos, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
    }
}

/// What the dialog's filters ask of a search, without the area: the collection, the dates and the
/// cloud limit that are switched on (a switched-off filter sends nothing, a ticked blank one
/// neither) and the sort. An error when the cloud limit typed is not a number from 0 to 100.
fn search_params(st: &State, is_static: bool) -> Result<serde_json::Value, String> {
    let datetime = match st.dates() {
        ("", "") => None,
        (a, "") => Some(format!("{a}/..")),
        ("", b) => Some(format!("../{b}")),
        (a, b) => Some(format!("{a}/{b}")),
    };
    let mut p = json!({"limit": 50});
    if !is_static {
        p["collections"] = json!(st.collection.trim());
    }
    if let Some(d) = datetime {
        p["datetime"] = json!(d);
    }
    if let Some(c) = st.cloud_limit()? {
        p["maxCloud"] = json!(c);
    }
    // The sort picked in the dialog (a preset starts with its own); a static catalog has none.
    if let Some(by) = sort_of(st, is_static) {
        p["sortby"] = json!(by);
    }
    Ok(p)
}

/// Finds items: an API's search, or a walk of a static catalog from the node picked.
fn find_items(app: &mut RustGisApp) {
    keep_preset_sort_for_its_api(&mut app.ui.stac);
    let st = &app.ui.stac;
    // A catalog's children still coming don't hold a search up.
    if st.walk.is_some() || st.preparing.is_some() || st.waiting.as_ref().is_some_and(|(a, _)| !matches!(a, Asked::Children(_))) {
        return;
    }
    // A file or selection still being read would be missing from the area searched, so wait for it.
    if st.loading.is_some() {
        app.ui.stac.error = Some(tl!("The search area is still loading: search when it has finished").to_string());
        return;
    }
    let is_static = catalog::is_static(&st.api);
    let mut p = match search_params(st, is_static) {
        Ok(p) => p,
        Err(e) => {
            app.ui.stac.error = Some(e);
            return;
        }
    };
    let buffer = st.buffer.trim().to_string();
    if !st.parts.is_empty() {
        // The area (merging, widening) takes as long as its shapes are many and detailed, so it is
        // made on a thread of its own and the search is sent with it when it lands.
        let metres = if buffer.is_empty() {
            Ok(0.0)
        } else {
            match buffer.parse::<f64>() {
                Ok(d) if d.is_finite() && d >= 0.0 => {
                    rustgis_engine::cmd::stac_area::buffer_metres(d, Some(app.forms.distance_units.as_str()))
                }
                _ => Err(tl!("The distance to widen by is a number, 0 or more").to_string()),
            }
        };
        let geometries = st.parts.clone();
        match metres {
            Ok(metres) => start_preparing(app, geometries, metres, p, is_static),
            Err(e) => app.ui.stac.error = Some(e),
        }
        return;
    }
    if st.use_view {
        p["bbox"] = json!("view");
    }
    send_search(app, p, is_static);
}

/// An area being made for a search, and the search to send with it, built for `api` (the one
/// the dialog was on): the API picker stays usable meanwhile, and the search (its collection and
/// sort are that API's) must not go to another.
struct Preparing {
    slot: Slot<Result<Option<Prepared>, String>>,
    params: serde_json::Value,
    is_static: bool,
    api: String,
}

/// Makes the search area of `geometries` widened by `metres` on a thread of its own (here and now
/// in the browser, which has one), then sends the search `params` describes with it.
fn start_preparing(app: &mut RustGisApp, geometries: Vec<Geometry>, metres: f64, params: serde_json::Value, is_static: bool) {
    let job = move || rustgis_engine::cmd::stac_area::prepare(&geometries, metres);
    let api = app.ui.stac.api.trim().to_string();
    if cfg!(target_arch = "wasm32") {
        land_prepared(app, job(), params, is_static, &api);
        return;
    }
    let slot: Slot<Result<Option<Prepared>, String>> = Slot::default();
    let out = slot.clone();
    let spawned = std::thread::Builder::new().name("stac-search-area".into()).spawn(move || {
        let answer = job();
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
    });
    match spawned {
        Ok(_) => {
            app.ui.stac.error = None;
            app.ui.stac.info = Some(tl!("Making the search area…").to_string());
            app.ui.stac.preparing = Some(Preparing { slot, params, is_static, api });
        }
        Err(e) => app.ui.stac.error = Some(i18n::fmt("Couldn't start the search: {e}", &[("e", &e.to_string())])),
    }
}

fn poll_preparing(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(preparing) = &app.ui.stac.preparing else { return };
    let Some(answer) = preparing.slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some(preparing) = app.ui.stac.preparing.take() else { return };
    land_prepared(app, answer, preparing.params, preparing.is_static, &preparing.api);
    ctx.request_repaint();
}

/// The area made: what it left out is said, and the search goes with it, to the API it was built
/// for (`api`): if the dialog is on another now, the search is dropped and that is said.
fn land_prepared(
    app: &mut RustGisApp,
    answer: Result<Option<Prepared>, String>,
    mut params: serde_json::Value,
    is_static: bool,
    api: &str,
) {
    if app.ui.stac.api.trim() != api {
        app.ui.stac.error = None;
        app.ui.stac.info = Some(tl!("The API changed while the search area was being made: press Find Items to search it").to_string());
        return;
    }
    match answer {
        Ok(Some(prepared)) => {
            for n in prepared.notices {
                app.session.warn(n);
            }
            params["intersects"] = stac::area::to_geojson(&prepared.area.geometry);
            send_search(app, params, is_static);
        }
        Ok(None) => app.ui.stac.error = Some(tl!("The search area has no point, line or polygon to search with").to_string()),
        Err(e) => app.ui.stac.error = Some(e),
    }
}

/// Sends the search `p` describes: a walk of a static catalog, or an API's search.
fn send_search(app: &mut RustGisApp, p: serde_json::Value, is_static: bool) {
    // The shape of the selection (if asked) replaces the box; what merging leaves out is a notice.
    let search = rustgis_engine::cmd::stac::search_with_area(&mut app.session, &p).map(|(s, _)| s);
    // The area a cube is asked over: the shape's extent when it has one a cube can use, not a
    // point, a line along a parallel or a box split at the dateline (the items' extent then).
    let usable = |b: [f64; 4]| (b[2] > b[0] && b[3] > b[1] && b[2] - b[0] < 360.0).then_some(b);
    app.ui.stac.asked_area =
        search.as_ref().ok().and_then(|s| s.bbox.or_else(|| s.intersects.as_ref().and_then(|g| usable(stac::area::Shape::new(g).bbox()))));
    app.ui.stac.error = None;
    if is_static {
        let st = &app.ui.stac;
        let root = if st.node.is_empty() { st.api.trim().to_string() } else { st.node.clone() };
        let ancestor_public = !st.node.is_empty() && st.trail_reach.iter().flatten().any(|r| *r == Reach::Public);
        let ceiling = if st.api_from_document || ancestor_public { Reach::Public } else { Reach::Local };
        let walk = address::check_reach(&root, ceiling)
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
    // A shape goes in the body of a POST, which has no limit that matters; a box and the rest in
    // the address, as before.
    let api = stac::api(Some(app.ui.stac.api.as_str()));
    let request = api.and_then(|api| {
        let s = search?;
        if s.intersects.is_some() {
            // The same search as a GET, when its shape fits in an address.
            let get = stac::search_url(&api, &s).ok();
            Ok((stac::search_post_url(&api), Some(stac::search_body(&s)?.to_string()), get))
        } else {
            Ok((stac::search_url(&api, &s)?, None, None))
        }
    });
    match request {
        Ok((url, body, get)) => {
            let slot = match body {
                Some(body) => post_service(app, &url, body),
                None => fetch_service(app, &url, false),
            };
            app.ui.stac.asked_sort = p["sortby"].as_str().map(str::to_string);
            app.ui.stac.waiting = Some((Asked::Search(url, get), slot));
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
    // The band combination picked on a card: its item's place and the combination's id.
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut bands: Option<(usize, &'static str)> = None;
    // The spectral index picked on a card, the same way.
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut index: Option<(usize, &'static str)> = None;
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
                            // The band combinations this item's files can make (the browser has no
                            // thread to read several files on).
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                // Read once for both menus: this runs every frame, for every card.
                                let all = stac::bands::bands(item);
                                // The commands get the item as JSON, without where it came from: an item from a local or
                                // private network would be refused there, so they are not offered for it.
                                let public = item.origin_reach == Reach::Public;
                                let offered = if public { combinations_offered(&all) } else { Vec::new() };
                                if !offered.is_empty() {
                                    ui.menu_button(tl!("Bands"), |ui| {
                                        for (id, name) in &offered {
                                            if ui.button(menu_name(name)).clicked() {
                                                bands = Some((i, *id));
                                                ui.close();
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(tl!("Add the item's bands as one image, for the part of the map in view"));
                                }
                                let indices = if public { indices_offered(&all) } else { Vec::new() };
                                if !public && !all.is_empty() {
                                    ui.weak(tl!("Bands: public items only")).on_hover_text(tl!(
                                        "This item is from a local or private network, whose files Bands and Index don't read yet"
                                    ));
                                }
                                if !indices.is_empty() {
                                    ui.menu_button(tl!("Index"), |ui| {
                                        for (id, name) in &indices {
                                            if ui.button(menu_name(name)).clicked() {
                                                index = Some((i, *id));
                                                ui.close();
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(tl!("Add a spectral index of the item's bands, for the part of the map in view"));
                                }
                                // The item's bands were cut (too many to keep): a combination or index whose
                                // band is past the cut is missing from the menus, so say why where it is looked for.
                                // Worked out once per item (it looks at every asset), then kept for the frames after.
                                let note_id = ui.scope_id().with(("stac.cut_note", &item.collection, &item.id));
                                let cut = ui.data_mut(|d| d.get_temp_mut_or_insert_with(note_id, || stac::bands::cut_note(item)).clone());
                                if let Some(cut) = cut {
                                    ui.weak(tl!("Bands left out")).on_hover_text(cut);
                                }
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
    if let Some((i, id)) = bands {
        let Some(item) = app.ui.stac.items.get(i).cloned() else { return };
        add_item_bands(app, &item, id);
    }
    if let Some((i, id)) = index {
        let Some(item) = app.ui.stac.items.get(i).cloned() else { return };
        add_item_index(app, &item, id);
    }
    if let Some(i) = add {
        let Some(item) = app.ui.stac.items.get(i).cloned() else { return };
        let key = app.ui.stac.chosen.get(i).cloned().unwrap_or_default();
        add_item_asset(app, &item, Some(key.as_str()).filter(|k| !k.is_empty()));
    }
}

/// The band combinations an item's files can make, as (id, name): only those whose three bands it
/// has ([`stac::bands::offered`]), given the item's bands ([`stac::bands::bands`]).
#[cfg(not(target_arch = "wasm32"))]
fn combinations_offered(bands: &[stac::bands::Band]) -> Vec<(&'static str, &'static str)> {
    stac::bands::offered(bands).into_iter().map(|c| (c.id, c.name)).collect()
}

/// A combination's or index's name (a literal in `stac::bands`) in the current language. Each is
/// written out as a `tl!` call here so the catalogs list it; a name not in the list (a new one
/// added there first) shows in English until it is.
#[cfg(not(target_arch = "wasm32"))]
fn menu_name(name: &str) -> &str {
    match name {
        "True color" => tl!("True color"),
        "Color infrared" => tl!("Color infrared"),
        "Shortwave infrared" => tl!("Shortwave infrared"),
        "False color urban" => tl!("False color urban"),
        "Agriculture" => tl!("Agriculture"),
        "Geology" => tl!("Geology"),
        "Bathymetric" => tl!("Bathymetric"),
        "NDVI (vegetation)" => tl!("NDVI (vegetation)"),
        "NDWI (water)" => tl!("NDWI (water)"),
        "SAVI (vegetation over soil)" => tl!("SAVI (vegetation over soil)"),
        "NBR (burn ratio)" => tl!("NBR (burn ratio)"),
        "NDBI (built-up)" => tl!("NDBI (built-up)"),
        "EVI (enhanced vegetation)" => tl!("EVI (enhanced vegetation)"),
        other => tl!(other),
    }
}

/// The spectral indices an item's files can make, as (id, name): only those whose bands it has
/// ([`stac::bands::available_indices`]), given the item's bands.
#[cfg(not(target_arch = "wasm32"))]
fn indices_offered(bands: &[stac::bands::Band]) -> Vec<(&'static str, &'static str)> {
    stac::bands::available_indices(bands).into_iter().map(|i| (i.id, i.name)).collect()
}

/// The Workflows section that shows a band combination's progress and Cancel while it is read: one
/// no extension owns, so it opens with the Imagery extension off too (its Indices section would not).
#[cfg(not(target_arch = "wasm32"))]
const BANDS_SECTION: &str = "rasterFunctions";

/// Adds an item's band combination as one image for the part of the map in view: `stac.addBands`
/// reads the three files' windows on a thread of its own, with progress and Cancel in the Workflows
/// pane, and the layer lands when they are read.
#[cfg(not(target_arch = "wasm32"))]
fn add_item_bands(app: &mut RustGisApp, item: &Item, combination: &str) {
    let p = json!({"item": stac::item_json(item), "combination": combination, "bbox": "view"});
    crate::analysispane::run_elsewhere(app, BANDS_SECTION, "stac.addBands", p);
}

/// Adds a spectral index of an item's bands as one image for the part of the map in view:
/// `stac.addIndex` reads the files' windows on a thread of its own, with progress and Cancel in the
/// Workflows pane, and the layer lands when they are read.
#[cfg(not(target_arch = "wasm32"))]
fn add_item_index(app: &mut RustGisApp, item: &Item, index: &str) {
    let p = json!({"item": stac::item_json(item), "index": index, "bbox": "view"});
    crate::analysispane::run_elsewhere(app, BANDS_SECTION, "stac.addIndex", p);
}

/// The browser makes no band combination yet (its menu isn't shown).
#[cfg(target_arch = "wasm32")]
fn add_item_bands(app: &mut RustGisApp, _item: &Item, _combination: &str) {
    app.ui.stac.error = Some(tl!("Band combinations are made on the desktop for now").to_string());
}

/// The browser makes no index yet (its menu isn't shown).
#[cfg(target_arch = "wasm32")]
fn add_item_index(app: &mut RustGisApp, _item: &Item, _index: &str) {
    app.ui.stac.error = Some(tl!("Indices are made on the desktop for now").to_string());
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
    fn switched_off_dates_and_cloud_limit_are_not_sent() {
        let mut st = State { from: "2024-06-01".into(), to: "2024-06-30".into(), ..State::default() };
        // A fresh dialog leaves the dates out and sends the cloud limit.
        assert_eq!((st.dates(), st.cloud_limit()), (("", ""), Ok(Some(20.0))));
        (st.use_dates, st.use_cloud) = (true, true);
        assert_eq!((st.dates(), st.cloud_limit()), (("2024-06-01", "2024-06-30"), Ok(Some(20.0))));
        (st.use_dates, st.use_cloud) = (false, false);
        // The typed values stay in their boxes, but none goes with the search.
        assert_eq!((st.dates(), st.cloud_limit()), (("", ""), Ok(None)));
        assert_eq!((st.from.as_str(), st.max_cloud.as_str()), ("2024-06-01", "20"));
        // Switched off, even a value that is not a number is left alone.
        st.max_cloud = "2O".into();
        assert_eq!(st.cloud_limit(), Ok(None));
    }

    #[test]
    fn a_ticked_blank_box_sends_nothing_and_a_bad_cloud_limit_is_refused() {
        let mut st = State { use_dates: true, use_cloud: true, ..State::default() };
        // Ticked and blank: no filter (any cloud cover, any date).
        (st.from, st.to, st.max_cloud) = (String::new(), "  ".into(), " ".into());
        assert_eq!((st.dates(), st.cloud_limit()), (("", ""), Ok(None)));
        // A number from 0 to 100 goes, spaces around it don't matter.
        for (typed, limit) in [("0", 0.0), (" 7.5 ", 7.5), ("100", 100.0)] {
            st.max_cloud = typed.into();
            assert_eq!(st.cloud_limit(), Ok(Some(limit)), "{typed}");
        }
        // Anything else is an error, not a search without the limit.
        for typed in ["2O", "-5", "100.1", "500", "NaN", "inf", "20%"] {
            st.max_cloud = typed.into();
            assert!(st.cloud_limit().is_err(), "{typed}");
        }
        // The search is not sent with it, and the cube refuses it too.
        let mut app = RustGisApp::with_sample();
        app.ui.stac.max_cloud = "2O".into();
        find_items(&mut app);
        let said = app.ui.stac.error.clone().unwrap_or_default();
        assert!(said.contains("0 to 100"), "{said}");
        assert!(app.ui.stac.waiting.is_none() && app.ui.stac.preparing.is_none());
        #[cfg(not(target_arch = "wasm32"))]
        assert!(cube_params(&app.ui.stac, "2024-01-01".into(), "2024-02-01".into(), None).is_err());
    }

    /// The request a search is sent with carries only the filters that are switched on and filled.
    #[test]
    fn the_search_request_carries_only_the_filters_that_are_on_and_filled() {
        let mut st = State { from: "2024-06-01".into(), to: "2024-06-30".into(), ..State::default() };
        // As the dialog opens: no dates, the cloud limit.
        let p = search_params(&st, false).unwrap_or_default();
        assert_eq!((p.get("datetime"), p.get("maxCloud")), (None, Some(&json!(20.0))));
        // Both on: the range and the limit.
        (st.use_dates, st.use_cloud) = (true, true);
        let p = search_params(&st, false).unwrap_or_default();
        assert_eq!((p.get("datetime"), p.get("maxCloud")), (Some(&json!("2024-06-01/2024-06-30")), Some(&json!(20.0))));
        // Both off: neither, though both are still typed.
        (st.use_dates, st.use_cloud) = (false, false);
        let p = search_params(&st, false).unwrap_or_default();
        assert_eq!((p.get("datetime"), p.get("maxCloud")), (None, None));
        // On and blank: neither.
        (st.use_dates, st.use_cloud) = (true, true);
        (st.from, st.to, st.max_cloud) = (String::new(), String::new(), String::new());
        let p = search_params(&st, false).unwrap_or_default();
        assert_eq!((p.get("datetime"), p.get("maxCloud")), (None, None));
        // A limit that is not a number refuses the request.
        st.max_cloud = "2O".into();
        assert!(search_params(&st, false).is_err());
        // Switched off, the same text is not looked at.
        st.use_cloud = false;
        assert!(search_params(&st, false).is_ok());
    }

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
            opened: app.session.opened,
            token_url: "http://127.0.0.1:9/token".into(),
            href: "http://127.0.0.1:9/points.geojson".into(),
            name: "Original asset".into(),
            connection: selected_connection(&app),
            from: api.into(),
            public: false,
        };
        let slot = Slot::default();
        *slot.lock().unwrap() =
            Some(Ok(cache::Fetched { bytes: br#"{"token":"sig=snapshot"}"#.to_vec(), origin: Origin::named("http://127.0.0.1:9/token") }));
        app.ui.stac.waiting = Some((asked, slot));
        app.ui.stac.connection = two["id"].as_str().map(str::to_string);
        poll(&mut app, &egui::Context::default());
        let params = &app.ui.downloads.last().unwrap().params;
        assert_eq!(params["connection"], one["id"], "token completion must not re-read the picker");
        assert_ne!(params["connection"], two["id"]);
    }

    #[test]
    fn item_link_and_token_responses_cannot_add_to_a_replacement_project() {
        let _turn = crate::fetching::turn();
        for (case, (token, ready)) in [(false, false), (false, true), (true, false), (true, true)].into_iter().enumerate() {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sent = seen.clone();
            cache::set_fetcher(move |url, done| {
                sent.lock().unwrap().push(url);
                done(Err("unexpected stale asset request".into()));
            });
            let mut app = RustGisApp::with_sample();
            let opened = app.session.opened;
            let from = format!("https://late-project{case}.corp.example/item.json");
            let href = if token {
                format!("https://late-project{case}.blob.core.windows.net/assets/points.geojson")
            } else {
                format!("http://10.0.0.5/late-project{case}.geojson")
            };
            let token_url = token.then(|| stac::sas_token_url(&href).unwrap());
            let asked = match &token_url {
                Some(url) => Asked::Token {
                    token_url: url.clone(),
                    href: href.clone(),
                    name: "Old asset".into(),
                    connection: None,
                    from: from.clone(),
                    public: true,
                    opened,
                },
                None => Asked::Link { url: from.clone(), opened },
            };
            let bytes = if token {
                br#"{"token":"sig=late-project-synthetic"}"#.to_vec()
            } else {
                json!({"type":"Feature","id":"old","geometry":null,"properties":{},
                    "assets":{"data":{"href":href,"type":"application/geo+json","roles":["data"]}},"links":[]})
                .to_string()
                .into_bytes()
            };
            let origin = Origin::connected(&from, "10.0.0.5".parse().unwrap()).unwrap();
            let answer: Answer = Slot::default();
            if ready {
                *answer.lock().unwrap() = Some(Ok(cache::Fetched { bytes: bytes.clone(), origin: origin.clone() }));
            }
            app.ui.stac.waiting = Some((asked, answer.clone()));
            app.session.run("project.new", &json!({})).unwrap();
            let layers = app.session.map().layers.len();
            let mark = app.session.notice_mark();
            let ctx = egui::Context::default();
            poll(&mut app, &ctx);
            assert!(app.ui.stac.waiting.is_none());
            if !ready {
                *answer.lock().unwrap() = Some(Ok(cache::Fetched { bytes, origin }));
            }
            poll(&mut app, &ctx);
            assert!(seen.lock().unwrap().is_empty());
            assert!(app.ui.downloads.is_empty() && app.ui.cog_probes.is_empty());
            assert_eq!(app.session.map().layers.len(), layers);
            let params = json!({"url":href,"from":from,"fromDocument":false});
            assert!(rustgis_engine::cmd::layer::request_origin_for(&app.session, &params).unwrap().1);
            if let Some(url) = token_url {
                assert!(stac::cached_token(&url).is_none());
            }
            assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.text.contains("project was closed")).count(), 1);
        }
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
    fn the_sort_picked_is_sent_to_an_api_only() {
        let mut st = State::default();
        assert_eq!(sort_of(&st, false), None);
        for (v, _) in SORTS.iter().filter(|(v, _)| !v.is_empty()) {
            st.sortby = format!(" {v} ");
            assert_eq!(sort_of(&st, false), Some(*v));
            assert_eq!(sort_of(&st, true), None);
        }
        // Newest first is a field the search can sort on.
        assert!(SORTS.iter().any(|(v, _)| *v == "-datetime") && SORTS.iter().any(|(v, _)| *v == "datetime"));
        // An API that ignored the sort is said so (and only then): the items tell.
        let page = |dates: [&str; 3]| {
            let f: Vec<_> = dates
                .iter()
                .enumerate()
                .map(|(n, d)| json!({"type": "Feature", "id": format!("i{n}"), "bbox": [0, 0, 1, 1], "properties": {"datetime": d}}))
                .collect();
            stac::parse_items(json!({"type": "FeatureCollection", "features": f}).to_string().as_bytes(), "https://x.org/search").unwrap().0
        };
        let sorted = page(["2024-06-03T00:00:00Z", "2024-06-02T00:00:00Z", "2024-06-01T00:00:00Z"]);
        assert_eq!(sort_ignored_note(&sorted, Some("-datetime")), None);
        assert_eq!(sort_ignored_note(&sorted, None), None);
        assert!(sort_ignored_note(&sorted, Some("datetime")).is_some_and(|n| n.contains("datetime")));
        // A sort the items can't show is said so, and only then: not for one the items show, no
        // sort, or a page of one.
        assert!(sort_unchecked_note(&sorted, Some("platform")).is_some_and(|n| n.contains("platform")));
        assert_eq!(sort_unchecked_note(&sorted, Some("-datetime")), None);
        assert_eq!(sort_unchecked_note(&sorted, Some("datetime")), None, "ignored is the other note's to say");
        assert_eq!(sort_unchecked_note(&sorted, None), None);
        assert_eq!(sort_unchecked_note(&sorted[..1], Some("platform")), None);
    }

    #[test]
    fn opening_stac_search_while_an_area_is_made_keeps_the_presets_sort() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), (PRESET_SORT, true));
        // The search is being made with the preset's sort, and the ribbon's STAC Search is opened.
        let api = app.ui.stac.api.clone();
        app.ui.stac.preparing = Some(Preparing { slot: Slot::default(), params: json!({"sortby": PRESET_SORT}), is_static: false, api });
        open_preset(&mut app, None, &serde_json::Value::Null).unwrap();
        assert!(app.ui.stac.preset.is_none());
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), (PRESET_SORT, true), "it goes out with that sort");
        assert!(keeps_preset_sort(&app.ui.stac), "and the dropdown keeps reading it");
        keep_preset_sort_for_its_api(&mut app.ui.stac);
        assert_eq!(app.ui.stac.sortby, PRESET_SORT);
        // Once the area has landed the preset is gone, and so is its sort.
        app.ui.stac.preparing = None;
        assert!(!keeps_preset_sort(&app.ui.stac));
        keep_preset_sort_for_its_api(&mut app.ui.stac);
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), ("", false));
    }

    /// An item with a file a colour band, at an address that never resolves (`.invalid`).
    fn scene(bands: &[&str]) -> Item {
        let assets: serde_json::Map<String, serde_json::Value> = bands
            .iter()
            .map(|b| {
                let a = json!({"href": format!("https://scene.invalid/{b}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                    "eo:bands": [{"common_name": b}]});
                (b.to_string(), a)
            })
            .collect();
        let feature = json!({"type": "Feature", "id": "scene", "bbox": [0, 0, 1, 1], "properties": {"datetime": "2024-06-01T00:00:00Z"}, "assets": assets});
        stac::item_from_json(&feature, None).unwrap()
    }

    #[test]
    fn a_card_offers_the_band_combinations_its_item_can_make_and_adds_one_in_the_background() {
        let mut app = RustGisApp::with_sample();
        assert_eq!(combinations_offered(&stac::bands::bands(&scene(&["red", "green", "blue"]))), [("trueColor", "True color")]);
        let infrared = combinations_offered(&stac::bands::bands(&scene(&["red", "green", "blue", "nir"])));
        assert_eq!(infrared.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["trueColor", "colorInfrared"]);
        assert!(combinations_offered(&stac::bands::bands(&scene(&["red"]))).is_empty(), "no three bands, no menu");
        // Picking one starts the command on a thread, without holding the window, whether or not the
        // Imagery extension is on: its progress is shown in a section no extension owns.
        assert!(crate::analysispane::section_extension(BANDS_SECTION).is_none());
        app.run("extension.disable", json!({"id": "imagery"})).unwrap();
        assert!(crate::analysispane::section_blocked(&app.session, "indices").is_some(), "the Indices section is what was refused");
        let layers = app.session.map().layers.len();
        add_item_bands(&mut app, &scene(&["red", "green", "blue"]), "trueColor");
        assert!(app.workflow.job.is_some() && app.workflow.error.is_none(), "{:?}", app.workflow.error);
        // The files are never found: it ends as an error, and no layer is added.
        // A failed lookup of a name that never resolves can take a runner long to give up: the cap (about a
        // minute) only guards against a hang.
        for _ in 0..6000 {
            if !crate::analysispane::land_job(&mut app) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: the read runs on a thread of its own and lands through land_job, which gives no signal to wait on
        }
        assert!(app.workflow.job.is_none());
        assert!(app.workflow.error.is_some(), "the read's failure is said");
        assert_eq!(app.session.map().layers.len(), layers);
    }

    #[test]
    fn a_card_offers_the_indices_its_item_can_make_and_adds_one_in_the_background() {
        let mut app = RustGisApp::with_sample();
        assert!(indices_offered(&stac::bands::bands(&scene(&["red", "green", "blue"]))).is_empty(), "no near infrared, no index");
        let some = indices_offered(&stac::bands::bands(&scene(&["red", "nir"])));
        assert_eq!(some.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["ndvi", "savi"]);
        let layers = app.session.map().layers.len();
        add_item_index(&mut app, &scene(&["red", "nir"]), "ndvi");
        assert!(app.workflow.job.is_some() && app.workflow.error.is_none(), "{:?}", app.workflow.error);
        // The files are never found: it ends as an error, and no layer is added.
        // A failed lookup of a name that never resolves can take a runner long to give up: the cap (about a
        // minute) only guards against a hang.
        for _ in 0..6000 {
            if !crate::analysispane::land_job(&mut app) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: the read runs on a thread of its own and lands through land_job, which gives no signal to wait on
        }
        assert!(app.workflow.job.is_none() && app.workflow.error.is_some(), "the read's failure is said");
        assert_eq!(app.session.map().layers.len(), layers);
    }

    #[test]
    fn opening_stac_search_while_a_search_is_in_flight_keeps_the_presets_sort() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        // The search went out with the preset's sort and is not answered yet.
        app.ui.stac.waiting = Some((Asked::Search("https://x.org/search".into(), None), Slot::default()));
        open_preset(&mut app, None, &serde_json::Value::Null).unwrap();
        assert!(app.ui.stac.preset.is_none());
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), (PRESET_SORT, true), "it went out with that sort");
        assert!(keeps_preset_sort(&app.ui.stac), "and the dropdown keeps reading it");
        // Answered: the preset is gone, and so is its sort.
        app.ui.stac.waiting = None;
        keep_preset_sort_for_its_api(&mut app.ui.stac);
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), ("", false));
        // Any other wait (a listing of collections) isn't a search with that sort.
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        app.ui.stac.waiting = Some((Asked::Collections("https://x.org".into()), Slot::default()));
        open_preset(&mut app, None, &serde_json::Value::Null).unwrap();
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), ("", false));
    }

    #[test]
    fn a_presets_sort_stays_with_the_presets_api() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        assert_eq!(app.ui.stac.sortby, PRESET_SORT);
        // Staying on the preset's API keeps it; leaving for another drops it.
        let api = app.ui.stac.api.clone();
        switch_api(&mut app.ui.stac, &api);
        assert_eq!(app.ui.stac.sortby, PRESET_SORT);
        switch_api(&mut app.ui.stac, "https://example.org/stac/v1");
        assert_eq!(app.ui.stac.sortby, "");
        // A sort the user picked on another API is theirs: it stays when the API changes again.
        app.ui.stac.sortby = "-datetime".into();
        switch_api(&mut app.ui.stac, "https://other.example.org/stac/v1");
        assert_eq!(app.ui.stac.sortby, "-datetime");
        // The same sort picked by hand on the preset is the user's: it stays (a click in the
        // dropdown clears `sort_is_default`).
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        (app.ui.stac.sortby, app.ui.stac.sort_is_default) = (PRESET_SORT.into(), false);
        switch_api(&mut app.ui.stac, "https://example.org/stac/v1");
        assert_eq!(app.ui.stac.sortby, PRESET_SORT);
        // A sort picked on the USGS LiDAR preset (which sets none) stays too.
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.usgsLidar", serde_json::Value::Null).unwrap();
        app.ui.stac.sortby = PRESET_SORT.into();
        switch_api(&mut app.ui.stac, "https://example.org/stac/v1");
        assert_eq!(app.ui.stac.sortby, PRESET_SORT);
    }

    /// A search whose area was being made when another API was picked is not sent to that API
    /// (its collection and sort were the first one's): it is dropped, and the pane says so.
    #[test]
    fn an_area_made_for_one_api_is_not_searched_on_another() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        let made_for = app.ui.stac.api.clone();
        let params = json!({"limit": 50, "collections": "sentinel-2-c1-l2a", "sortby": PRESET_SORT});
        let area = rustgis_engine::cmd::stac_area::prepare(&[rectangle([5.0, 45.0, 6.0, 46.0])], 0.0).unwrap();
        // Another API was picked while the area was being made: nothing is sent, and it is said.
        switch_api(&mut app.ui.stac, "https://example.org/stac/v1");
        land_prepared(&mut app, Ok(area), params, false, &made_for);
        assert!(app.ui.stac.waiting.is_none(), "the search went to the other API");
        assert!(app.ui.stac.error.is_none());
        assert!(app.ui.stac.info.as_deref().is_some_and(|i| i.contains("API changed")), "{:?}", app.ui.stac.info);
    }

    /// Typing in the API field doesn't touch the sort: deleting a character of the preset's
    /// address and typing it back keeps the preset's default. It is dropped when a search is sent
    /// (or another API is picked) while the dialog is on another API.
    #[test]
    fn editing_the_address_and_retyping_it_keeps_the_presets_sort() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        let address = app.ui.stac.api.clone();
        // What the text field does on each keystroke: change the text and drop the listing.
        app.ui.stac.api.pop();
        drop_listing(&mut app.ui.stac);
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), (PRESET_SORT, true));
        app.ui.stac.api = address;
        drop_listing(&mut app.ui.stac);
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), (PRESET_SORT, true));
        assert_eq!(shown_preset(&app.ui.stac), Some(Preset::Sentinel2));
        // Left on another address, the check made when a search is sent drops it.
        app.ui.stac.api = "https://example.org/stac/v1".into();
        keep_preset_sort_for_its_api(&mut app.ui.stac);
        assert_eq!((app.ui.stac.sortby.as_str(), app.ui.stac.sort_is_default), ("", false));
    }

    /// Reopening STAC Search from the ribbon (no preset) takes the preset's default sort with it,
    /// so it can't go on to another API typed in afterwards.
    #[test]
    fn reopening_the_dialog_without_a_preset_drops_the_presets_sort() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.stac.sentinel2", serde_json::Value::Null).unwrap();
        assert_eq!(app.ui.stac.sortby, PRESET_SORT);
        app.run("ui.stac", serde_json::Value::Null).unwrap();
        assert_eq!(app.ui.stac.sortby, "");
        switch_api(&mut app.ui.stac, "https://example.org/stac/v1");
        assert_eq!(app.ui.stac.sortby, "");
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
        (app.ui.stac.from, app.ui.stac.use_dates) = ("soon".into(), true);
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
        *late.lock().unwrap() = Some(Ok(cache::Fetched {
            bytes: br#"{"collections":[{"id":"old","title":"Old"}]}"#.to_vec(),
            origin: Origin::named("https://one.example/stac"),
        }));
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

        app.ui.stac.waiting =
            Some((Asked::Link { url: "https://two.example/item.json".into(), opened: app.session.opened }, Slot::default()));
        switch_api(&mut app.ui.stac, "https://three.example/stac");
        assert!(matches!(app.ui.stac.waiting, Some((Asked::Link { .. }, _))), "a link already asked for goes on");
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

    fn at(x: f64, y: f64) -> Coord {
        Coord::new(x, y)
    }

    #[test]
    fn a_drawn_shape_joins_the_search_area_and_not_a_graphics_layer() {
        let mut app = RustGisApp::with_sample();
        let layers = app.session.map().layers.len();
        // Nothing is being drawn for the area: the graphic is the usual one.
        assert_eq!(take_drawn(&mut app, "polygon", &[at(0.0, 0.0), at(1.0, 0.0), at(1.0, 1.0)]), None);
        draw(&mut app, "polygon");
        assert_eq!(app.ui.stac.drawing, Some("polygon"));
        assert!(matches!(app.session.tool, rustgis_engine::Tool::Graphic("polygon")));
        // Another shape's graphic is not taken while polygons are drawn.
        assert_eq!(take_drawn(&mut app, "line", &[at(0.0, 0.0), at(1.0, 1.0)]), None);
        assert_eq!(take_drawn(&mut app, "polygon", &[at(0.0, 0.0), at(1.0, 0.0), at(1.0, 1.0)]), Some(true));
        assert!(
            matches!(app.ui.stac.parts.as_slice(), [Geometry::Polygon(p)] if p.first().and_then(|r| r.first()).is_some_and(|ring| ring.len() == 4))
        );
        assert_eq!(app.ui.stac.drawing, None);
        assert_eq!(app.session.map().layers.len(), layers, "no graphics layer was made");
    }

    #[test]
    fn each_shape_is_drawn_from_its_own_points() {
        let mut app = RustGisApp::with_sample();
        for (shape, pts, kind) in [
            ("point", vec![at(5.0, 6.0)], "point"),
            ("line", vec![at(0.0, 0.0), at(1.0, 1.0)], "line"),
            ("rectangle", vec![at(3.0, 4.0), at(1.0, 2.0)], "polygon"),
        ] {
            draw(&mut app, shape);
            assert_eq!(take_drawn(&mut app, shape, &pts), Some(true), "{shape}");
            let made = app.ui.stac.parts.last();
            assert!(
                match kind {
                    "point" => matches!(made, Some(Geometry::Point(c)) if *c == at(5.0, 6.0)),
                    "line" => matches!(made, Some(Geometry::Line(l)) if l.first().is_some_and(|part| part.len() == 2)),
                    _ => made == Some(&rectangle([1.0, 2.0, 3.0, 4.0])),
                },
                "{shape}: {made:?}"
            );
        }
        assert_eq!(app.ui.stac.parts.len(), 3);
    }

    #[test]
    fn a_shape_too_small_to_search_with_keeps_the_tool_for_another_try() {
        let mut app = RustGisApp::with_sample();
        for (shape, pts) in [
            ("rectangle", vec![at(2.0, 2.0), at(2.0, 2.0)]),
            ("rectangle", vec![at(2.0, 2.0)]),
            ("line", vec![at(0.0, 0.0)]),
            ("polygon", vec![at(0.0, 0.0), at(1.0, 1.0)]),
            ("point", vec![]),
            ("line", vec![at(f64::NAN, 0.0), at(1.0, f64::INFINITY)]),
        ] {
            draw(&mut app, shape);
            assert_eq!(take_drawn(&mut app, shape, &pts), Some(false), "{shape} {pts:?}");
            assert_eq!(app.ui.stac.drawing, Some(shape));
            assert!(app.ui.stac.parts.is_empty());
            clear_area(&mut app);
        }
    }

    #[test]
    fn a_click_on_a_repeated_world_is_the_same_place_on_the_globe() {
        let mut app = RustGisApp::with_sample();
        draw(&mut app, "point");
        assert_eq!(take_drawn(&mut app, "point", &[at(190.0, 95.0)]), Some(true));
        assert!(matches!(app.ui.stac.parts.as_slice(), [Geometry::Point(c)] if (c.x + 170.0).abs() < 1e-9 && c.y == 90.0));
    }

    #[test]
    fn a_search_waits_for_an_area_that_is_still_loading() {
        let mut app = RustGisApp::with_sample();
        app.ui.stac.api = "https://example.org/stac/v1".into();
        app.ui.stac.parts.push(rectangle([0.0, 0.0, 1.0, 1.0]));
        let slot: Slot<Result<Loaded, String>> = Slot::default();
        app.ui.stac.loading = Some(("pts.geojson".into(), slot));
        find_items(&mut app);
        assert!(app.ui.stac.waiting.is_none(), "nothing is sent while the area loads");
        let said = app.ui.stac.error.clone().unwrap_or_default();
        assert!(said.contains("still loading"), "{said}");
        // Once it has landed, the same search starts (its area made first, off the frame), and the
        // message is gone.
        app.ui.stac.loading = None;
        app.ui.stac.error = None;
        find_items(&mut app);
        wait_for_the_area(&mut app);
        assert!(app.ui.stac.waiting.is_some() && app.ui.stac.error.is_none());
    }

    #[test]
    fn clear_empties_the_area_and_stops_drawing() {
        let mut app = RustGisApp::with_sample();
        app.ui.stac.parts.push(rectangle([0.0, 0.0, 1.0, 1.0]));
        draw(&mut app, "line");
        clear_area(&mut app);
        assert!(app.ui.stac.parts.is_empty() && app.ui.stac.drawing.is_none() && app.ui.stac.loading.is_none());
        assert!(matches!(app.session.tool, rustgis_engine::Tool::Explore));
        // Drawing again with the same button stops it.
        draw(&mut app, "line");
        draw(&mut app, "line");
        assert!(app.ui.stac.drawing.is_none() && matches!(app.session.tool, rustgis_engine::Tool::Explore));
    }

    #[test]
    fn picking_another_tool_stops_drawing_the_area() {
        let mut app = RustGisApp::with_sample();
        draw(&mut app, "point");
        app.run("tool.set", json!({"tool": "explore"})).unwrap();
        stop_stale_drawing(&mut app);
        assert_eq!(app.ui.stac.drawing, None);
    }

    #[test]
    fn closing_the_pane_stops_drawing_the_area_and_returns_to_explore() {
        let mut app = RustGisApp::with_sample();
        app.ui.right = Some(crate::RightPane::WebData);
        app.ui.web_data = "stac";
        draw(&mut app, "line");
        stop_stale_drawing(&mut app);
        assert_eq!(app.ui.stac.drawing, Some("line"), "the pane is shown");
        app.ui.right = None;
        stop_stale_drawing(&mut app);
        assert_eq!(app.ui.stac.drawing, None);
        assert!(matches!(app.session.tool, rustgis_engine::Tool::Explore));
    }

    #[test]
    fn a_kept_tool_goes_on_adding_shapes_to_the_area() {
        let mut app = RustGisApp::with_sample();
        app.session.keep_tool = true;
        draw(&mut app, "point");
        for x in [1.0, 2.0] {
            assert_eq!(take_drawn(&mut app, "point", &[at(x, 0.0)]), Some(true));
            assert_eq!(app.ui.stac.drawing, Some("point"));
        }
        assert_eq!(app.ui.stac.parts.len(), 2);
        app.session.keep_tool = false;
        assert_eq!(take_drawn(&mut app, "point", &[at(3.0, 0.0)]), Some(true));
        assert_eq!(app.ui.stac.drawing, None);
    }

    #[test]
    fn a_search_area_holds_a_bounded_number_of_parts_and_says_so() {
        let mut app = RustGisApp::with_sample();
        for i in 0..MAX_PARTS {
            push_part(&mut app, Geometry::Point(at(f64::from(i as u32 % 100), 0.0)));
        }
        assert_eq!(app.ui.stac.parts.len(), MAX_PARTS);
        assert!(app.ui.stac.error.is_none());
        push_part(&mut app, Geometry::Point(at(0.0, 0.0)));
        assert_eq!(app.ui.stac.parts.len(), MAX_PARTS);
        assert!(app.ui.stac.error.is_some());
    }

    #[test]
    fn a_buffer_that_is_not_a_number_stops_the_search_before_it_asks() {
        let mut app = RustGisApp::with_sample();
        app.ui.stac.parts.push(Geometry::Point(at(10.0, 45.0)));
        for bad in ["far", "NaN", "inf", "-5"] {
            app.ui.stac.buffer = bad.into();
            app.ui.stac.error = None;
            find_items(&mut app);
            assert!(app.ui.stac.error.is_some(), "{bad}");
            assert!(app.ui.stac.waiting.is_none(), "{bad}: nothing was asked of the server");
        }
    }

    #[test]
    fn selected_features_are_merged_on_a_thread_and_landed_with_their_notices() {
        let mut app = RustGisApp::with_sample();
        // Nothing selected: said, and nothing is started.
        add_selected(&mut app);
        assert!(app.ui.stac.error.is_some() && app.ui.stac.loading.is_none());
        app.ui.stac.error = None;
        // A mixed answer lands its notices, then its part.
        let answer = rustgis_engine::cmd::stac_area::merge_loaded(
            &[rectangle([0.0, 0.0, 1.0, 1.0]), Geometry::Point(at(5.0, 5.0))],
            Default::default(),
        );
        let before = app.session.notice_mark();
        land(&mut app, answer);
        assert_eq!(app.ui.stac.parts.len(), 1);
        assert!(app.session.notices_since(before).iter().any(|n| n.text.contains("lower dimension")));
        // A failed one lands its error and adds nothing.
        land(&mut app, Err("no such file".into()));
        assert_eq!(app.ui.stac.parts.len(), 1);
        assert_eq!(app.ui.stac.error.as_deref(), Some("no such file"));
    }

    #[test]
    fn a_refused_post_search_is_asked_again_in_the_address_when_it_fits() {
        let ctx = egui::Context::default();
        // A refused POST with a GET to fall back on asks it, once, and shows no error yet.
        let refused: Answer = Slot::new(std::sync::Mutex::new(Some(Err("HTTP 405".to_string()))));
        let mut app = RustGisApp::with_sample();
        app.ui.stac.waiting =
            Some((Asked::Search("http://127.0.0.1/search".into(), Some("http://127.0.0.1/search?limit=1".into())), refused));
        poll(&mut app, &ctx);
        assert!(app.ui.stac.error.is_none());
        assert!(matches!(&app.ui.stac.waiting, Some((Asked::Search(u, None), _)) if u == "http://127.0.0.1/search?limit=1"));
        // With no GET to try (a shape too long for an address), the refusal is shown.
        let refused: Answer = Slot::new(std::sync::Mutex::new(Some(Err("HTTP 405".to_string()))));
        app.ui.stac.waiting = Some((Asked::Search("http://127.0.0.1/search".into(), None), refused));
        poll(&mut app, &ctx);
        assert_eq!(app.ui.stac.error.as_deref(), Some("HTTP 405"));
        assert!(app.ui.stac.waiting.is_none());
    }

    #[test]
    fn a_post_search_with_a_next_link_says_more_match() {
        let ctx = egui::Context::default();
        let answer = |body: &str| -> Answer {
            Slot::new(std::sync::Mutex::new(Some(Ok(cache::Fetched {
                bytes: body.as_bytes().to_vec(),
                origin: Origin::named("http://127.0.0.1/search"),
            }))))
        };
        // A POST `next` link has no address, so the parser gives none; the pane still says so.
        let post_next = r#"{"type":"FeatureCollection","features":[],"links":[{"rel":"next","method":"POST","href":"http://127.0.0.1/search","body":{"token":"x"}}]}"#;
        let mut app = RustGisApp::with_sample();
        app.ui.stac.waiting = Some((Asked::Search("http://127.0.0.1/search".into(), None), answer(post_next)));
        poll(&mut app, &ctx);
        let said = app.ui.stac.info.clone().unwrap_or_default();
        assert!(said.contains("more match"), "{said}");
        // No next link and no count: nothing more is claimed.
        let all = r#"{"type":"FeatureCollection","features":[],"links":[]}"#;
        app.ui.stac.waiting = Some((Asked::Search("http://127.0.0.1/search".into(), None), answer(all)));
        poll(&mut app, &ctx);
        let said = app.ui.stac.info.clone().unwrap_or_default();
        assert!(!said.contains("more match"), "{said}");
    }

    #[test]
    fn a_next_link_the_reach_refuses_is_said_rather_than_read_as_more_matches() {
        let ctx = egui::Context::default();
        // A public document's next page on the local network is dropped by the parser: the pane says
        // so, and does not ask the user to narrow a search whose next page it won't load.
        let page = r#"{"type":"FeatureCollection","features":[],"links":[{"rel":"next","href":"http://127.0.0.1/search?page=2"}]}"#;
        let answer: Answer = Slot::new(std::sync::Mutex::new(Some(Ok(cache::Fetched {
            bytes: page.as_bytes().to_vec(),
            origin: Origin::named("https://example.com/search"),
        }))));
        let mut app = RustGisApp::with_sample();
        app.ui.stac.waiting = Some((Asked::Search("https://example.com/search".into(), None), answer));
        poll(&mut app, &ctx);
        let said = app.ui.stac.info.clone().unwrap_or_default();
        assert!(said.contains("refused"), "{said}");
        assert!(!said.contains("more match"), "{said}");
    }

    #[test]
    fn another_graphic_tool_stops_the_drawing_of_the_area() {
        let mut app = RustGisApp::with_sample();
        draw(&mut app, "polygon");
        assert_eq!(app.ui.stac.drawing, Some("polygon"));
        // The ribbon picks the line graphic: the polygon drawn next isn't the area's.
        app.run("tool.set", json!({"tool": "graphicLine"})).unwrap();
        stop_stale_drawing(&mut app);
        assert_eq!(app.ui.stac.drawing, None);
        let polygon = [Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0)];
        assert_eq!(take_drawn(&mut app, "polygon", &polygon), None);
        assert!(app.ui.stac.parts.is_empty());
    }

    /// Polls the pane until the area of a shape search has been made on its thread.
    fn wait_for_the_area(app: &mut RustGisApp) {
        let ctx = egui::Context::default();
        for _ in 0..500 {
            if app.ui.stac.preparing.is_none() {
                return;
            }
            poll_preparing(app, &ctx);
            std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: legacy polling for area preparation
        }
        panic!("the search area was not made in five seconds");
    }

    #[test]
    fn a_shape_search_makes_its_area_off_the_frame_and_then_sends() {
        let mut app = RustGisApp::with_sample();
        app.ui.stac.api = "http://127.0.0.1:1/stac".into();
        app.ui.stac.collection = "c".into();
        app.ui.stac.parts.push(rectangle([7.0, 46.0, 8.0, 47.0]));
        find_items(&mut app);
        // The click returned with the area still being made, and nothing sent yet.
        assert!(app.ui.stac.preparing.is_some() && app.ui.stac.waiting.is_none() && app.ui.stac.error.is_none());
        // A second click while it is made does nothing.
        find_items(&mut app);
        wait_for_the_area(&mut app);
        assert!(app.ui.stac.waiting.is_some() || app.ui.stac.error.is_some(), "the search went out");
        assert_eq!(app.ui.stac.asked_area, Some([7.0, 46.0, 8.0, 47.0]));
        // A point has no extent a cube could use: the items' extent is used then, as before.
        app.ui.stac.waiting = None;
        app.ui.stac.parts = vec![Geometry::Point(Coord::new(7.5, 46.5))];
        find_items(&mut app);
        wait_for_the_area(&mut app);
        assert_eq!(app.ui.stac.asked_area, None);
        // Clear drops an area still being made.
        app.ui.stac.waiting = None;
        find_items(&mut app);
        clear_area(&mut app);
        assert!(app.ui.stac.preparing.is_none() && app.ui.stac.parts.is_empty());
    }

    #[test]
    fn the_sort_is_fixed_while_the_area_is_made_and_the_search_goes_out_with_it() {
        let mut app = RustGisApp::with_sample();
        app.ui.stac.api = "http://127.0.0.1:1/stac".into();
        app.ui.stac.collection = "c".into();
        app.ui.stac.sortby = "-datetime".into();
        app.ui.stac.parts.push(rectangle([7.0, 46.0, 8.0, 47.0]));
        assert!(!busy(&app.ui.stac), "nothing is under way before Find Items");
        find_items(&mut app);
        // The dialog is busy while the area is made: the Sort by dropdown is off then, as Find
        // Items is, so a pick can't show in it without being the one sent.
        assert!(app.ui.stac.preparing.is_some() && busy(&app.ui.stac));
        wait_for_the_area(&mut app);
        assert_eq!(app.ui.stac.asked_sort.as_deref(), Some("-datetime"));
        assert!(busy(&app.ui.stac) || app.ui.stac.error.is_some(), "the search is under way until it is answered");
    }

    #[test]
    fn a_line_or_polygon_across_the_dateline_is_refused_with_a_reason() {
        let mut app = RustGisApp::with_sample();
        draw(&mut app, "line");
        let across = [Coord::new(179.0, 0.0), Coord::new(181.0, 0.0)];
        assert_eq!(take_drawn(&mut app, "line", &across), Some(false));
        assert!(app.ui.stac.parts.is_empty());
        let said = app.ui.stac.error.clone().unwrap_or_default();
        assert!(said.contains("180°"), "{said}");
        // A polygon with a side over the meridian is refused too.
        app.ui.stac.error = None;
        draw(&mut app, "polygon");
        let polygon = [Coord::new(179.0, 0.0), Coord::new(179.0, 5.0), Coord::new(181.0, 5.0)];
        assert_eq!(take_drawn(&mut app, "polygon", &polygon), Some(false));
        // A line wholly on one side, or wholly on the next copy of the world, is taken.
        app.ui.stac.error = None;
        for line in [[Coord::new(10.0, 0.0), Coord::new(20.0, 0.0)], [Coord::new(190.0, 0.0), Coord::new(200.0, 0.0)]] {
            draw(&mut app, "line");
            assert_eq!(take_drawn(&mut app, "line", &line), Some(true), "{line:?}");
        }
        assert!(app.ui.stac.error.is_none());
    }

    #[test]
    fn a_box_dragged_across_the_dateline_is_two_boxes() {
        let (a, b) = (Coord::new(179.0, 10.0), Coord::new(181.0, 20.0));
        let Geometry::Polygon(two) = box_between(a, b) else { panic!("a polygon") };
        assert_eq!(two.len(), 2, "one box each side of the dateline");
        let xs = |p: &Vec<Vec<Coord>>| p.iter().flatten().map(|c| c.x).fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
        assert_eq!(xs(&two[0]), (179.0, 180.0));
        assert_eq!(xs(&two[1]), (-180.0, -179.0));
        // Dragged from the other side, or on a repeated copy of the world, it is the same.
        let Geometry::Polygon(again) = box_between(Coord::new(-181.0, 10.0), Coord::new(-179.0, 20.0)) else { panic!("a polygon") };
        assert_eq!(again.len(), 2, "-181 to -179 is 179 to 181 on the next copy of the world");
        assert_eq!(xs(&again[0]), (179.0, 180.0));
        assert_eq!(xs(&again[1]), (-180.0, -179.0));
    }

    #[test]
    fn a_box_within_the_world_stays_one_box_and_a_wide_one_is_the_globe() {
        let Geometry::Polygon(one) = box_between(Coord::new(10.0, 0.0), Coord::new(20.0, 5.0)) else { panic!("a polygon") };
        assert_eq!(one.len(), 1);
        let Geometry::Polygon(globe) = box_between(Coord::new(-200.0, -10.0), Coord::new(200.0, 10.0)) else { panic!("a polygon") };
        assert_eq!(globe.len(), 1);
        let xs: Vec<f64> = globe[0].iter().flatten().map(|c| c.x).collect();
        assert!(xs.iter().all(|x| (-180.0..=180.0).contains(x)), "{xs:?}");
    }

    #[test]
    fn an_api_a_document_named_is_not_one_the_user_named() {
        let mut app = RustGisApp::with_sample();
        assert!(!app.ui.stac.api_from_document);
        app.ui.stac.api_from_document = true;
        switch_api(&mut app.ui.stac, "https://example.org/stac/v1");
        assert!(!app.ui.stac.api_from_document, "picking an API is the user's choice");
    }

    #[test]
    fn stac_document_assets_and_links_enforce_public_reach() {
        let _turn = crate::fetching::turn();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, rustgis_engine::render::rustgis_tiles::address::Reach)>>> =
            std::sync::Arc::default();
        let log = seen.clone();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let clean = cache::split_download_limit(&url).0.split("#rustgis-range=").next().unwrap_or(&url).to_string();
            log.lock().unwrap().push((clean.clone(), ceiling));
            let origin = rustgis_engine::render::rustgis_tiles::address::Origin::named(&clean);
            done(Ok(cache::Fetched { bytes: br#"{"type":"FeatureCollection","features":[]}"#.to_vec(), origin }));
        });

        let mut app = RustGisApp::with_sample();
        let doc = "https://example.com/stac/item.json";
        for bad in [
            "http://127.0.0.1:8080/internal.geojson",
            "http://localhost:8080/internal.geojson",
            "http://169.254.169.254/latest/meta-data",
            "http://10.0.0.1/internal.tif",
        ] {
            let raw = json!({
                "type": "Feature",
                "id": "scene-1",
                "bbox": [0.0, 0.0, 1.0, 1.0],
                "properties": {"datetime": "2025-01-01T00:00:00Z"},
                "links": [{"rel": "self", "href": "http://127.0.0.1:8080/spoofed-self.json"}],
                "assets": {"data": {"href": bad, "type": "application/geo+json", "roles": ["data"]}}
            });
            app.ui.stac.error = None;
            landed_link(&mut app, doc, Ok(cache::Fetched { bytes: raw.to_string().into_bytes(), origin: Origin::named(doc) }));
            let err = app.ui.stac.error.as_deref().unwrap_or("");
            assert!(
                err.contains("local network") || err.contains("never fetched"),
                "{bad} must be refused by address policy when linked from {doc}, got: {err:?}"
            );
            assert!(app.ui.downloads.is_empty() && app.ui.cog_probes.is_empty(), "{bad} must not start a download or probe");
        }
        assert!(seen.lock().unwrap().is_empty(), "refused private asset links must never reach the transport");

        let public_item = json!({
            "type": "Feature",
            "id": "scene-ok",
            "bbox": [0.0, 0.0, 1.0, 1.0],
            "properties": {"datetime": "2025-01-01T00:00:00Z"},
            "assets": {"data": {"href": "https://example.com/data/places.geojson", "type": "application/geo+json", "roles": ["data"]}}
        });
        app.ui.stac.error = None;
        landed_link(&mut app, doc, Ok(cache::Fetched { bytes: public_item.to_string().into_bytes(), origin: Origin::named(doc) }));
        assert_eq!(app.ui.stac.error, None);
        assert_eq!(app.ui.downloads.last().map(|d| d.params["from"].as_str()), Some(Some(doc)));
        assert_eq!(app.ui.downloads.last().and_then(|d| d.params["fromDocument"].as_bool()), Some(true));
        assert_eq!(
            seen.lock().unwrap().last().cloned(),
            Some(("https://example.com/data/places.geojson".into(), rustgis_engine::render::rustgis_tiles::address::Reach::Public))
        );
    }

    #[test]
    fn local_origin_stac_item_fetches_local_thumbnail_with_local_reach() {
        let _turn = crate::fetching::turn();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, Reach)>>> = std::sync::Arc::default();
        let log = seen.clone();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let clean = cache::split_download_limit(&url).0.to_string();
            log.lock().unwrap().push((clean.clone(), ceiling));
            done(Ok(cache::Fetched { bytes: vec![0x89, b'P', b'N', b'G'], origin: Origin::named(&clean) }));
        });

        let mut app = RustGisApp::with_sample();
        let local_doc = "http://localhost:8080/stac/item.json";
        let local_thumb = "http://localhost:8080/stac/preview.png";
        let raw = json!({
            "type": "Feature",
            "id": "local-scene",
            "bbox": [0.0, 0.0, 1.0, 1.0],
            "properties": {"datetime": "2025-01-01T00:00:00Z"},
            "assets": {
                "thumbnail": {"href": local_thumb, "type": "image/png", "roles": ["thumbnail"]}
            }
        });
        let item = stac::parse_item_from_origin(raw.to_string().as_bytes(), &Origin::named(local_doc)).unwrap();
        assert_eq!(item.origin_reach, Reach::Local);
        app.ui.stac.api = "http://localhost:8080/stac".into();
        app.ui.stac.items = vec![item];
        want_thumbs(&mut app);
        assert!(matches!(app.ui.stac.thumbs.get(local_thumb), Some(Thumb::Loading(_))));
        assert_eq!(seen.lock().unwrap().as_slice(), &[(local_thumb.to_string(), Reach::Local)]);
    }

    #[test]
    fn local_stac_link_redirecting_to_public_host_rejects_local_assets() {
        let _turn = crate::fetching::turn();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, Reach)>>> = std::sync::Arc::default();
        let log = seen.clone();
        let redirected_doc = "https://public-redirect.example/stac/item.json";
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let clean = cache::split_download_limit(&url).0.to_string();
            log.lock().unwrap().push((clean.clone(), ceiling));
            let body = json!({
                "type": "Feature",
                "id": "redirected-scene",
                "bbox": [0.0, 0.0, 1.0, 1.0],
                "properties": {"datetime": "2025-01-01T00:00:00Z"},
                "assets": {
                    "data": {"href": "http://127.0.0.1:8080/internal.geojson", "type": "application/geo+json", "roles": ["data"]},
                    "thumbnail": {"href": "http://127.0.0.1:8080/preview.png", "type": "image/png", "roles": ["thumbnail"]}
                }
            });
            done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin: Origin::named(redirected_doc) }));
        });

        let mut app = RustGisApp::with_sample();
        open_link(&mut app, "http://127.0.0.1:8080/stac-redirect").unwrap();
        poll(&mut app, &egui::Context::default());

        assert_eq!(seen.lock().unwrap().as_slice(), &[("http://127.0.0.1:8080/stac-redirect".to_string(), Reach::Local)]);
        assert_eq!(app.ui.stac.items.first().map(|i| i.origin_reach), Some(Reach::Public));
        assert_eq!(app.ui.stac.items.first().and_then(|i| i.origin.as_deref()), Some(redirected_doc));
        let err = app.ui.stac.error.as_deref().unwrap_or("");
        assert!(err.contains("local network"), "redirected public STAC item must reject loopback assets, got: {err:?}");
        assert!(app.ui.downloads.is_empty());

        seen.lock().unwrap().clear();
        want_thumbs(&mut app);
        assert!(matches!(app.ui.stac.thumbs.get("http://127.0.0.1:8080/preview.png"), Some(Thumb::Failed)));
        assert!(seen.lock().unwrap().is_empty(), "redirected public STAC item must not fetch loopback thumbnails");
    }

    #[test]
    fn static_catalog_walk_uses_user_reach_for_root_and_observed_reach_for_descendants() {
        let _turn = crate::fetching::turn();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, Reach)>>> = std::sync::Arc::default();

        // Case 1: User-selected internal hostname whose DNS resolves to a private IP (Reach::Local).
        // The initial root request uses Reach::Local, and once it lands with Reach::Local its local
        // child/item links also use Reach::Local.
        {
            let log = seen.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let body = if clean.ends_with("/catalog.json") {
                    json!({
                        "type": "Catalog",
                        "id": "corp",
                        "links": [{"rel": "item", "href": "http://10.0.0.5/stac/item.json"}]
                    })
                } else {
                    json!({
                        "type": "Feature",
                        "id": "corp-item",
                        "bbox": [0.0, 0.0, 1.0, 1.0],
                        "properties": {"datetime": "2025-01-01T00:00:00Z"},
                        "assets": {"data": {"href": "http://10.0.0.5/stac/scene.geojson", "type": "application/geo+json"}}
                    })
                };
                let origin = Origin::connected(&clean, "10.0.0.5".parse().unwrap()).unwrap().narrow_reach(ceiling);
                done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
            });

            let mut app = RustGisApp::with_sample();
            app.ui.stac.api = "https://catalog.corp.example.com/catalog.json".into();
            app.ui.stac.use_view = false;
            app.ui.stac.max_cloud.clear();
            find_items(&mut app);
            let ctx = egui::Context::default();
            poll(&mut app, &ctx);
            poll(&mut app, &ctx);
            poll(&mut app, &ctx);

            assert_eq!(
                seen.lock().unwrap().as_slice(),
                &[
                    ("https://catalog.corp.example.com/catalog.json".into(), Reach::Local),
                    ("http://10.0.0.5/stac/item.json".into(), Reach::Local),
                ]
            );
            assert_eq!(app.ui.stac.items.len(), 1);
            assert_eq!(app.ui.stac.items[0].origin_reach, Reach::Local);
        }

        // Case 2: User-selected root resolves to a public peer (Reach::Public).
        // The initial root request still uses Reach::Local (user-named), and once it lands with
        // Reach::Public its descendant links switch to Reach::Public.
        seen.lock().unwrap().clear();
        {
            let log = seen.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let body = if clean.ends_with("/catalog.json") {
                    json!({
                        "type": "Catalog",
                        "id": "pub",
                        "links": [{"rel": "item", "href": "https://public.example.com/stac/item.json"}]
                    })
                } else {
                    json!({
                        "type": "Feature",
                        "id": "pub-item",
                        "bbox": [0.0, 0.0, 1.0, 1.0],
                        "properties": {"datetime": "2025-01-01T00:00:00Z"},
                        "assets": {"data": {"href": "https://public.example.com/stac/scene.geojson", "type": "application/geo+json"}}
                    })
                };
                let origin = Origin::connected(&clean, "93.184.216.34".parse().unwrap()).unwrap().narrow_reach(ceiling);
                done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
            });

            let mut app = RustGisApp::with_sample();
            app.ui.stac.api = "https://public.example.com/catalog.json".into();
            app.ui.stac.use_view = false;
            app.ui.stac.max_cloud.clear();
            find_items(&mut app);
            let ctx = egui::Context::default();
            poll(&mut app, &ctx);
            poll(&mut app, &ctx);
            poll(&mut app, &ctx);

            assert_eq!(
                seen.lock().unwrap().as_slice(),
                &[
                    ("https://public.example.com/catalog.json".into(), Reach::Local),
                    ("https://public.example.com/stac/item.json".into(), Reach::Public),
                ]
            );
            assert_eq!(app.ui.stac.items.len(), 1);
            assert_eq!(app.ui.stac.items[0].origin_reach, Reach::Public);
        }
    }
    #[test]
    fn a_refused_next_link_is_reported_without_claiming_more_match() {
        let mut app = RustGisApp::with_sample();
        let body =
            br#"{"type":"FeatureCollection","features":[],"numberMatched":100,"links":[{"rel":"next","href":"http://127.0.0.1/private"}]}"#;
        let answer = Slot::new(std::sync::Mutex::new(Some(Ok(cache::Fetched {
            bytes: body.to_vec(),
            origin: Origin::named("https://public.example/search"),
        }))));
        app.ui.stac.waiting = Some((Asked::Search("http://127.0.0.1/start".into(), None), answer));
        poll(&mut app, &egui::Context::default());
        let said = app.ui.stac.info.as_deref().unwrap();
        assert!(said.contains("next page link was refused"), "{said}");
        assert!(!said.contains("more match") && !said.contains("narrow the search"), "{said}");
    }
    #[test]
    fn observed_local_hostname_assets_reach_the_downloader() {
        let _turn = crate::fetching::turn();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            log.lock().unwrap().push((url, ceiling));
            done(Err("fixture stops after admission".into()));
        });
        let mut app = RustGisApp::with_sample();
        let raw = json!({"type":"Feature","id":"intranet","geometry":null,"properties":{},"assets":{
            "data":{"href":"http://10.0.0.5/scene.geojson","type":"application/geo+json","roles":["data"]}
        }});
        let origin = Origin::connected("https://catalog.corp.example/item.json", "10.0.0.5".parse().unwrap()).unwrap();
        let item = stac::parse_item_from_origin(raw.to_string().as_bytes(), &origin).unwrap();
        add_item_asset(&mut app, &item, None);
        assert!(app.ui.stac.error.is_none(), "{:?}", app.ui.stac.error);
        assert!(seen.lock().unwrap().iter().any(|(url, reach)| url.contains("10.0.0.5/scene.geojson") && *reach == Reach::Local));
    }

    #[test]
    fn walk_notices_report_skipped_links_once_and_clean_walks_stay_quiet() {
        for skipped in [false, true] {
            let mut app = RustGisApp::with_sample();
            let links = if skipped {
                json!([
                    {"rel":"child","href":"ftp://files.example/catalog.json"}, {"rel":"item","href":""}
                ])
            } else {
                json!([])
            };
            let body = json!({"type":"Catalog","id":"test","links":links}).to_string();
            let mut walk = catalog::Walk::new("https://public.example/catalog.json", catalog::Filter::default(), 50);
            let url = walk.next_url().unwrap();
            walk.land(&url, Ok(body.into_bytes()));
            app.ui.stac.walk = Some((walk, Vec::new()));
            let mark = app.session.notice_mark();
            let ctx = egui::Context::default();
            poll_walk(&mut app, &ctx);
            assert_eq!(app.session.notices_since(mark).len(), usize::from(skipped));
            if skipped {
                assert!(app.session.notices_since(mark)[0].text.contains("2 catalog links were left out"));
                assert!(app.ui.stac.info.as_deref().unwrap().contains("2 catalog links were skipped"));
            }
            let mark = app.session.notice_mark();
            poll_walk(&mut app, &ctx);
            assert!(app.session.notices_since(mark).is_empty());
        }
    }
    #[test]
    fn catalog_child_notices_count_refusals_and_stay_quiet_for_clean_documents() {
        let mut app = RustGisApp::with_sample();
        let ctx = egui::Context::default();
        for (href, refused) in [("http://10.0.0.5/catalog.json", true), ("https://public.example/child.json", false)] {
            let body = json!({"type":"Catalog","id":"test","links":[{"rel":"child","href":href}]});
            let answer = Slot::new(std::sync::Mutex::new(Some(Ok(cache::Fetched {
                bytes: body.to_string().into_bytes(),
                origin: Origin::named("https://public.example/catalog.json"),
            }))));
            let mark = app.session.notice_mark();
            app.ui.stac.waiting = Some((Asked::Children("https://public.example/catalog.json".into()), answer));
            poll(&mut app, &ctx);
            let notices = app.session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(refused));
            if refused {
                assert!(notices[0].text.contains("1 catalog links were refused"));
            }
        }
    }

    #[test]
    fn preview_notices_report_refused_links_once_and_allow_local_documents() {
        let _turn = crate::fetching::turn();
        cache::set_fetcher(|_, done| done(Err("fixture stops after admission".into())));
        for (from, refused) in [("https://public.example/item.json", true), ("http://localhost/item.json", false)] {
            let mut app = RustGisApp::with_sample();
            let body = json!({"type":"Feature","id":"test","properties":{},"assets":{
                "thumbnail":{"href":"http://10.0.0.5/preview.png","type":"image/png","roles":["thumbnail"]}
            }});
            app.ui.stac.items = vec![stac::parse_item_from_origin(body.to_string().as_bytes(), &Origin::named(from)).unwrap()];
            let mark = app.session.notice_mark();
            want_thumbs(&mut app);
            let notices = app.session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(refused));
            if refused {
                assert!(notices[0].text.contains("1 preview links were refused"));
            }
            let mark = app.session.notice_mark();
            want_thumbs(&mut app);
            assert!(app.session.notices_since(mark).is_empty());
        }
    }
    #[test]
    fn partial_catalog_children_remain_visible_with_counted_limit_and_reach_notices() {
        let mut app = RustGisApp::with_sample();
        let mut links: Vec<_> = (0..2001).map(|i| json!({"rel":"child","href":format!("https://public.example/{i}.json")})).collect();
        links.push(json!({"rel":"child","href":"http://10.0.0.5/private.json"}));
        links.push(json!({"rel":"child","href":"ftp://files.example/catalog.json"}));
        let text = json!({"type":"Catalog","id":"test","links":links}).to_string();
        let from = "https://public.example/catalog.json";
        let answer = Slot::new(std::sync::Mutex::new(Some(Ok(cache::Fetched { bytes: text.into_bytes(), origin: Origin::named(from) }))));
        app.ui.stac.waiting = Some((Asked::Children(from.into()), answer));
        let mark = app.session.notice_mark();
        poll(&mut app, &egui::Context::default());
        assert!(app.ui.stac.error.is_none());
        assert_eq!(app.ui.stac.children.len(), 2000);
        let notices = app.session.notices_since(mark);
        assert_eq!(notices.len(), 3);
        assert!(notices.iter().any(|n| n.text.contains("1 catalog links were refused")));
        assert!(notices.iter().any(|n| n.text.contains("1 catalog links were left out because their addresses")));
    }
}
