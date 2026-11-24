//! Home › Add › Add Data › Zarr…: a store's variables, a slice picked by its other dimensions (a slider for
//! time), added as a raster layer. A store by web address is read off the frame through the tile
//! cache's downloads (a thread natively, fetch on the web), a few files at a time; a folder (the
//! desktop) through the commands.

use std::collections::HashMap;

use egui::vec2;
use rustgis_engine::data::raster::{Crs, zarr};
use rustgis_engine::render::rustgis_tiles::cache;
use serde_json::{Value, json};

use crate::{RustGisApp, Slot, i18n};

type Answer = Slot<Result<Vec<u8>, String>>;

/// Files fetched at once.
const AT_ONCE: usize = 6;

/// What the dialog is fetching.
#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    /// The metadata files.
    Meta,
    /// The coordinates that label the dimensions.
    Labels,
    /// A slice's chunks, to be added as `name` once they land.
    Read(Box<(zarr::Plan, zarr::Selection, String)>),
}

/// The dialog's store and choices, and the fetches under way.
#[derive(Default)]
pub struct State {
    pub address: String,
    /// What the store is called in its layers' names, when it has a better name than its file's
    /// (a Web Data dataset's title).
    pub title: Option<String>,
    pub crs: String,
    /// The store's root: a web address or a folder, as connected.
    root: String,
    web: bool,
    /// The store's address came from a document read from the internet (a Search Open Data
    /// result): its files are fetched only from there ([`connect_within`]).
    public: bool,
    store: Option<zarr::Store>,
    /// `raster.zarrInfo`'s answer: variables, dimensions and their labels.
    info: Option<Value>,
    pub variable: usize,
    /// The steps picked, by dimension name.
    steps: HashMap<String, u64>,
    phase: Phase,
    queue: Vec<String>,
    waiting: Vec<(String, Answer)>,
    got: HashMap<String, Vec<u8>>,
    /// Files of this batch refused with 403: all metadata refused is access denied, not a store
    /// without metadata.
    forbidden: usize,
    pub error: Option<String>,
    /// Add the variable as a layer that follows the time slider (opengeos/RustGIS#554).
    pub follow: bool,
}

impl State {
    fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    fn fetch(&mut self, keys: Vec<String>, phase: Phase) {
        self.queue = keys;
        self.waiting.clear();
        self.got.clear();
        self.forbidden = 0;
        self.phase = phase;
    }
}

