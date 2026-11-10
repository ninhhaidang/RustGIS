//! Web Data › NASA Earthaccess: like GeoLibre's Earthaccess panel, a dataset (a popular one, or
//! one found by keyword), then its granules where the map is and between two dates, drawn as
//! footprints, each with its files (a GeoTIFF added as a COG, read in place), Zoom and Details.
//! The Earthdata Login token the files are read with is kept for the session, or remembered on
//! this computer (`earthdata.login` with `remember`). Requests go off the frame through the tile
//! cache's downloads, and the remembered token is read off the frame too.

use rustgis_engine::render::rustgis_tiles::cache;
use rustgis_engine::render::rustgis_tiles::earthdata::{self, Collection, Granule};
use serde_json::{Value, json};

use crate::{RustGisApp, Slot};

type Answer = Slot<Result<Vec<u8>, String>>;

/// Granules asked for a page.
const PAGE: usize = 25;
/// The most preview pictures kept, and the largest fetched.
const MAX_THUMBS: usize = 200;
const MAX_THUMB_BYTES: usize = 4 << 20;

/// A granule's preview picture: being fetched, shown, or not to be had.
enum Thumb {
    Loading(Answer),
    Ready(egui::TextureHandle),
    Failed,
}

/// What is being fetched.
enum Asked {
    /// Datasets by keyword: the next page (true) or a new search.
    Collections { more: bool, page: usize },
    /// A popular dataset's versions, to pick the best.
    Preset(String),
    /// Granules: the next page (true) or a new search.
    Granules { more: bool, page: usize },
}

