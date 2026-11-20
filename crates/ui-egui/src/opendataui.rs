//! Search Open Data and the Web Data services built on it: datasets found by text or where the map
//! is, in a CKAN catalogue (data.gov's by default), on Socrata or ArcGIS Hub, or in one service
//! (ArcGIS Online, Earthdata GIS, OpenAerialMap, USGS 3DEP, IGN LiDAR HD, GeoLens, Hugging Face,
//! the Ocean Data Platform, US agencies', states' and local governments' portals), and their
//! resources added with a click. The search goes off the frame through the tile cache's downloads
//! (a thread natively, fetch on the web); a file is added by its address through `ui.addUrl`, a
//! service opens Add From Service connected to it.

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::cache;
use rustgis_engine::render::rustgis_tiles::portal::{DEM_DATASETS, Dataset, Portal, Use};
use rustgis_engine::render::rustgis_tiles::usgis::{self, Search, Set};
use serde_json::json;

use crate::{RustGisApp, Slot};

type Answer = Slot<Result<Vec<u8>, String>>;

/// The portals Search Open Data offers: the kind, its name, and what its address field takes.
const PORTALS: [(&str, &str, &str); 3] = [
    ("ckan", "data.gov and CKAN catalogues", "catalog.data.gov, or a CKAN site such as https://open.canada.ca/data"),
    ("socrata", "Socrata", "every Socrata site, or one such as data.cityofnewyork.us"),
    ("arcgis-hub", "ArcGIS Hub", "hub.arcgis.com, or an organisation's Hub site"),
];

/// A Web Data service the dialog opens on: its id, its title, the portal searched, what its
/// address field takes (none: no field), and the US portals it picks from.
pub struct Service {
    pub id: &'static str,
    pub title: &'static str,
    portal: &'static str,
    address: Option<&'static str>,
    sets: Option<&'static [Set]>,
}

/// The services.
pub const SERVICES: [Service; 13] = [
    Service {
        id: "arcgis-portal",
        title: "ArcGIS Portal",
        portal: "arcgis-portal",
        address: Some("ArcGIS Online, or a portal such as https://gis.example.org/portal"),
        sets: None,
    },
    Service { id: "dynamical", title: "Dynamical", portal: "dynamical", address: None, sets: None },
    Service { id: "earthdata-gis", title: "Earthdata GIS", portal: "earthdata-gis", address: None, sets: None },
    Service {
        id: "geolens",
        title: "GeoLens",
        portal: "geolens",
        address: Some("https://datasets.geolibre.app, or another GeoLens server"),
        sets: None,
    },
    Service { id: "huggingface", title: "Hugging Face", portal: "huggingface", address: None, sets: None },
    Service { id: "ocean-data", title: "Ocean Data Platform", portal: "ocean-data", address: None, sets: None },
    Service { id: "openaerialmap", title: "OpenAerialMap", portal: "openaerialmap", address: None, sets: None },
    Service { id: "usgs-dem", title: "USGS 3DEP DEM", portal: "usgs-dem", address: None, sets: None },
    Service { id: "ign-lidar", title: "IGN LiDAR HD", portal: "ign-lidar", address: None, sets: None },
    Service { id: "us-federal", title: "US Federal GIS", portal: "arcgis-portal", address: None, sets: Some(usgis::FEDERAL) },
    Service { id: "us-state", title: "US State GIS", portal: "arcgis-portal", address: None, sets: Some(usgis::STATE) },
    Service { id: "us-local", title: "US Local GIS", portal: "arcgis-portal", address: None, sets: Some(usgis::LOCAL) },
    Service { id: "tennessee", title: "Tennessee GIS", portal: "arcgis-portal", address: None, sets: Some(usgis::TENNESSEE) },
];

fn service(id: &str) -> Option<&'static Service> {
    SERVICES.iter().find(|s| s.id == id)
}

/// A search under way: the portal, its address, the box it keeps to, its words, what it is on
/// (the US portal picked when it began), and whether it is a next page (added to the list).
struct Pending {
    portal: Portal,
    url: String,
    area: Option<[f64; 4]>,
    query: String,
    source: String,
    more: bool,
    slot: Answer,
}

/// A result's picture: being fetched, ready, or not to be had.
enum Thumb {
    Loading(Answer),
    Ready(egui::TextureHandle),
    Failed,
}