fn take(slot: &Answer) -> Option<Result<Vec<u8>, String>> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Moves fetches along, and acts when a batch is in.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let st = &mut app.ui.zarr;
    if !st.busy() {
        return;
    }
    // Landed answers: a missing file (404, or 403 as S3 says it, by the HTTP status) is left out.
    let mut failed = None;
    let mut forbidden = 0;
    st.waiting.retain(|(k, slot)| match take(slot) {
        Some(Ok(b)) => {
            st.got.insert(k.clone(), b);
            false
        }
        Some(Err(e)) => {
            if !rustgis_engine::cmd::zarr::is_absent(&e) {
                failed = Some(format!("{k}: {e}"));
            } else if cache::http_status(&e) == Some(403) {
                forbidden += 1;
            }
            false
        }
        None => true,
    });
    st.forbidden += forbidden;
    if let Some(e) = failed {
        st.error = Some(e);
        st.phase = Phase::Idle;
        st.queue.clear();
        st.waiting.clear();
        return;
    }
    while st.waiting.len() < AT_ONCE
        && let Some(k) = st.queue.pop()
    {
        let slot: Answer = Slot::default();
        let land = slot.clone();
        let url = format!("{}/{k}", st.root);
        let done: cache::Done = Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r));
        if st.public {
            cache::download_public(&url, done)
        } else {
            cache::download(&url, done)
        }
        st.waiting.push((k, slot));
    }
    if !st.waiting.is_empty() || !st.queue.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return;
    }
    // The batch is in.
    match std::mem::take(&mut st.phase) {
        Phase::Idle => {}
        Phase::Meta => {
            let docs: Vec<(String, Vec<u8>)> = std::mem::take(&mut st.got).into_iter().collect();
            if docs.is_empty() && st.forbidden > 0 {
                st.error =
                    Some(i18n::fmt("{url}: access denied (HTTP 403)", &[("url", &rustgis_engine::sources::address_in_message(&st.root))]));
                return;
            }
            match zarr::parse_store(&docs) {
                Ok(store) => {
                    let keys = zarr::info_keys(&store);
                    st.store = Some(store);
                    st.fetch(keys, Phase::Labels);
                }
                Err(e) => st.error = Some(e.to_string()),
            }
        }
        Phase::Labels => {
            if let Some(store) = &st.store {
                let info = zarr::info(store, &st.got);
                // A Web Data dataset opens on its air temperature when it has one (a weather
                // model's most looked-at variable), else its first.
                st.variable = if st.title.is_some() { first_temperature(&info) } else { 0 };
                st.info = Some(info);
                st.steps.clear();
            }
            st.got.clear();
        }
        Phase::Read(job) => {
            let (plan, sel, name) = *job;
            let got = std::mem::take(&mut st.got);
            let typed = st.crs.trim().to_string();
            let crs = match typed.as_str() {
                "" => Ok(None),
                c => Crs::parse(c).map(Some),
            };
            let raster = crs.map_err(|e| e.to_string()).and_then(|crs| {
                let store = st.store.as_ref().ok_or("the store is gone")?;
                let mut r = zarr::assemble(store, &plan, &got, &name, crs).map_err(|e| e.to_string())?;
                // What a typed system says, when the slice is in it, as for a folder's slice.
                let note = (!typed.is_empty()).then(|| zarr::typed_crs_note(store, &sel.variable, &r, &typed)).flatten();
                if let Some(note) = note
                    && !r.lost.contains(&note)
                {
                    r.lost.push(note);
                }
                // What reading the store said: its GeoTIFF doesn't carry it, so it's said here.
                Ok::<_, String>((zarr::write_project_slice(&r).map_err(|e| e.to_string())?, r.lost))
            });
            match raster {
                Ok((tif, lost)) => {
                    let id = app.session.download_started();
                    app.session.download_landed(id, tif);
                    let index: serde_json::Map<String, Value> = sel.index.iter().map(|(d, i)| (d.clone(), json!(i))).collect();
                    let root = app.ui.zarr.root.clone();
                    let mut p = json!({"url": root, "variable": sel.variable, "index": index, "ran": id, "name": name});
                    // The system typed for a store that doesn't say: the layer keeps it for its
                    // series, as a folder's slice does.
                    if !typed.is_empty() {
                        p["crs"] = json!(typed);
                    }
                    // A failure here (the read handed over, then refused) is said in the dialog.
                    match app.run("layer.addZarr", p) {
                        Ok(_) => {
                            // As a layer's read notices are said (`raster::read_notices`).
                            let added =
                                app.session.active_layer.and_then(|id| app.session.map().layer(id)).map_or(name, |l| l.name.clone());
                            for loss in lost {
                                app.session.notify(i18n::fmt("{layer}: {notice}", &[("layer", &added), ("notice", &loss)]), false);
                            }
                            app.map.invalidate();
                        }
                        Err(e) => app.ui.zarr.error = Some(e),
                    }
                }
                Err(e) => app.ui.zarr.error = Some(e),
            }
        }
    }
}

/// Connects to the store at the dialog's address, the user's own: its files are fetched wherever
/// its name points.
pub(crate) fn connect(app: &mut RustGisApp) {
    connect_within(app, false);
}

/// [`connect`], to a store a document read from the internet named when `public`: its files are
/// fetched only from there, however its name resolves.
pub(crate) fn connect_within(app: &mut RustGisApp, public: bool) {
    let st = &mut app.ui.zarr;
    let a = st.address.trim().trim_end_matches('/').to_string();
    st.error = None;
    st.info = None;
    st.store = None;
    if a.is_empty() {
        st.error = Some(crate::tl!("Give a Zarr store's address or folder").to_string());
        return;
    }
    let lower = a.to_ascii_lowercase();
    st.root = a.clone();
    st.web = lower.starts_with("http://") || lower.starts_with("https://");
    if st.web {
        let keys = zarr::METADATA_KEYS.iter().map(|k| k.to_string()).collect();
        // Set with the batch it is for, so a connect that starts none leaves a batch under way as
        // it was.
        st.public = public;
        st.fetch(keys, Phase::Meta);
        return;
    }
    if cfg!(target_arch = "wasm32") {
        st.error = Some(crate::tl!("In the web app, give the store's web address (https://…)").to_string());
        return;
    }
    match app.run("raster.zarrInfo", json!({"path": a})) {
        Ok(v) => {
            let st = &mut app.ui.zarr;
            st.info = Some(v);
            st.variable = 0;
            st.steps.clear();
        }
        Err(e) => app.ui.zarr.error = Some(e),
    }
}