/// The pane's choices, results and the request under way.
pub struct State {
    pub keyword: String,
    /// Only datasets in NASA's cloud (Earthdata Cloud).
    pub cloud: bool,
    pub collections: Option<Vec<Collection>>,
    collections_page: usize,
    collections_more: bool,
    /// The dataset chosen.
    pub collection: Option<Collection>,
    pub start: String,
    pub end: String,
    pub granules: Vec<Granule>,
    searched: bool,
    page: usize,
    more: bool,
    /// The footprints layer drawn for the granules listed.
    footprints: Option<u32>,
    /// GeoTIFFs added (their addresses), so Add says Added.
    added: Vec<String>,
    /// A token being typed.
    pub token: String,
    /// Remember the token on this computer (off unless ticked, or one is remembered already).
    pub remember: bool,
    /// The remembered token was asked for (once, when the pane is first shown).
    restoring: bool,
    /// Whether a token was remembered at the last frame, so `remember` follows a change.
    was_remembered: bool,
    /// Preview pictures, by address.
    thumbs: std::collections::HashMap<String, Thumb>,
    waiting: Option<(Asked, Answer)>,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        let (start, end) = earthdata::recent(30);
        State {
            keyword: String::new(),
            cloud: true,
            collections: None,
            collections_page: 1,
            collections_more: false,
            collection: None,
            start,
            end,
            granules: Vec::new(),
            searched: false,
            page: 1,
            more: false,
            footprints: None,
            added: Vec::new(),
            token: String::new(),
            remember: false,
            restoring: false,
            was_remembered: false,
            thumbs: Default::default(),
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

/// Lands the preview pictures fetched, as small textures.
fn poll_thumbs(app: &mut RustGisApp, ctx: &egui::Context) {
    let mut waiting = false;
    for (url, thumb) in app.ui.earthaccess.thumbs.iter_mut() {
        let Thumb::Loading(slot) = thumb else { continue };
        let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            waiting = true;
            continue;
        };
        // Browse images are up to 1000 px: kept at a card's size.
        *thumb = match r.and_then(|b| image::load_from_memory(&b).map_err(|e| e.to_string())) {
            Ok(img) => {
                let small = img.thumbnail(160, 160).to_rgba8();
                let size = [small.width() as usize, small.height() as usize];
                let img = egui::ColorImage::from_rgba_unmultiplied(size, small.as_raw());
                Thumb::Ready(ctx.load_texture(format!("earthaccess_thumb_{url}"), img, egui::TextureOptions::LINEAR))
            }
            Err(_) => Thumb::Failed,
        };
    }
    if waiting {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
}

/// Lands the answers.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    poll_thumbs(app, ctx);
    let Some((_, slot)) = &app.ui.earthaccess.waiting else { return };
    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some((asked, _)) = app.ui.earthaccess.waiting.take() else { return };
    let done = match asked {
        Asked::Collections { more, page } => r.and_then(|b| earthdata::parse_collections(&b)).map(|found| {
            let st = &mut app.ui.earthaccess;
            // The page counts once it has come.
            st.collections_page = page;
            st.collections_more = found.len() >= 20;
            match (&mut st.collections, more) {
                (Some(list), true) => list.extend(found),
                _ => st.collections = Some(found),
            }
        }),
        Asked::Preset(short) => r.and_then(|b| earthdata::parse_collections(&b)).and_then(|found| {
            let c = earthdata::best(&found, &short)
                .cloned()
                .ok_or_else(|| crate::i18n::fmt("NASA's CMR has no dataset named {name}", &[("name", &short)]))?;
            choose(app, c);
            Ok(())
        }),
        Asked::Granules { more, page } => r.and_then(|b| earthdata::parse_granules(&b)).and_then(|found| {
            let st = &mut app.ui.earthaccess;
            st.page = page;
            st.more = found.len() >= PAGE;
            st.searched = true;
            if !more {
                st.granules.clear();
            }
            st.granules.extend(found);
            draw_footprints(app)
        }),
    };
    if let Err(e) = done {
        app.ui.earthaccess.error = Some(e);
    }
    app.map.invalidate();
}

/// Draws the granules listed as one footprints layer, replacing the one drawn before.
fn draw_footprints(app: &mut RustGisApp) -> Result<(), String> {
    if let Some(id) = app.ui.earthaccess.footprints.take()
        && app.session.map().layer(id).is_some()
    {
        app.run("layer.remove", json!({"layer": id}))?;
    }
    let st = &app.ui.earthaccess;
    let fc = earthdata::footprints_geojson(&st.granules);
    if fc["features"].as_array().is_none_or(Vec::is_empty) {
        return Ok(());
    }
    let name = st.collection.as_ref().map(|c| format!("{} footprints", c.short_name)).unwrap_or_else(|| "Earthdata footprints".into());
    let v = app.run("stac.addFootprints", json!({"footprints": fc, "name": name}))?;
    app.ui.earthaccess.footprints = v["layer"].as_u64().and_then(|id| u32::try_from(id).ok());
    Ok(())
}

/// Takes the footprints layer drawn off the map, with the granules it showed.
fn clear_granules(app: &mut RustGisApp) {
    if let Some(id) = app.ui.earthaccess.footprints.take()
        && app.session.map().layer(id).is_some()
    {
        let _ = app.run("layer.remove", json!({"layer": id}));
    }
    let st = &mut app.ui.earthaccess;
    st.granules.clear();
    st.searched = false;
    st.more = false;
}

/// Picks a dataset: its granules are searched next.
fn choose(app: &mut RustGisApp, c: Collection) {
    clear_granules(app);
    app.ui.earthaccess.collection = Some(c);
}

fn search_collections(app: &mut RustGisApp, more: bool) {
    let st = &mut app.ui.earthaccess;
    st.error = None;
    let page = if more { st.collections_page + 1 } else { 1 };
    match rustgis_engine::cmd::earthdata::collections_query(
        &json!({"keyword": st.keyword, "cloudHosted": st.cloud, "limit": 20, "page": page}),
    ) {
        Ok(url) => {
            st.waiting = Some((Asked::Collections { more, page }, fetch(&url)));
        }
        Err(e) => st.error = Some(e),
    }
}

fn search_granules(app: &mut RustGisApp, more: bool) {
    let Some(c) = app.ui.earthaccess.collection.clone() else { return };
    let st = &app.ui.earthaccess;
    let page = if more { st.page + 1 } else { 1 };
    let p = json!({"collection": c.id, "start": st.start, "end": st.end, "limit": PAGE, "page": page});
    match rustgis_engine::cmd::earthdata::granules_query(&app.session, &p) {
        Ok(url) => {
            let st = &mut app.ui.earthaccess;
            st.error = None;
            st.waiting = Some((Asked::Granules { more, page }, fetch(&url)));
        }
        Err(e) => app.ui.earthaccess.error = Some(e),
    }
}

/// The box around a GeoJSON geometry's coordinates.
fn bounds(g: &Value) -> Option<[f64; 4]> {
    fn walk(v: &Value, b: &mut [f64; 4], depth: usize) {
        match v.as_array() {
            Some(a) if a.len() >= 2 && a.iter().all(Value::is_number) => {
                if let (Some(x), Some(y)) = (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
                    *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                }
            }
            Some(a) if depth < 8 => a.iter().for_each(|x| walk(x, b, depth + 1)),
            _ => {}
        }
    }
    let empty = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let coords = g.get("coordinates")?;
    let mut b = empty;
    walk(coords, &mut b, 0);
    // A footprint split at the antimeridian (its parts at both ±180) would span the world: the
    // widest part is zoomed to instead.
    let multi = matches!(g.get("type").and_then(Value::as_str), Some("MultiPolygon" | "MultiLineString"));
    if multi && b[0] <= -180.0 && b[2] >= 180.0 {
        let parts = coords.as_array().map(|p| {
            p.iter()
                .map(|part| {
                    let mut pb = empty;
                    walk(part, &mut pb, 1);
                    pb
                })
                .filter(|pb| pb.iter().all(|x| x.is_finite()))
                .max_by(|a, c| (a[2] - a[0]).total_cmp(&(c[2] - c[0])))
        });
        if let Some(Some(widest)) = parts {
            return Some(widest);
        }
    }
    b.iter().all(|x| x.is_finite()).then_some(b)
}

/// A file's name, without the granule's name it starts with (HLS's files all do).
fn short_name(url: &str, granule: &str) -> String {
    let file = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or(url);
    file.strip_prefix(granule).and_then(|r| r.strip_prefix('.')).filter(|r| !r.is_empty()).unwrap_or(file).to_string()
}

/// A result's card, as the other Web Data panes draw theirs.
fn card_frame(t: &crate::theme::Tokens) -> egui::Frame {
    egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6).inner_margin(egui::Margin::same(8))
}