/// The most pictures kept (a few pages of results).
const MAX_THUMBS: usize = 200;

/// The kinds of ArcGIS items the chips keep a search to.
const CHIPS: [(&str, &str); 5] =
    [("all", "All"), ("imagery", "Imagery"), ("maps", "Maps"), ("features", "Features"), ("webmaps", "Web maps")];

/// The pane's choices, the search under way and what it found.
pub struct State {
    /// The Web Data service the pane is on, or none (Search Open Data, any portal).
    pub service: Option<&'static str>,
    pub portal: &'static str,
    pub url: String,
    pub query: String,
    /// Only datasets where the map is.
    pub here: bool,
    /// The US portal picked: its set and its place in it.
    pub set: usize,
    pub catalog: usize,
    /// The 3DEP elevation model searched (in [`DEM_DATASETS`]).
    pub dem: usize,
    /// The kind of ArcGIS items searched (a [`CHIPS`] name).
    pub category: &'static str,
    waiting: Option<Pending>,
    /// A dataset's files or layers being listed: the dataset (its place in `found`), the portal,
    /// the listing's address.
    listing: Option<(usize, Portal, String, Answer)>,
    pub found: Vec<Dataset>,
    /// How many the search found in all, when the portal says, and its next page's address.
    total: Option<u64>,
    next: Option<(Portal, String, Option<[f64; 4]>, String, String)>,
    /// The results whose details are shown.
    open: std::collections::HashSet<usize>,
    thumbs: std::collections::HashMap<String, Thumb>,
    /// The Zarr stores being checked (by their result's place), which some datasets lack.
    probes: Vec<(usize, Answer)>,
    /// The result whose Zarr store is open in the pane, to pick its variable and steps.
    zarr_open: Option<String>,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            service: None,
            portal: "ckan",
            url: String::new(),
            query: String::new(),
            here: false,
            set: 0,
            catalog: 0,
            dem: 2,
            category: "all",
            waiting: None,
            listing: None,
            found: Vec::new(),
            total: None,
            next: None,
            open: Default::default(),
            thumbs: Default::default(),
            probes: Vec::new(),
            zarr_open: None,
            error: None,
            info: None,
        }
    }
}

impl State {
    /// The pane's title: the service's, or Search Open Data's.
    pub fn title(&self) -> &'static str {
        self.service.and_then(service).map(|s| s.title).unwrap_or("Search Open Data")
    }

    /// What the search was on, in its count: the US portal picked, the service, or the portal.
    fn source(&self, portal: &Portal) -> String {
        let svc = self.service.and_then(service);
        let picked = svc.and_then(|s| s.sets).and_then(|sets| sets.get(self.set)).and_then(|s| s.catalogs.get(self.catalog));
        match (picked, svc) {
            (Some(c), _) => c.name.to_string(),
            (None, Some(s)) if s.address.is_none() || self.url.trim().is_empty() => tl!(s.title).to_string(),
            _ => portal.label(),
        }
    }
}

/// Starts fetching `url` off the frame.
fn fetch(url: &str) -> Answer {
    let slot: Answer = Slot::default();
    let land = slot.clone();
    cache::download(url, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
    slot
}

/// Lands the search's answer, a dataset's listing and the results' pictures.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    poll_listing(app, ctx);
    poll_thumbs(app, ctx);
    poll_probes(app, ctx);
    let Some(w) = &app.ui.opendata.waiting else { return };
    let Some(r) = w.slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some(w) = app.ui.opendata.waiting.take() else { return };
    let replaced = !w.more && app.ui.opendata.zarr_open.is_some();
    let st = &mut app.ui.opendata;
    match r.and_then(|b| w.portal.parse(&w.url, &b, w.area, &w.query).map(|found| (found, w.portal.paging(&w.url, &b)))) {
        Ok((found, (total, next))) => {
            let first = if w.more { st.found.len() } else { 0 };
            if w.more {
                st.found.extend(found);
            } else {
                st.found = found;
                st.zarr_open = None;
                // The earlier list's checks would land on these results' places.
                st.probes.clear();
            }
            // A dynamical.org store (Zarr 3) is checked before it is offered.
            for (i, d) in st.found.iter().enumerate().skip(first) {
                if let Some(r) = d.resources.iter().find(|r| r.using == Use::Zarr && r.url.starts_with("https://data.dynamical.org/")) {
                    st.probes.push((i, fetch(&format!("{}/zarr.json", r.url))));
                }
            }
            st.total = total;
            st.next = next.map(|n| (w.portal.clone(), n, w.area, w.query.clone(), w.source.clone()));
            let shown = st.found.len().to_string();
            st.info = Some(match total {
                _ if st.found.is_empty() => tl!("No datasets with data RustGIS can add").to_string(),
                Some(t) => crate::i18n::fmt(
                    "Showing {n} of {total} on {portal}",
                    &[("n", &shown), ("total", &t.to_string()), ("portal", &w.source)],
                ),
                None => crate::i18n::fmt("{n} datasets on {portal}", &[("n", &shown), ("portal", &w.source)]),
            });
        }
        Err(e) => st.error = Some(e),
    }
    // A new list closes the open store, and the Zarr picker's dataset mode with it.
    if replaced && app.ui.opendata.zarr_open.is_none() {
        app.ui.zarr.title = None;
    }
}