/// The first variable named for temperature (`temperature_2m`), or the first.
fn first_temperature(info: &Value) -> usize {
    info["variables"]
        .as_array()
        .and_then(|vs| vs.iter().position(|v| v["name"].as_str().is_some_and(|n| n.starts_with("temperature"))))
        .unwrap_or(0)
}

/// The dialog. Returns whether it should close.
pub fn dialog(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(
        egui::RichText::new(crate::tl!(
            "A Zarr store (format 2 or 3) by its web address, with consolidated metadata, or a folder on this computer. A variable's map is read for the steps picked of its other dimensions, such as time."
        ))
        .weak(),
    );
    let mut go = false;
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.ui.zarr.address).hint_text("https://…/store.zarr").desired_width(380.0));
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button(crate::tl!("Folder…")).clicked()
            && let Some(d) = crate::filedialog::FileDialog::new().pick_folder()
        {
            app.ui.zarr.address = d.display().to_string();
            go = true;
        }
        go |= ui.add_enabled(!app.ui.zarr.busy(), egui::Button::new(crate::tl!("Connect"))).clicked();
    });
    if go {
        app.ui.zarr.title = None;
        connect(app);
    }
    picker(app, ui);
    // The dialog stays open, to add other steps.
    false
}

/// The store's variables and their other dimensions' steps, and Add to Map: the dialog's body, and
/// the Web Data pane's for a dataset it opens (Dynamical).
pub fn picker(app: &mut RustGisApp, ui: &mut egui::Ui) {
    // Beside the dimensions' names.
    let wide = (ui.available_width() - 230.0).clamp(120.0, 380.0);
    // A Web Data dataset (a forecast or analysis archive: `title` is set while the Web Data pane
    // has it open, cleared when it closes) opens on its latest complete time, as GeoLibre's does:
    // the step before the newest, which may still be being written (an archive whose newest is
    // complete opens one step early). A store added by hand opens on its first.
    let latest = app.ui.zarr.title.is_some();
    let busy = app.ui.zarr.busy();
    if busy {
        ui.horizontal(|ui| {
            ui.spinner();
            let n = app.ui.zarr.queue.len() + app.ui.zarr.waiting.len();
            ui.label(i18n::fmt("Reading the store… ({n} files to go)", &[("n", &n.to_string())]));
        });
    }
    if let Some(e) = &app.ui.zarr.error {
        ui.label(egui::RichText::new(e).color(ui.visuals().error_fg_color));
    }
    let Some(info) = app.ui.zarr.info.clone() else { return };
    let vars: Vec<Value> = info["variables"].as_array().cloned().unwrap_or_default();
    if vars.is_empty() {
        ui.label(crate::tl!("The store has no variables with two or more dimensions."));
        return;
    }
    let st = &mut app.ui.zarr;
    st.variable = st.variable.min(vars.len() - 1);
    let title = |v: &Value| {
        let mut t = v["name"].as_str().unwrap_or("").to_string();
        if let Some(l) = v["longName"].as_str() {
            t = format!("{t} — {l}");
        }
        if let Some(u) = v["units"].as_str() {
            t = format!("{t} ({u})");
        }
        t
    };
    egui::Grid::new("zarr").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(crate::tl!("Variable"));
        egui::ComboBox::from_id_salt("zarr_var")
            .selected_text(title(&vars[st.variable]))
            .width(wide)
            .truncate()
            .show_ui(ui, |ui| {
                for (i, v) in vars.iter().enumerate() {
                    if ui.selectable_value(&mut st.variable, i, title(v)).changed() {
                        st.steps.clear();
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let var = &vars[st.variable];
        for d in var["dims"].as_array().into_iter().flatten().filter(|d| d["map"] != json!(true)) {
            let (name, size) = (d["name"].as_str().unwrap_or("").to_string(), d["size"].as_u64().unwrap_or(1).max(1));
            ui.label(name.clone());
            let timely = name.contains("time") && !name.contains("lead");
            let step = st.steps.entry(name).or_insert(if latest && timely { size.saturating_sub(2) } else { 0 });
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(step, 0..=size - 1).show_value(true));
                let label = d["labels"]
                    .get(*step as usize)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| Some(i18n::fmt("{first} … {last}", &[("first", d["first"].as_str()?), ("last", d["last"].as_str()?)])));
                if let Some(l) = label {
                    ui.label(egui::RichText::new(l).weak());
                }
            });
            ui.end_row();
        }
        let input_label = ui.label(crate::tl!("Coordinate system"));
        ui.add(egui::TextEdit::singleline(&mut st.crs).hint_text(crate::tl!("for a store that doesn't say")).desired_width(200.0))
            .labelled_by(input_label.id);
        ui.end_row();
    });
    if !vars[st.variable]["readable"].as_bool().unwrap_or(true) {
        ui.label(egui::RichText::new(vars[st.variable]["why"].as_str().unwrap_or("")).color(ui.visuals().warn_fg_color));
    }
    let var = vars[st.variable]["name"].as_str().unwrap_or("").to_string();
    let mut index: Vec<(String, u64)> = st.steps.iter().map(|(d, i)| (d.clone(), *i)).collect();
    index.sort();
    // The steps' labels name the layer (`tas 1850-01-15`).
    let labels: Vec<String> = vars[st.variable]["dims"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["map"] != json!(true))
        .filter_map(|d| {
            let step = st.steps.get(d["name"].as_str()?).copied().unwrap_or(0);
            d["labels"].get(step as usize).and_then(Value::as_str).map(str::to_string)
        })
        .collect();
    // A variable with a dimension besides the map's can follow the time slider (desktop).
    let timed = vars[st.variable]["dims"].as_array().into_iter().flatten().any(|d| d["map"] != json!(true));
    // A variable with only the map's dimensions can't follow: the box is gone, so is its choice.
    if !timed {
        st.follow = false;
    }
    if cfg!(not(target_arch = "wasm32")) && timed {
        ui.checkbox(&mut st.follow, crate::tl!("Follow the time slider")).on_hover_text(crate::tl!(
            "The layer shows the step the time slider is at, and turns the slider on over the variable's dates."
        ));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, egui::Button::new(crate::tl!("Add to Map"))).clicked() {
            add(app, zarr::Selection { variable: var.clone(), index, window: None }, &labels);
        }
    });
}