/// Whether a browse address is a picture shown on a card (JPEG or PNG).
fn is_picture(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    [".jpg", ".jpeg", ".png"].iter().any(|x| path.ends_with(x))
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text).strong());
}

/// The NASA Earthaccess pane.
pub fn pane(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("NASA Earthdata through Earthaccess's search: find a dataset, then its granules where the map is, drawn as footprints. GeoTIFFs are added as COGs, read in place. Searching needs no account.")).weak());
    let busy = app.ui.earthaccess.waiting.is_some();
    let w = ui.available_width();
    auth(app, ui, w);

    heading(ui, tl!("Dataset"));
    let mut change = false;
    let mut chosen = None;
    let mut preset = None;
    let mut go = false;
    let mut more_sets = false;
    if let Some(c) = &app.ui.earthaccess.collection {
        card_frame(&t).show(ui, |ui| {
            ui.set_width(w - 18.0);
            ui.label(egui::RichText::new(format!("{} v{}", c.short_name, c.version)).strong());
            ui.label(&c.title);
            let cloud = if c.cloud_hosted { tl!("Earthdata Cloud") } else { "" };
            let sub: Vec<&str> = [c.data_center.as_str(), cloud].into_iter().filter(|s| !s.is_empty()).collect();
            ui.label(egui::RichText::new(sub.join(" · ")).small().color(t.text_dim));
        });
        change = ui.add_enabled(!busy, egui::Button::new(tl!("Change dataset"))).clicked();
    } else {
        let st = &mut app.ui.earthaccess;
        egui::ComboBox::from_id_salt("ea_preset")
            .selected_text(tl!("Popular datasets…"))
            .width(w - 8.0)
            .show_ui(ui, |ui| {
                for (short, label) in earthdata::PRESETS {
                    if ui.selectable_label(false, format!("{label} ({short})")).clicked() {
                        preset = Some((*short).to_string());
                    }
                }
            })
            .response
            .on_hover_text(tl!("Dataset"));
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut st.keyword).hint_text(tl!("Search datasets, e.g. GEDI biomass")).desired_width(w - 80.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            go = (ui.add_enabled(!busy, egui::Button::new(tl!("Search"))).clicked() || enter) && !busy && !st.keyword.trim().is_empty();
        });
        ui.checkbox(&mut st.cloud, tl!("Earthdata Cloud only"));
        if let Some(list) = &st.collections {
            if list.is_empty() {
                ui.label(egui::RichText::new(tl!("No datasets match.")).color(t.text_dim));
            } else {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt("Showing {n} datasets.", &[("n", &list.len().to_string())]))
                        .small()
                        .color(t.text_dim),
                );
            }
            for (i, c) in list.iter().enumerate() {
                ui.push_id(("ea_col", i), |ui| {
                    let text = egui::RichText::new(format!("{} v{} · {}", c.short_name, c.version, c.data_center)).strong();
                    let r = ui.add(egui::Button::new(text).frame(false)).on_hover_text(&c.summary);
                    ui.label(egui::RichText::new(&c.title).small());
                    if r.clicked() {
                        chosen = Some(c.clone());
                    }
                    ui.separator();
                });
            }
            if st.collections_more && ui.add_enabled(!busy, egui::Button::new(tl!("Load more"))).clicked() {
                more_sets = true;
            }
        }
    }
    if change {
        clear_granules(app);
        app.ui.earthaccess.collection = None;
    }
    if let Some(c) = chosen {
        choose(app, c);
    }
    if let Some(short) = preset {
        app.ui.earthaccess.error = None;
        let url = earthdata::collections_url("", Some(&short), false, 50, 1);
        app.ui.earthaccess.waiting = Some((Asked::Preset(short), fetch(&url)));
    }
    if go || more_sets {
        search_collections(app, more_sets);
    }

    if app.ui.earthaccess.collection.is_some() {
        heading(ui, tl!("Granules"));
        let mut search = false;
        egui::Grid::new("ea_dates").num_columns(2).spacing(egui::vec2(10.0, 6.0)).show(ui, |ui| {
            let st = &mut app.ui.earthaccess;
            let input_label = ui.label(tl!("Start date"));
            ui.add(egui::TextEdit::singleline(&mut st.start).hint_text("YYYY-MM-DD").desired_width(120.0)).labelled_by(input_label.id);
            ui.end_row();
            let input_label = ui.label(tl!("End date"));
            ui.add(egui::TextEdit::singleline(&mut st.end).hint_text("YYYY-MM-DD").desired_width(120.0)).labelled_by(input_label.id);
            ui.end_row();
        });
        let label = if busy && app.ui.earthaccess.granules.is_empty() { tl!("Searching…") } else { tl!("Search this view") };
        if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
            search = true;
        }
        if search {
            search_granules(app, false);
        }
    }
    status(app, ui, &t);
    granules(app, ui, &t, w);
}