fn poll_listing(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some((_, _, _, slot)) = &app.ui.opendata.listing else { return };
    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some((i, portal, url, _)) = app.ui.opendata.listing.take() else { return };
    let st = &mut app.ui.opendata;
    match r.and_then(|b| portal.parse_listing(&url, &b)) {
        Ok(files) => {
            if files.is_empty() {
                st.info = Some(tl!("The dataset has nothing RustGIS can add").to_string());
            }
            if let Some(d) = st.found.get_mut(i) {
                d.resources = files;
                d.listing = None;
            }
        }
        Err(e) => st.error = Some(e),
    }
}

/// Lands the Zarr stores' checks: a dataset without one is shown as such.
fn poll_probes(app: &mut RustGisApp, ctx: &egui::Context) {
    let st = &mut app.ui.opendata;
    let mut landed = Vec::new();
    st.probes.retain(|(i, slot)| match slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some(r) => {
            // Only a store that isn't there (a 404): any other error, a 403 included, may be
            // passing and leaves the dataset to be added.
            let absent = r.err().and_then(|e| cache::http_status(&e)) == Some(404);
            landed.push((*i, !absent));
            false
        }
        None => true,
    });
    for (i, ok) in landed {
        if !ok && let Some(d) = st.found.get_mut(i) {
            d.resources.clear();
            d.kind = format!("{} · {}", d.kind, tl!("Icechunk only, not read yet"));
        }
    }
    if !st.probes.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
}

/// Lands the pictures fetched, as textures.
fn poll_thumbs(app: &mut RustGisApp, ctx: &egui::Context) {
    let mut waiting = false;
    for (url, thumb) in app.ui.opendata.thumbs.iter_mut() {
        let Thumb::Loading(slot) = thumb else { continue };
        let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            waiting = true;
            continue;
        };
        *thumb = match r.and_then(|b| cache::decode_image(&b)) {
            Ok(pm) => {
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                Thumb::Ready(ctx.load_texture(format!("webdata_thumb_{url}"), img, egui::TextureOptions::LINEAR))
            }
            Err(_) => Thumb::Failed,
        };
    }
    if waiting {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
}