/// Adds the slice: through the command for a folder, or fetches its chunks first for a web store.
fn add(app: &mut RustGisApp, sel: zarr::Selection, labels: &[String]) {
    let st = &mut app.ui.zarr;
    st.error = None;
    let name = match &st.title {
        Some(t) => format!("{t} {}", rustgis_engine::cmd::zarr::layer_name("", &sel, labels)),
        None => rustgis_engine::cmd::zarr::layer_name(&st.root, &sel, labels),
    };
    // Following the time slider: the engine reads each step itself, a folder's or a web store's.
    if st.follow && cfg!(not(target_arch = "wasm32")) {
        let index: serde_json::Map<String, Value> = sel.index.iter().map(|(d, i)| (d.clone(), json!(i))).collect();
        let key = if st.web { "url" } else { "path" };
        let mut p = json!({key: st.root, "variable": sel.variable, "index": index, "time": true});
        if let Some(t) = &st.title {
            p["name"] = json!(format!("{t} {}", sel.variable));
        }
        if !st.crs.trim().is_empty() {
            p["crs"] = json!(st.crs.trim());
        }
        match app.run("layer.addZarr", p) {
            Ok(_) => app.map.invalidate(),
            Err(e) => app.ui.zarr.error = Some(e),
        }
        return;
    }
    if !st.web {
        let index: serde_json::Map<String, Value> = sel.index.iter().map(|(d, i)| (d.clone(), json!(i))).collect();
        let mut p = json!({"path": st.root, "variable": sel.variable, "index": index, "name": name});
        if !st.crs.trim().is_empty() {
            p["crs"] = json!(st.crs.trim());
        }
        match app.run("layer.addZarr", p) {
            Ok(_) => app.map.invalidate(),
            Err(e) => app.ui.zarr.error = Some(e),
        }
        return;
    }
    let Some(store) = &st.store else { return };
    match zarr::plan(store, &sel) {
        Ok(plan) => {
            let keys = plan.keys.clone();
            st.fetch(keys, Phase::Read(Box::new((plan, sel, name))));
        }
        Err(e) => st.error = Some(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{Project, Session};

    /// A store a document read from the internet named (a Search Open Data result) has its files
    /// fetched only from there; one the user names, wherever its name points.
    #[test]
    fn a_store_a_document_named_is_read_only_from_the_internet() {
        use rustgis_engine::render::rustgis_tiles::{
            address::{Origin, Reach},
            cache,
        };
        for public in [true, false] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                log.lock().unwrap().push(ceiling);
                let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
                done(Ok(cache::Fetched { bytes: b"{}".to_vec(), origin }));
            });
            let mut app = RustGisApp::new(Session::new(Project::default()));
            app.ui.zarr.address = format!("https://store-{public}.example/data.zarr");
            connect_within(&mut app, public);
            poll(&mut app, &egui::Context::default());
            let asked = asked.lock().unwrap().clone();
            let want = if public { Reach::Public } else { Reach::Local };
            assert!(!asked.is_empty() && asked.iter().all(|c| *c == want), "{asked:?}");
        }
    }

    /// A connect that starts no batch (a blank address) leaves the reach of a batch under way as it
    /// was: a store a document named stays read only from the internet.
    #[test]
    fn a_connect_that_starts_nothing_leaves_a_batch_under_way_as_it_was() {
        use rustgis_engine::render::rustgis_tiles::{
            address::{Origin, Reach},
            cache,
        };
        let _turn = crate::fetching::turn();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            log.lock().unwrap().push(ceiling);
            let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
            done(Ok(cache::Fetched { bytes: b"{}".to_vec(), origin }));
        });
        let mut app = RustGisApp::new(Session::new(Project::default()));
        app.ui.zarr.address = "https://store-batch.example/data.zarr".into();
        connect_within(&mut app, true);
        // Another connect, with nothing to connect to, before the batch is fetched.
        app.ui.zarr.address = String::new();
        connect(&mut app);
        poll(&mut app, &egui::Context::default());
        let asked = asked.lock().unwrap().clone();
        assert!(!asked.is_empty() && asked.iter().all(|c| *c == Reach::Public), "{asked:?}");
    }

    /// A slice keeps its geographic system through its GeoTIFF handoff: another body's (an ESRI PE
    /// citation in the keys), and the Earth's other datums, retain their datum through `ran`.
    #[test]
    fn a_geographic_slice_keeps_its_system_through_handoff() {
        let coords = |dim: &str| {
            json!({
                "zarr_format": 3, "node_type": "array", "shape": [2], "data_type": "float64",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2]}},
                "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                "fill_value": 0, "dimension_names": [dim]
            })
        };
        for (def, expected) in
            [("+proj=longlat +a=3396190 +b=3396190 +no_defs", None), ("EPSG:4230", Some(Crs::Epsg(4230))), ("EPSG:4326", Some(Crs::LonLat))]
        {
            let elev = json!({
                "zarr_format": 3, "node_type": "array", "shape": [2, 2], "data_type": "int16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 2]}},
                "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                "fill_value": 0, "dimension_names": ["lat", "lon"], "attributes": {"crs": def}
            });
            let root = json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {
                "kind": "inline", "metadata": {"elev": elev, "lat": coords("lat"), "lon": coords("lon")}
            }});
            let store = zarr::parse_store(&[("zarr.json".into(), root.to_string().into_bytes())]).unwrap();
            let sel = zarr::Selection { variable: "elev".into(), index: vec![], window: None };
            let plan = zarr::plan(&store, &sel).unwrap();
            let mut app = RustGisApp::new(Session::new(Project::default()));
            let st = &mut app.ui.zarr;
            st.root = "https://example.invalid/geographic.zarr".into();
            st.web = true;
            st.store = Some(store);
            let f64s = |values: [f64; 2]| values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
            st.got = HashMap::from([
                ("elev/c/0/0".into(), [1i16, 2, 3, 4].iter().flat_map(|v| v.to_le_bytes()).collect()),
                ("lat/c/0".into(), f64s([43.1, 43.0])),
                ("lon/c/0".into(), f64s([-79.1, -79.0])),
            ]);
            st.phase = Phase::Read(Box::new((plan, sel, "geographic".into())));
            poll(&mut app, &egui::Context::default());
            assert_eq!(app.ui.zarr.error, None, "{def}");
            let layer = app.session.map().layers.first().unwrap();
            let raster = layer.raster.as_ref().unwrap().data.as_ref().unwrap();
            assert_eq!(raster.bands[0].data, [1.0, 2.0, 3.0, 4.0], "{def}");
            match expected {
                Some(expected) => assert_eq!(raster.crs, expected, "{def}"),
                // Mars by its sphere: kept as another body's longitude/latitude.
                None => assert!(raster.crs.is_planetary() && raster.crs.is_geographic(), "{def}: {:?}", raster.crs),
            }
        }
    }

    /// A slice the dialog reads itself (a store by address, or any store in the web app) says what
    /// reading the store said, which its GeoTIFF doesn't carry, and hands a system typed for it to
    /// the layer. UTM zone 17N on NAD 27 bound to WGS 84 by the NTv2 grid `conus` says once that
    /// the grid isn't applied: in the store, typed for a store that doesn't say, or both. Typed for
    /// a store that gives another system (WGS 84's UTM zone 17N), it isn't used and says nothing,
    /// and WGS 84's UTM zone 17N alone says nothing (#1678).
    #[test]
    fn a_read_slice_says_what_reading_its_store_said() {
        let wgs84 = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]"#;
        let utm = r#"PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["UTM zone 17N",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",-81,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]],ID["EPSG",16017]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]"#;
        let bound = format!(
            r#"BOUNDCRS[SOURCECRS[{utm}],TARGETCRS[{wgs84}],ABRIDGEDTRANSFORMATION["unknown to WGS84",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","conus",ID["EPSG",8656]]]]"#
        );
        let coords = |dim: &str| json!({"zarr_format": 3, "node_type": "array", "shape": [2], "data_type": "float64", "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2]}}, "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}], "fill_value": 0, "dimension_names": [dim]});
        let f64s = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        // The notice, then whether it is the typed system's own ("the definition's").
        for (attrs, typed, expected, typed_said) in [
            (json!({"crs_wkt": bound}), "", 1, 0),
            (json!({}), bound.as_str(), 1, 1),
            (json!({"crs_wkt": bound}), bound.as_str(), 1, 0),
            (json!({"epsg_code": 32617}), bound.as_str(), 0, 0),
            (json!({"epsg_code": 32617}), "", 0, 0),
            (json!({}), "EPSG:32617", 0, 0),
        ] {
            let elev = json!({
                "zarr_format": 3, "node_type": "array", "shape": [2, 2], "data_type": "int16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 2]}},
                "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
                "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                "fill_value": 0, "dimension_names": ["y", "x"], "attributes": attrs
            });
            let root = json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
                "elev": elev, "y": coords("y"), "x": coords("x"),
            }}});
            let store = zarr::parse_store(&[("zarr.json".into(), root.to_string().into_bytes())]).unwrap();
            let sel = zarr::Selection { variable: "elev".into(), index: vec![], window: None };
            let plan = zarr::plan(&store, &sel).unwrap();
            let mut app = RustGisApp::new(Session::new(Project::default()));
            let st = &mut app.ui.zarr;
            st.root = "https://example.invalid/dem.zarr".into();
            st.web = true;
            st.crs = typed.into();
            st.store = Some(store);
            st.got = HashMap::from([
                ("elev/c/0/0".to_string(), [1i16, 2, 3, 4].iter().flat_map(|v| v.to_le_bytes()).collect()),
                ("y/c/0".to_string(), f64s(&[4_800_100.0, 4_800_000.0])),
                ("x/c/0".to_string(), f64s(&[500_000.0, 500_100.0])),
            ]);
            st.phase = Phase::Read(Box::new((plan, sel, "dem".into())));
            let mark = app.session.notice_mark();
            poll(&mut app, &egui::Context::default());
            assert_eq!(app.ui.zarr.error, None, "{attrs} {typed}");
            assert_eq!(app.session.map().layers.len(), 1, "{attrs} {typed}");
            let notices = app.session.notices_since(mark);
            let said = notices.iter().filter(|n| n.text.contains("(conus) isn't applied")).count();
            let shifts = notices.iter().filter(|n| n.text.contains("isn't applied") || n.text.contains("has no conversion")).count();
            let own = notices.iter().filter(|n| n.text.contains("the definition's")).count();
            assert_eq!((said, shifts, own), (expected, expected, typed_said), "{attrs} {typed}: {notices:?}");
            // The typed system goes with the layer, for its series.
            let kept = app.session.map().layers.first().and_then(|l| l.raster.as_ref()).and_then(|r| r.crs.clone());
            assert_eq!(kept.as_deref(), Some(typed).filter(|t| !t.is_empty()), "{attrs} {typed}");
        }
    }
}