/// The Earthdata Login section: the token kept for the session or remembered on this computer.
fn auth(app: &mut RustGisApp, ui: &mut egui::Ui, w: f32) {
    restore(app, ui.ctx());
    let expiry = earthdata::token_expiry();
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let title = match (earthdata::has_token(), expiry) {
        (true, Some(e)) if e < now => tl!("Earthdata Login: token expired"),
        (true, _) => tl!("Earthdata Login: token set"),
        (false, _) if earthdata::expired_forgotten() => tl!("Earthdata Login: token expired"),
        (false, _) => tl!("Earthdata Login: no token"),
    };
    egui::CollapsingHeader::new(title).id_salt("ea_auth").show(ui, |ui| {
        if cfg!(target_arch = "wasm32") {
            ui.label(egui::RichText::new(tl!("Searching works here, but NASA's data servers don't let web pages read their files (no CORS), so GeoTIFFs and downloads of protected data need the desktop app. Download opens a file in a new tab, where Earthdata Login asks you to sign in.")).weak());
            return;
        }
        let remembered = earthdata::is_remembered();
        // The box follows whether a token is remembered (read back, saved or forgotten); a token
        // replacing a remembered one stays remembered unless it is unticked.
        if remembered != app.ui.earthaccess.was_remembered {
            app.ui.earthaccess.was_remembered = remembered;
            app.ui.earthaccess.remember = remembered;
        }
        if earthdata::has_token() {
            let text = match expiry {
                Some(e) => crate::i18n::fmt("The token expires on {date}.", &[("date", &earthdata::date_of(e))]),
                None => tl!("A token is set.").to_string(),
            };
            ui.label(egui::RichText::new(text).small());
            if remembered {
                ui.label(egui::RichText::new(tl!("It is remembered on this computer.")).small());
            }
            if ui.button(tl!("Remove token")).clicked() {
                let r = app.run("earthdata.logout", Value::Null);
                logged_in(app, r);
            }
        } else if earthdata::expired_forgotten() {
            ui.label(egui::RichText::new(tl!("The token expired and was forgotten. Paste a new one.")).small());
        }
        ui.label(egui::RichText::new(tl!("Reading a granule's files needs an Earthdata Login token, which works for every NASA DAAC. It is kept until RustGIS closes unless you tick Remember, never in the project, and sent only to NASA's Earthdata data servers.")).small().weak());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.ui.earthaccess.token).password(true).hint_text(tl!("Paste a token")).desired_width(w - 80.0));
            if ui.button(tl!("Save")).clicked() && !app.ui.earthaccess.token.trim().is_empty() {
                let token = std::mem::take(&mut app.ui.earthaccess.token);
                let remember = app.ui.earthaccess.remember && app.services.secrets.is_some();
                let r = app.run("earthdata.login", json!({"token": token, "remember": remember}));
                logged_in(app, r);
            }
        });
        if app.services.secrets.is_some() {
            ui.checkbox(&mut app.ui.earthaccess.remember, tl!("Remember on this computer")).on_hover_text(tl!(
                "Keeps the token in the system's credential store, so it is there when RustGIS starts again. Unticked, a token remembered before is deleted when you save another."
            ));
        }
        ui.hyperlink_to(tl!("Generate a token"), earthdata::TOKEN_PAGE);
    });
}