/// Fills the pane from `ui.searchOpenData`'s parameters (`service`, `portal`, `url`, `query`,
/// `view`, `type`) and, with a query (or for a service that needs none), starts the search. A
/// service opens the pane on it alone.
pub fn start(app: &mut RustGisApp, p: &serde_json::Value) {
    let text = |k: &str| p.get(k).and_then(serde_json::Value::as_str).map(str::trim);
    let svc = text("service").and_then(service);
    let st = &mut app.ui.opendata;
    let fresh = st.service != svc.map(|s| s.id);
    if fresh {
        // Another service: its own choices.
        *st = State { service: svc.map(|s| s.id), ..State::default() };
        if let Some(s) = svc {
            st.portal = s.portal;
            // A search by place is where the map is.
            st.here = Portal::new(Some(s.portal), None).is_ok_and(|p| p.needs_place());
        }
    }
    if svc.is_none()
        && let Some(kind) = text("portal")
    {
        st.portal = match kind {
            "socrata" => "socrata",
            "arcgis-hub" | "hub" | "arcgis" => "arcgis-hub",
            _ => "ckan",
        };
    }
    if let Some(u) = text("url") {
        st.url = u.to_string();
    }
    if let Some(c) = text("type").and_then(|c| CHIPS.iter().find(|(k, _)| *k == c)) {
        st.category = c.0;
    }
    if let Some(v) = p.get("view").and_then(serde_json::Value::as_bool) {
        // A search by place is always where the map is.
        st.here = v || Portal::new(Some(st.portal), None).is_ok_and(|p| p.needs_place());
    }
    let words = text("query").filter(|q| !q.is_empty());
    if let Some(q) = words {
        st.query = q.to_string();
    }
    // A service that lists without words shows its newest datasets as it opens, as a search does.
    let lists = svc.is_some() && fresh && Portal::new(Some(st.portal), None).is_ok_and(|p| !p.needs_query() && !p.needs_place());
    if (words.is_some() || lists) && st.waiting.is_none() {
        st.error = None;
        st.info = None;
        search(app, false);
    }
}

/// The search's parameters, as `portal.search` takes them.
fn params(st: &State) -> serde_json::Value {
    let mut p = json!({"portal": st.portal, "query": st.query.trim(), "view": st.here});
    if !st.url.trim().is_empty() {
        p["url"] = json!(st.url.trim());
    }
    let svc = st.service.and_then(service);
    if let Some(sets) = svc.and_then(|s| s.sets)
        && let Some(c) = sets.get(st.set).and_then(|s| s.catalogs.get(st.catalog))
    {
        match c.search {
            Search::ArcGis(org) => p["org"] = json!(org),
            Search::Socrata(domain) => {
                p["portal"] = json!("socrata");
                p["url"] = json!(domain);
            }
        }
    }
    if svc.is_some_and(|s| s.id == "usgs-dem")
        && let Some((label, _)) = DEM_DATASETS.get(st.dem)
    {
        p["dataset"] = json!(label);
    }
    if p["portal"] == "arcgis-portal" || p["portal"] == "earthdata-gis" {
        p["type"] = json!(st.category);
    }
    p
}

/// Starts a search for the pane's choices, or (`more`) its next page.
fn search(app: &mut RustGisApp, more: bool) {
    if more {
        let Some((portal, url, area, query, source)) = app.ui.opendata.next.take() else { return };
        let slot = fetch(&url);
        app.ui.opendata.waiting = Some(Pending { portal, url, area, query, source, more, slot });
        return;
    }
    let p = params(&app.ui.opendata);
    match rustgis_engine::cmd::portals::portal_query(&app.session, &p) {
        Ok((portal, query, area, limit)) => {
            let url = portal.search_url(&query, area, limit);
            let slot = fetch(&url);
            let st = &mut app.ui.opendata;
            st.found.clear();
            st.open.clear();
            st.probes.clear();
            // Pictures of the earlier results go (and their textures).
            st.thumbs.clear();
            st.listing = None;
            st.total = None;
            st.next = None;
            let source = st.source(&portal);
            st.waiting = Some(Pending { portal, url, area, query, source, more: false, slot });
        }
        Err(e) => app.ui.opendata.error = Some(e),
    }
}

/// Starts listing a dataset's files or layers.
fn list_files(app: &mut RustGisApp, i: usize) {
    let st = &app.ui.opendata;
    let Some(url) = st.found.get(i).and_then(|d| d.listing.clone()) else { return };
    match Portal::new(Some(st.portal), None) {
        Ok(portal) => {
            let slot = fetch(&url);
            app.ui.opendata.listing = Some((i, portal, url, slot));
        }
        Err(e) => app.ui.opendata.error = Some(e),
    }
}

/// What the pane says the service is.
fn about(id: Option<&str>) -> &'static str {
    match id {
        Some("arcgis-portal") => {
            "Public layers on ArcGIS Online or another ArcGIS portal: feature, map, image and vector tile services, web maps and GeoJSON files."
        }
        Some("dynamical") => {
            "Weather and climate data from dynamical.org (NOAA GFS, GEFS and HRRR, ECMWF, DWD ICON…) as Zarr stores: open a dataset, pick a variable and its time steps, and add it."
        }
        Some("earthdata-gis") => "NASA's Earthdata GIS: imagery, maps, features and web maps of Earth science data.",
        Some("geolens") => "A GeoLens server's datasets, read as features.",
        Some("huggingface") => {
            "Datasets on Hugging Face by name or owner/name; browse a dataset's files and add GeoTIFFs, GeoParquet, PMTiles, LiDAR and other files."
        }
        Some("ocean-data") => "HUB Ocean's Ocean Data Platform: marine datasets, read as features (up to 10,000 each).",
        Some("openaerialmap") => "Drone and aerial imagery shared on OpenAerialMap, newest first, added as cloud-optimised GeoTIFFs.",
        Some("usgs-dem") => "USGS 3DEP elevation models (GeoTIFF tiles from The National Map) where the map is.",
        Some("ign-lidar") => "IGN's LiDAR HD point clouds of France: 1 km tiles where the map is, added as COPC point clouds.",
        Some("us-federal") => "Public GIS portals of US federal agencies, by department.",
        Some("us-state") => "Public GIS portals of US states and the District of Columbia.",
        Some("tennessee") => {
            "Tennessee's public GIS data (Tennessee GeoData, geodata.tn.gov): property boundaries, transportation, health, hydrology and more."
        }
        Some("us-local") => "Public GIS portals of US counties and cities, by state.",
        _ => {
            "Open data catalogues searched by text. Files are added by their address (read in place when they can be); a service opens Add From Service to pick its layer."
        }
    }
}

/// What a click on a result asks for.
enum Act {
    Details(usize),
    Zoom([f64; 4]),
    Add(String, Use, String),
    List(usize),
}