/// Reads the token remembered on this computer, once, off the frame: the system may ask for its
/// store to be unlocked.
pub(crate) fn restore(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.ui.earthaccess.restoring || app.services.secrets.is_none() {
        return;
    }
    app.ui.earthaccess.restoring = true;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let ctx = ctx.clone();
        let read = std::thread::Builder::new().name("earthdata-token".into()).spawn(move || {
            earthdata::restore();
            ctx.request_repaint();
        });
        if read.is_err() {
            app.ui.earthaccess.restoring = false;
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = ctx;
}

/// Shows what a login or logout said: its error, or the warning it gave.
fn logged_in(app: &mut RustGisApp, r: Result<Value, String>) {
    app.ui.earthaccess.error = match r {
        Ok(v) => v.get("warning").and_then(Value::as_str).map(str::to_string),
        Err(e) => Some(e),
    };
}

fn status(app: &RustGisApp, ui: &mut egui::Ui, t: &crate::theme::Tokens) {
    if app.ui.earthaccess.waiting.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Asking NASA's CMR…"));
        });
    }
    if let Some(e) = &app.ui.earthaccess.error {
        ui.label(egui::RichText::new(e).color(t.red));
    }
    if let Some(i) = &app.ui.earthaccess.info {
        ui.label(egui::RichText::new(i).weak());
    }
}