/// The pane: a portal (or the service's catalog), a search, and the datasets found as cards with
/// their pictures and actions.
pub fn dialog(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let svc = app.ui.opendata.service.and_then(service);
    ui.label(egui::RichText::new(tl!(about(svc.map(|s| s.id)))).color(t.text_dim));
    let busy = app.ui.opendata.waiting.is_some();
    let mut go = false;
    // The fields fill the pane, beside their labels.
    let w = (ui.available_width() - 120.0).clamp(160.0, 320.0);
    egui::Grid::new("opendata").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let st = &mut app.ui.opendata;
        match svc {
            None => {
                let input_label = ui.label(tl!("Portal"));
                let shown = PORTALS.iter().find(|(k, _, _)| *k == st.portal).map(|(_, n, _)| tl!(*n).to_string()).unwrap_or_default();
                egui::ComboBox::from_id_salt("od_portal")
                    .selected_text(shown)
                    .width(w)
                    .show_ui(ui, |ui| {
                        for (kind, name, _) in PORTALS {
                            if ui.selectable_label(st.portal == kind, tl!(name)).clicked() && st.portal != kind {
                                st.portal = kind;
                                st.url.clear();
                            }
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
                ui.end_row();
                let input_label = ui.label(tl!("Address"));
                let hint = PORTALS.iter().find(|(k, _, _)| *k == st.portal).map(|(_, _, h)| tl!(*h)).unwrap_or_default();
                ui.add(egui::TextEdit::singleline(&mut st.url).hint_text(hint).desired_width(w)).labelled_by(input_label.id);
                ui.end_row();
            }
            Some(s) => {
                if let Some(hint) = s.address {
                    let input_label = ui.label(tl!("Address"));
                    ui.add(egui::TextEdit::singleline(&mut st.url).hint_text(tl!(hint)).desired_width(w)).labelled_by(input_label.id);
                    ui.end_row();
                }
                // A service with one portal (Tennessee) has nothing to pick.
                if let Some(sets) = s.sets.filter(|sets| sets.len() > 1 || sets.iter().any(|s| s.catalogs.len() > 1)) {
                    catalog_picker(ui, st, sets, w);
                }
                if s.id == "usgs-dem" {
                    let input_label = ui.label(tl!("Elevation model"));
                    let shown = DEM_DATASETS.get(st.dem).map(|(l, _)| tl!(*l).to_string()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("od_dem")
                        .selected_text(shown)
                        .width(w)
                        .show_ui(ui, |ui| {
                            for (i, (label, _)) in DEM_DATASETS.iter().enumerate() {
                                ui.selectable_value(&mut st.dem, i, tl!(*label));
                            }
                        })
                        .response
                        .labelled_by(input_label.id);
                    ui.end_row();
                }
            }
        }
    });
    let st = &mut app.ui.opendata;
    let place = Portal::new(Some(st.portal), None).is_ok_and(|p| p.needs_place());
    // A Socrata site among the US portals is searched by words only.
    let needs = {
        let p = params(st);
        Portal::new(p["portal"].as_str(), p["url"].as_str()).is_ok_and(|p| p.needs_query())
    };
    let hint: String = match svc {
        Some(s) if s.id == "huggingface" => tl!("a name, or owner/name such as giswqs/geospatial").into(),
        _ if place => tl!("words to keep to (optional)").into(),
        Some(_) if needs => tl!("Search (this portal needs words)…").into(),
        Some(s) => crate::i18n::fmt("Search {name}…", &[("name", tl_raw!(s.title))]),
        None => tl!("parks, bike lanes, flood zones…").into(),
    };
    ui.horizontal(|ui| {
        let button = ui.spacing().interact_size.x + 50.0;
        let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text(hint).desired_width((ui.available_width() - button).max(120.0)));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let search = egui::Button::new(egui::RichText::new(tl!("Search")).color(t.on_accent)).fill(t.accent);
        go = (ui.add_enabled(!busy, search).clicked() || enter) && !busy;
    });
    // ArcGIS portals: the kind of item, as chips.
    let arcgis = matches!(params(st)["portal"].as_str(), Some("arcgis-portal" | "earthdata-gis"));
    if arcgis {
        ui.horizontal_wrapped(|ui| {
            for (key, label) in CHIPS {
                if ui.selectable_label(st.category == key, tl!(label)).clicked() && st.category != key {
                    st.category = key;
                    go = !busy;
                }
            }
        });
    }
    // A search by place is always where the map is.
    if ui.add_enabled(!place, egui::Checkbox::new(&mut st.here, tl!("Limit to map view"))).changed() {
        go = !busy;
    }
    if place {
        st.here = true;
    }
    if go {
        st.error = None;
        st.info = None;
        search(app, false);
    }
    let st = &app.ui.opendata;
    if busy || st.listing.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(if busy { tl!("Searching…") } else { tl!("Listing…") });
        });
    }
    if let Some(e) = &st.error {
        ui.label(egui::RichText::new(e).color(t.red));
    }
    if let Some(i) = &st.info {
        ui.label(egui::RichText::new(i).color(t.text_dim));
    }
    // A dataset's Zarr store, open to pick its variable and steps.
    let mut close_zarr = false;
    if let Some(title) = st.zarr_open.clone() {
        // The frame's margins and border are outside its contents: they're kept within the pane.
        let inner = ui.available_width() - 18.0;
        egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.accent)).corner_radius(6).inner_margin(egui::Margin::same(8)).show(
            ui,
            |ui| {
                ui.set_width(inner);
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&title).strong()).wrap());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close_zarr = ui.button(tl!("Close")).clicked();
                    });
                });
                crate::zarrui::picker(app, ui);
            },
        );
        ui.add_space(4.0);
    }
    if close_zarr {
        // The Zarr picker's dataset mode goes with the dataset.
        app.ui.opendata.zarr_open = None;
        app.ui.zarr.title = None;
    }
    let st = &mut app.ui.opendata;
    let listing = st.listing.is_some();
    let mut act = None;
    let mut wanted = Vec::new();
    for (i, d) in st.found.iter().enumerate() {
        let thumb = d.thumbnail.as_ref().and_then(|u| match st.thumbs.get(u) {
            Some(Thumb::Ready(tex)) => Some(tex.clone()),
            None => {
                wanted.push(u.clone());
                None
            }
            _ => None,
        });
        ui.push_id(i, |ui| {
            if let Some(a) = card(ui, &t, d, thumb, st.open.contains(&i), i, busy || listing) {
                act = Some(a);
            }
        });
    }
    let more = st.next.is_some() && !busy;
    if more && ui.button(tl!("Load more")).clicked() {
        search(app, true);
    }
    // Pictures of the results shown, fetched once each.
    let st = &mut app.ui.opendata;
    for u in wanted {
        if st.thumbs.len() >= MAX_THUMBS {
            break;
        }
        let slot = fetch(&u);
        st.thumbs.insert(u, Thumb::Loading(slot));
    }
    match act {
        Some(Act::Details(i)) if !st.open.remove(&i) => {
            st.open.insert(i);
        }
        Some(Act::Zoom(b)) => {
            if let Err(e) = app.run("map.zoomToExtent", json!({"extent": b})) {
                app.ui.opendata.error = Some(e);
            }
        }
        Some(Act::List(i)) => {
            app.ui.opendata.error = None;
            list_files(app, i);
        }
        Some(Act::Add(url, Use::Zarr, title)) => {
            // Its variable and steps are picked in the pane.
            app.ui.zarr.title = Some(title.clone());
            app.ui.opendata.zarr_open = Some(title);
            app.ui.zarr.address = url;
            crate::zarrui::connect(app);
        }
        Some(Act::Add(url, using, title)) => {
            app.ui.opendata.error = None;
            if let Err(e) = add(app, &url, &using, &title) {
                app.ui.opendata.error = Some(e);
            }
        }
        None | Some(Act::Details(_)) => {}
    }
}

/// Adds a result's resource: a file by its address, a service's features page by page, or a
/// service through Add From Service, connected, to pick its layer.
fn add(app: &mut RustGisApp, url: &str, using: &Use, title: &str) -> Result<(), String> {
    match using {
        Use::File | Use::Zarr => app.run("ui.addUrl", json!({"url": url, "name": title})).map(|_| ()),
        Use::Features => app.run("ui.addUrl", json!({"url": url, "name": title, "service": true})).map(|_| ()),
        Use::Service { kind, .. } => app
            .run("ui.dialog", json!({"name": "addService"}))
            .and_then(|_| app.run("ui.addService", json!({"url": url, "type": kind})))
            .map(|_| ()),
    }
}

/// A result's card: its picture, title, what it is, who made it and when, and its actions
/// (Details, Portal, Zoom, Add), its details below when shown.
fn card(
    ui: &mut egui::Ui,
    t: &crate::theme::Tokens,
    d: &Dataset,
    thumb: Option<egui::TextureHandle>,
    open: bool,
    i: usize,
    busy: bool,
) -> Option<Act> {
    let mut act = None;
    // The frame's margins and border are outside its contents: they're kept within the pane.
    let inner = ui.available_width() - 18.0;
    egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6).inner_margin(egui::Margin::same(8)).show(
        ui,
        |ui| {
            ui.set_width(inner);
            ui.horizontal_top(|ui| {
                if let Some(tex) = &thumb {
                    let size = tex.size_vec2();
                    let h = 64.0;
                    let w = (size.x / size.y.max(1.0) * h).clamp(48.0, 96.0);
                    ui.add(egui::Image::new(tex).fit_to_exact_size(vec2(w, h)).corner_radius(4));
                }
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&d.title).strong()).wrap());
                    let meta: Vec<&str> =
                        [d.kind.as_str(), d.organization.as_str(), d.date.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
                    if !meta.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(meta.join(" · ")).small().color(t.text_dim)).wrap());
                    }
                    ui.horizontal_wrapped(|ui| {
                        if (!d.description.is_empty() || !d.id.is_empty())
                            && ui.add(egui::Button::new(tl!("Details")).selected(open)).clicked()
                        {
                            act = Some(Act::Details(i));
                        }
                        if let Some(page) = &d.page
                            && ui.button(tl!("Portal")).on_hover_text(page).clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(page));
                        }
                        if let Some(b) = d.bbox
                            && ui.button(tl!("Zoom")).clicked()
                        {
                            act = Some(Act::Zoom(b));
                        }
                        if d.listing.is_some() {
                            if ui.add_enabled(!busy, egui::Button::new(tl!("Add"))).on_hover_text(tl!("List what it holds")).clicked() {
                                act = Some(Act::List(i));
                            }
                        } else if let [r] = d.resources.as_slice() {
                            if ui
                                .add_enabled(!busy, egui::Button::new(tl!("Add")))
                                .on_hover_text(format!("{} · {}", r.format, r.url))
                                .clicked()
                            {
                                act = Some(Act::Add(r.url.clone(), r.using.clone(), d.title.clone()));
                            }
                        } else if !d.resources.is_empty() {
                            ui.add_enabled_ui(!busy, |ui| {
                                ui.menu_button(tl!("Add ▾"), |ui| {
                                    for r in &d.resources {
                                        if ui.button(&r.format).on_hover_text(&r.url).clicked() {
                                            // A file of a dataset is named after the file.
                                            let name = if r.format.contains('.') || r.format.len() > 12 {
                                                r.format.rsplit('/').next().unwrap_or(&r.format).to_string()
                                            } else {
                                                d.title.clone()
                                            };
                                            act = Some(Act::Add(r.url.clone(), r.using.clone(), name));
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        }
                    });
                });
            });
            if open {
                if !d.description.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(&d.description).small()).wrap());
                }
                if !d.id.is_empty() {
                    ui.label(egui::RichText::new(crate::i18n::fmt("Id: {id}", &[("id", &d.id)])).small().color(t.text_dim));
                }
                if let Some(b) = d.bbox {
                    let extent = format!("{:.4}, {:.4}, {:.4}, {:.4}", b[0], b[1], b[2], b[3]);
                    ui.label(egui::RichText::new(crate::i18n::fmt("Extent: {extent}", &[("extent", &extent)])).small().color(t.text_dim));
                }
            }
        },
    );
    ui.add_space(2.0);
    act
}