/// The granules found, as cards: name, date · size · cloud · files, then Add, Files, Zoom and
/// Details.
fn granules(app: &mut RustGisApp, ui: &mut egui::Ui, t: &crate::theme::Tokens, w: f32) {
    let st = &app.ui.earthaccess;
    if !st.searched {
        return;
    }
    if st.granules.is_empty() {
        ui.label(egui::RichText::new(tl!("No granules in this view and date range.")).color(t.text_dim));
        return;
    }
    ui.label(
        egui::RichText::new(crate::i18n::fmt("Showing {n} granules, newest first.", &[("n", &st.granules.len().to_string())]))
            .small()
            .color(t.text_dim),
    );
    let busy = st.waiting.is_some();
    let web = cfg!(target_arch = "wasm32");
    let mut add = None;
    let mut zoom = None;
    let mut wanted = Vec::new();
    for (i, g) in st.granules.iter().enumerate() {
        let thumb = g.browse.iter().find(|u| is_picture(u)).and_then(|u| match st.thumbs.get(u) {
            Some(Thumb::Ready(tex)) => Some(tex.clone()),
            None => {
                wanted.push(u.clone());
                None
            }
            _ => None,
        });
        ui.push_id(("ea_g", i), |ui| {
            card_frame(t).show(ui, |ui| {
                ui.set_width(w - 18.0);
                ui.horizontal_top(|ui| {
                    if let Some(tex) = &thumb {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(64.0, 64.0)).corner_radius(4));
                    }
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&g.title).strong()).truncate()).on_hover_text(&g.title);
                        let mut parts = vec![g.time_start.replace('T', " ").chars().take(16).collect::<String>()];
                        if let Some(mb) = g.size_mb {
                            parts.push(if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.1} MB") });
                        }
                        if let Some(c) = g.cloud_cover {
                            parts.push(crate::i18n::fmt("{value}% cloud", &[("value", &format!("{c:.0}"))]));
                        }
                        if g.data.len() > 1 {
                            parts.push(crate::i18n::fmt("{n} files", &[("n", &g.data.len().to_string())]));
                        }
                        ui.label(egui::RichText::new(parts.join(" · ")).small().color(t.text_dim));
                        ui.horizontal_wrapped(|ui| {
                            // The first GeoTIFF, added in one click.
                            let tiffs: Vec<&String> = g.tiffs().collect();
                            if let Some(first) = tiffs.first()
                                && !web
                            {
                                let added = st.added.contains(first);
                                let label = if added { tl!("Added") } else { tl!("Add") };
                                if ui
                                    .add_enabled(!added && !busy, egui::Button::new(label).small())
                                    .on_hover_text(tl!("Add this GeoTIFF to the map"))
                                    .clicked()
                                {
                                    add = Some(((*first).clone(), g.title.clone()));
                                }
                            }
                            if let Some(first) = g.data.first() {
                                ui.hyperlink_to(egui::RichText::new(tl!("Download")).small(), first).on_hover_text(first);
                            }
                            if g.geometry.is_some() && ui.add(egui::Button::new(tl!("Zoom")).small()).clicked() {
                                zoom = g.geometry.as_ref().and_then(bounds);
                            }
                            ui.hyperlink_to(egui::RichText::new(tl!("Details")).small(), g.details_url())
                                .on_hover_text(tl!("Open this granule in Earthdata Search"));
                        });
                    });
                });
                if g.data.len() > 1 {
                    egui::CollapsingHeader::new(crate::i18n::fmt("Files ({n})", &[("n", &g.data.len().to_string())]))
                        .id_salt(("ea_files", i))
                        .show(ui, |ui| {
                            for (k, url) in g.data.iter().enumerate() {
                                ui.push_id(k, |ui| {
                                    ui.horizontal(|ui| {
                                        let name = short_name(url, &g.title);
                                        ui.add(egui::Label::new(egui::RichText::new(&name).small()).truncate()).on_hover_text(url);
                                        if earthdata::is_tiff(url) && !web {
                                            let added = st.added.contains(url);
                                            let label = if added { tl!("Added") } else { tl!("Add") };
                                            if ui.add_enabled(!added && !busy, egui::Button::new(label).small()).clicked() {
                                                add = Some((url.clone(), format!("{} {name}", g.title)));
                                            }
                                        }
                                        ui.hyperlink_to(egui::RichText::new(tl!("Download")).small(), url);
                                    });
                                });
                            }
                        });
                }
            });
        });
    }
    let more = st.more && ui.add_enabled(!busy, egui::Button::new(tl!("Load more"))).clicked();
    // Preview pictures of the granules shown, fetched once each.
    let st = &mut app.ui.earthaccess;
    for u in wanted {
        if st.thumbs.len() >= MAX_THUMBS {
            break;
        }
        let slot: Answer = Slot::default();
        let land = slot.clone();
        cache::download_limited(&u, MAX_THUMB_BYTES, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
        st.thumbs.insert(u, Thumb::Loading(slot));
    }
    if let Some(b) = zoom {
        let _ = app.run("map.zoomToExtent", json!({"extent": b}));
    }
    if let Some((url, name)) = add {
        app.ui.earthaccess.error = None;
        // NASA's servers turn a file away without a token: say so here, before trying.
        if earthdata::is_data_host(&url) && !earthdata::has_token() {
            app.ui.earthaccess.error =
                Some(tl!("Add an Earthdata Login token (Earthdata Login, above) to put NASA GeoTIFFs on the map.").to_string());
            return;
        }
        match app.run("ui.addUrl", json!({"url": url, "name": name, "zoom": true})) {
            Ok(_) => app.ui.earthaccess.added.push(url),
            Err(e) => app.ui.earthaccess.error = Some(e),
        }
    }
    if more {
        search_granules(app, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first frame reads the remembered token, only where there is a credential store.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_first_frame_reads_the_remembered_token() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        // The token and its store are process-wide: this scenario runs alone.
        if crate::isolate_account_test("earthaccessui::tests::the_first_frame_reads_the_remembered_token") {
            return;
        }
        let ctx = egui::Context::default();
        let mut none = RustGisApp::with_sample();
        none.logic(&ctx);
        assert!(!none.ui.earthaccess.restoring && !earthdata::has_token(), "no store, nothing read");
        let store = std::sync::Arc::new(MemorySecrets::new());
        store.save(earthdata::ACCOUNT, &Secret::new("synthetic-remembered")).unwrap();
        let mut app = RustGisApp::with_sample().with_services(crate::Services { secrets: Some(store), ..Default::default() });
        app.logic(&ctx);
        assert!(app.ui.earthaccess.restoring, "the first frame starts the read");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !earthdata::has_token() {
            assert!(std::time::Instant::now() < deadline, "the token was never read");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        earthdata::set_secret_store(None);
    }

    #[test]
    fn bounds_and_names() {
        let g = json!({"type": "MultiPolygon", "coordinates": [[[[1.0, 2.0], [3.0, -4.0], [0.5, 9.0]]]]});
        assert_eq!(bounds(&g), Some([0.5, -4.0, 3.0, 9.0]));
        assert_eq!(bounds(&json!({"coordinates": []})), None);
        // Split at the antimeridian: its widest part, not the whole world.
        let across = json!({"type": "MultiPolygon", "coordinates": [
            [[[170.0, -10.0], [180.0, -10.0], [180.0, 10.0], [170.0, 10.0], [170.0, -10.0]]],
            [[[-180.0, -10.0], [-175.0, -10.0], [-175.0, 10.0], [-180.0, 10.0], [-180.0, -10.0]]]]});
        assert_eq!(bounds(&across), Some([170.0, -10.0, 180.0, 10.0]));
        assert_eq!(bounds(&json!({"coordinates": "x"})), None);
        assert_eq!(short_name("https://x/p/HLS.L30.T1.v2.0.B04.tif", "HLS.L30.T1.v2.0"), "B04.tif");
        assert_eq!(short_name("https://x/p/other.tif?x=1", "HLS"), "other.tif");
    }
}