/// The US portal picker: a set (department or state), then its portal.
fn catalog_picker(ui: &mut egui::Ui, st: &mut State, sets: &'static [Set], w: f32) {
    let input_label = ui.label(tl!("Group"));
    let shown = sets.get(st.set).map(|s| s.name).unwrap_or_default();
    egui::ComboBox::from_id_salt("od_set")
        .selected_text(shown)
        .width(w)
        .height(400.0)
        .show_ui(ui, |ui| {
            for (i, s) in sets.iter().enumerate() {
                if ui.selectable_label(st.set == i, s.name).clicked() && st.set != i {
                    st.set = i;
                    st.catalog = 0;
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    ui.label(tl!("Portal"));
    let catalogs = sets.get(st.set).map(|s| s.catalogs).unwrap_or_default();
    let shown = catalogs.get(st.catalog).map(|c| c.name).unwrap_or_default();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("od_catalog")
            .selected_text(shown)
            .width(w - 44.0)
            .height(400.0)
            .show_ui(ui, |ui| {
                for (i, c) in catalogs.iter().enumerate() {
                    ui.selectable_value(&mut st.catalog, i, c.name);
                }
            })
            .response
            .on_hover_text(tl!("Catalog"));
        if let Some(c) = catalogs.get(st.catalog) {
            ui.hyperlink_to(tl!("Open"), c.url);
        }
    });
    ui.end_row();
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::io::{Read, Write};

    /// A GeoLens dataset's 1,500 features, served in two pages.
    fn serve() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let next = format!("{base}/api/collections/trees/items?limit=1000&after=1000");
        let page = move |from: u32, to: u32, next: Option<&str>| {
            let features: Vec<Value> = (from..to)
                .map(|i| json!({"type": "Feature", "id": i, "properties": {"n": i}, "geometry": {"type": "Point", "coordinates": [f64::from(i % 90), 1.0]}}))
                .collect();
            let mut p = json!({"type": "FeatureCollection", "numberMatched": 1500, "features": features});
            if let Some(h) = next {
                p["links"] = json!([{"rel": "next", "type": "application/geo+json", "href": h}]);
            }
            p.to_string()
        };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.contains("after=1000") { page(1000, 1500, None) } else { page(0, 1000, Some(&next)) };
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        base
    }

    /// Web Data's Add reads a GeoLens dataset's every page, not only the server's first 1,000.
    #[test]
    fn a_geolens_dataset_is_added_page_by_page() {
        let _turn = crate::fetching::turn();
        let base = serve();
        let mut h =
            egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).with_step_dt(0.05).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                RustGisApp::with_sample(),
            );
        let url = format!("{base}/api/collections/trees/items?limit=1000");
        add(h.state_mut(), &url, &Use::Features, "Street Trees").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while h.state().session.map().layer_by_name("Street Trees").is_none() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20)); // test-wait: frames land the background download
        }
        assert_eq!(h.state().session.map().layer_by_name("Street Trees").unwrap().dataset.features.len(), 1500);
    }
}
