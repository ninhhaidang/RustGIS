//! Home › Add › Web Data › Satellite Embeddings: a dataset picked from the catalog (its provider, model,
//! resolution, years and links), its tiles found where the map is (footprints drawn), and one
//! shown on the map: AlphaEarth's three chosen bands as red, green and blue with a stretch,
//! Earth Index's points coloured by their vectors' principal components, Tessera's three bands of
//! the view as a raster. AlphaEarth's tiles can also be found over all its years and shown on
//! the time slider, a year a step (`embeddings.addCube`). Searches and reads go off the frame,
//! with Cancel; what they read is handed to the `embeddings.*` commands, so the change is
//! undoable like any other.

use std::collections::BTreeMap;

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::embeddings::{self as emb, EmbeddingDataset, Found, SeriesFound, Tile, tessera};
use rustgis_engine::render::rustgis_tiles::{cache, cog};
use serde_json::json;

use crate::{RustGisApp, Slot};

/// A read under way: a search, or a tile's data for the map.
enum Read {
    Search(Slot<Result<Found, String>>),
    /// AlphaEarth's tiles over its years.
    Series(Slot<Result<SeriesFound, String>>),
    /// Bytes to hand to `embeddings.add` (Earth Index's points as GeoJSON, Tessera's GeoTIFF),
    /// and what they were read for, as the read began.
    Data(Slot<Result<Vec<u8>, String>>, Request),
}

/// What a read of a tile's data was started for: the choices may change while it runs.
struct Request {
    name: String,
    dataset: &'static str,
    year: i32,
    bands: [usize; 3],
    /// The session's download it is handed over as (dropped when another project opens).
    id: u64,
    /// Set when the read took only part of the view (Tessera's window around its centre).
    cut: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The pane's choices, the read under way and what it found.
pub struct State {
    pub dataset: &'static str,
    pub year: i32,
    /// AlphaEarth's bands shown (0-based) and stretch.
    pub aef_bands: [usize; 3],
    pub stretch: f32,
    /// Tessera's and OlmoEarth's bands (0-based).
    pub zone_bands: [usize; 3],
    /// Earth Index: a vector value kept as each point's `value` (0-based), if any.
    pub dimension: Option<usize>,
    /// AlphaEarth: search its tiles over all its years, each added as a cube on the time slider.
    pub all_years: bool,
    reading: Option<Read>,
    /// The search's box and dataset.
    /// The search's box and dataset, and the project it was for (`Session::opened`).
    searched: Option<([f64; 4], &'static str, u64)>,
    pub found: Vec<Tile>,
    /// Each tile found's file a year, by `found`'s order, when the search was over the years.
    years: Vec<BTreeMap<i32, String>>,
    /// The rows that search left out, as they can't be read (handed over as `unusable`).
    years_unusable: usize,
    /// Whether the search over the years read the index whole (handed over as `complete`).
    years_whole: bool,
    /// The footprints layer of the last search, and its project (`Session::opened`): layer ids
    /// are a map's own, so another project's may be the same.
    footprints: Option<(u32, u64)>,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            dataset: "alphaearth",
            year: 2024,
            aef_bands: emb::AEF_DEFAULT_BANDS,
            stretch: emb::AEF_DEFAULT_STRETCH,
            zone_bands: tessera::DEFAULT_BANDS,
            dimension: None,
            all_years: false,
            reading: None,
            searched: None,
            found: Vec::new(),
            years: Vec::new(),
            years_unusable: 0,
            years_whole: true,
            footprints: None,
            error: None,
            info: None,
        }
    }
}

fn picked(st: &State) -> &'static EmbeddingDataset {
    emb::dataset(st.dataset).unwrap_or(&emb::DATASETS[0])
}

/// Picks a dataset (`ui.dialog {"name": "embeddings", "dataset": …}`).
pub fn pick(app: &mut RustGisApp, id: &str) -> Result<(), String> {
    let d = emb::dataset(id).ok_or_else(|| format!("unknown dataset '{id}' (embeddings.datasets lists them)"))?;
    app.ui.embeddings.select(d);
    Ok(())
}

impl State {
    /// Picks a dataset: its results and messages go, and the year keeps to its years.
    fn select(&mut self, d: &'static EmbeddingDataset) {
        if self.dataset == d.id {
            return;
        }
        self.dataset = d.id;
        self.found.clear();
        self.years.clear();
        self.info = None;
        self.error = None;
        if let Some((a, b)) = d.years {
            self.year = self.year.clamp(a, b);
        }
    }
}

/// The map's extent, in longitude/latitude.
fn view_bbox(app: &RustGisApp) -> Result<[f64; 4], String> {
    let e = rustgis_engine::cmd::workspace::view_extent(&app.session);
    if e.is_empty() {
        return Err(tl!("The map's extent isn't known yet").to_string());
    }
    emb::check_bbox([e.min_x, e.min_y, e.max_x, e.max_y])
}

/// What a search listed: its tiles, and for a search over the years each one's files and what
/// the search left out.
#[derive(Default)]
struct Listed {
    tiles: Vec<Tile>,
    complete: bool,
    years: Vec<BTreeMap<i32, String>>,
    dropped: usize,
    duplicates: usize,
    unusable: usize,
}

impl Listed {
    fn of_series(f: SeriesFound) -> Listed {
        let (tiles, years) = f.series.into_iter().map(|s| (s.tile, s.years)).unzip();
        Listed { tiles, complete: f.complete, years, dropped: f.dropped, duplicates: f.duplicates, unusable: f.unusable }
    }
}

/// Lands a search or a read.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let landed = match &app.ui.embeddings.reading {
        None => return,
        Some(Read::Search(slot)) => slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .map(|r| Ok(r.map(|f| Listed { tiles: f.tiles, complete: f.complete, unusable: f.unusable, ..Default::default() }))),
        Some(Read::Series(slot)) => slot.lock().unwrap_or_else(|e| e.into_inner()).take().map(|r| Ok(r.map(Listed::of_series))),
        Some(Read::Data(slot, _)) => slot.lock().unwrap_or_else(|e| e.into_inner()).take().map(Err),
    };
    let Some(landed) = landed else {
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
        return;
    };
    let read = app.ui.embeddings.reading.take();
    match landed {
        Ok(found) => match found {
            Ok(Listed { tiles, complete, years, dropped, duplicates, unusable }) => {
                let st = &mut app.ui.embeddings;
                // A search for a dataset no longer picked: its tiles aren't this one's.
                // Or for a project since closed.
                let opened = app.session.opened;
                if st.searched.is_some_and(|(_, d, o)| d != st.dataset || o != opened) {
                    return;
                }
                // Nothing found in a part of the index read is said as one.
                let mut info = if tiles.is_empty() && complete {
                    tl!("Nothing found where the map is").to_string()
                } else if complete {
                    crate::i18n::fmt("{n} found where the map is", &[("n", &tiles.len().to_string())])
                } else {
                    crate::i18n::fmt(
                        "{n} found; the map holds more than one search reads: zoom in for the rest",
                        &[("n", &tiles.len().to_string())],
                    )
                };
                if dropped > 0 {
                    info.push_str(&crate::i18n::fmt(" · {n} more left out: zoom in for them", &[("n", &dropped.to_string())]));
                }
                if duplicates > 0 {
                    info.push_str(&crate::i18n::fmt(
                        " · {n} tile-years had a second file; the first was taken",
                        &[("n", &duplicates.to_string())],
                    ));
                }
                if unusable > 0 {
                    let n = unusable.to_string();
                    info.push_str(&if complete {
                        crate::i18n::fmt(
                            " · rows of the index left out, as their date, address or footprint can't be read: {n}",
                            &[("n", &n)],
                        )
                    } else {
                        crate::i18n::fmt(
                            " · rows of the index left out, as their date, address or footprint can't be read: {n} of those read",
                            &[("n", &n)],
                        )
                    });
                }
                st.info = Some(info);
                st.found = tiles;
                st.years = years;
                st.years_whole = complete;
                st.years_unusable = unusable;
                show_footprints(app);
            }
            Err(e) => app.ui.embeddings.error = Some(e),
        },
        Err(data) => {
            let Some(Read::Data(_, req)) = read else { return };
            match data {
                Ok(bytes) => {
                    // Read for a project since closed: dropped.
                    if !app.session.download_landed(req.id, bytes) {
                        return;
                    }
                    let mut p = json!({"dataset": req.dataset, "ran": req.id, "name": req.name});
                    if tessera::store(req.dataset).is_some() {
                        p["year"] = json!(req.year);
                        p["bands"] = json!(req.bands.map(|b| b + 1));
                    }
                    match app.run("embeddings.add", p) {
                        Ok(_) => {
                            app.map.invalidate();
                            if req.cut.load(std::sync::atomic::Ordering::Relaxed) {
                                app.ui.embeddings.info =
                                    Some(tl!("The view is larger than one read: its centre was read (zoom in for the rest)").to_string());
                            }
                        }
                        Err(e) => app.ui.embeddings.error = Some(e),
                    }
                }
                Err(e) => {
                    app.session.download_failed(req.id);
                    app.ui.embeddings.error = Some(e);
                }
            }
        }
    }
}

/// Replaces the last search's footprints with this one's.
fn show_footprints(app: &mut RustGisApp) {
    if let Some((old, opened)) = app.ui.embeddings.footprints.take()
        && opened == app.session.opened
        && app.session.map().layer(old).is_some()
    {
        let _ = app.run("layer.remove", json!({"layer": old}));
    }
    let st = &app.ui.embeddings;
    // Tessera's and OlmoEarth's results are their zones: no outlines worth drawing.
    if st.found.is_empty() || tessera::store(st.dataset).is_some() {
        return;
    }
    let fc = emb::footprints_geojson(&st.found);
    // A layer's name, like other data: not translated.
    let name = format!("{} tiles", picked(st).name.split(" (").next().unwrap_or("Embeddings"));
    match app.run("stac.addFootprints", json!({"footprints": fc, "name": name})) {
        Ok(r) => {
            let opened = app.session.opened;
            app.ui.embeddings.footprints = r["layer"].as_u64().and_then(|i| u32::try_from(i).ok()).map(|id| (id, opened));
        }
        Err(e) => app.ui.embeddings.error = Some(e),
    }
    app.map.invalidate();
}

/// Starts a search where the map is.
fn search(app: &mut RustGisApp) {
    let bbox = match view_bbox(app) {
        Ok(b) => b,
        Err(e) => {
            app.ui.embeddings.error = Some(e);
            return;
        }
    };
    let st = &mut app.ui.embeddings;
    st.error = None;
    st.info = None;
    st.found.clear();
    st.years.clear();
    st.searched = Some((bbox, st.dataset, app.session.opened));
    if st.dataset == "alphaearth" && st.all_years {
        let years = picked(st).years.unwrap_or((st.year, st.year));
        let slot: Slot<Result<SeriesFound, String>> = Slot::default();
        let land = slot.clone();
        emb::search_aef_series(
            bbox,
            years,
            Box::new(move |r| {
                *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            }),
        );
        st.reading = Some(Read::Series(slot));
        return;
    }
    let slot: Slot<Result<Found, String>> = Slot::default();
    let land = slot.clone();
    let done: Box<dyn FnOnce(Result<Found, String>) + Send> = Box::new(move |r| {
        *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    });
    match st.dataset {
        "alphaearth" => emb::search_aef(bbox, st.year, done),
        "earth-index" => emb::search_earth_index(bbox, done),
        id => done(Ok(tessera::tiles(tessera::store(id).unwrap_or(&tessera::TESSERA), bbox, st.year).into())),
    }
    st.reading = Some(Read::Search(slot));
}

/// Shows a tile on the map: AlphaEarth's at once (read in place as the map needs it), the others
/// read for the view first.
fn add(app: &mut RustGisApp, i: usize) {
    let Some(tile) = app.ui.embeddings.found.get(i).cloned() else { return };
    app.ui.embeddings.error = None;
    match tile.dataset {
        "alphaearth" => {
            let st = &app.ui.embeddings;
            let (bands, stretch) = (st.aef_bands.map(|b| b + 1), st.stretch);
            // A tile found over the years: its files, handed over, on the time slider.
            let (id, p) = match st.years.get(i).filter(|y| !y.is_empty()) {
                Some(years) => {
                    let years: serde_json::Map<String, serde_json::Value> = years.iter().map(|(y, u)| (y.to_string(), json!(u))).collect();
                    let (first, last) = picked(st).years.unwrap_or((st.year, st.year));
                    let found = json!([{"years": years}]);
                    (
                        "embeddings.addCube",
                        json!({
                            "dataset": "alphaearth", "found": found, "complete": st.years_whole, "unusable": st.years_unusable,
                            "start": first, "end": last, "bands": bands, "stretch": stretch,
                        }),
                    )
                }
                None => ("embeddings.add", json!({"dataset": "alphaearth", "url": tile.url, "bands": bands, "stretch": stretch})),
            };
            match app.run(id, p) {
                Ok(_) => app.map.invalidate(),
                Err(e) => app.ui.embeddings.error = Some(e),
            }
        }
        "earth-index" => {
            let bbox = match view_bbox(app) {
                Ok(b) => b,
                Err(e) => return app.ui.embeddings.error = Some(e),
            };
            let name = format!("Earth Index {}", tile.id);
            let (slot, land) = data_slot();
            let (n, dim) = (name.clone(), app.ui.embeddings.dimension);
            emb::load_points(
                &tile.url,
                bbox,
                Box::new(move |r| {
                    let r = r.and_then(|v| {
                        if v.points.is_empty() {
                            return Err(tl!("Earth Index has no points in the view").to_string());
                        }
                        Ok(rustgis_engine::data::io::to_geojson(&emb::points_dataset(&n, &v, dim)).to_string().into_bytes())
                    });
                    land(r);
                }),
            );
            let id = app.session.download_started();
            let req = Request { name, dataset: "earth-index", year: emb::EARTH_INDEX_YEAR, bands: [0; 3], id, cut: Default::default() };
            app.ui.embeddings.reading = Some(Read::Data(slot, req));
        }
        id => {
            let bbox = match view_bbox(app) {
                Ok(b) => b,
                Err(e) => return app.ui.embeddings.error = Some(e),
            };
            let z = tessera::store(id).unwrap_or(&tessera::TESSERA);
            let st = &app.ui.embeddings;
            let (year, bands) = (st.year, st.zone_bands);
            let zone = tessera::zone_of((bbox[0] + bbox[2]) / 2.0);
            let name = format!("{} {year} ({})", z.name, bands.map(|b| (b + 1).to_string()).join(", "));
            let (slot, land) = data_slot();
            let cut: std::sync::Arc<std::sync::atomic::AtomicBool> = Default::default();
            let cut_set = cut.clone();
            cache::download(
                &tessera::group_url(z, zone),
                Box::new(move |r| {
                    let plan = r.and_then(|b| tessera::parse_group(z, zone, &b)).and_then(|g| tessera::plan(z, g, bbox, year));
                    match plan {
                        Ok(plan) => {
                            cut_set.store(plan.cut, std::sync::atomic::Ordering::Relaxed);
                            tessera::read(
                                plan,
                                bands,
                                Box::new(move |r| {
                                    land(r.and_then(|raster| {
                                        rustgis_engine::data::raster::geotiff::write(&raster, 0).map_err(|e| e.to_string())
                                    }))
                                }),
                            )
                        }
                        Err(e) => land(Err(e)),
                    }
                }),
            );
            let id = app.session.download_started();
            app.ui.embeddings.reading = Some(Read::Data(slot, Request { name, dataset: z.id, year, bands, id, cut }));
        }
    }
}

type Land = Box<dyn FnOnce(Result<Vec<u8>, String>) + Send>;

/// A slot for bytes read off the frame, and what fills it.
fn data_slot() -> (Slot<Result<Vec<u8>, String>>, Land) {
    let slot: Slot<Result<Vec<u8>, String>> = Slot::default();
    let land = slot.clone();
    (
        slot,
        Box::new(move |r| {
            *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        }),
    )
}

/// Shows the bands and stretch picked on the AlphaEarth layers on the map.
fn restyle(app: &mut RustGisApp) {
    let st = &app.ui.embeddings;
    let (bands, s) = (st.aef_bands.map(|b| b + 1), st.stretch);
    // AlphaEarth's layers: COGs shown dequantised as AlphaEarth's, by their place in the stack
    // (names may repeat).
    let shown: Vec<usize> = app
        .session
        .map()
        .tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == rustgis_engine::map::TileKind::Cog && cog::split(&t.url).1.aef)
        .map(|(i, _)| i)
        .collect();
    if shown.is_empty() {
        app.ui.embeddings.info = Some(tl!("No AlphaEarth layer on the map").to_string());
        return;
    }
    for index in shown {
        let p = json!({"index": index, "bands": bands, "range": [-s, s], "dequantize": "aef"});
        if let Err(e) = app.run("map.setTiles", p) {
            app.ui.embeddings.error = Some(e);
        }
    }
    app.map.invalidate();
}

/// Three band pickers (from 1), in a row.
fn band_row(ui: &mut egui::Ui, bands: &mut [usize; 3], max: usize) {
    ui.horizontal(|ui| {
        for (b, label) in bands.iter_mut().zip(["R", "G", "B"]) {
            let input_label = ui.label(label);
            let mut v = *b + 1;
            if ui.add(egui::DragValue::new(&mut v).range(1..=max).speed(0.2)).labelled_by(input_label.id).changed() {
                *b = v.clamp(1, max) - 1;
            }
        }
    });
}

/// The pane.
pub fn pane(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(
        egui::RichText::new(tl!(
            "Vectors a geospatial foundation model made for every pixel or image patch, found where the map is and shown as colours."
        ))
        .color(t.text_dim),
    );
    let busy = app.ui.embeddings.reading.is_some();
    let w = (ui.available_width() - 110.0).clamp(160.0, 320.0);
    let mut go = false;
    egui::Grid::new("embeddings").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let st = &mut app.ui.embeddings;
        let input_label = ui.label(tl!("Dataset"));
        let shown = picked(st).name.split(" (").next().unwrap_or("").to_string();
        egui::ComboBox::from_id_salt("emb_dataset")
            .selected_text(shown)
            .width(w)
            .show_ui(ui, |ui| {
                for d in emb::DATASETS.iter() {
                    let label = d.name.split(" (").next().unwrap_or(d.name);
                    if ui.selectable_label(st.dataset == d.id, label).clicked() {
                        st.select(d);
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let d = picked(st);
        if d.search
            && let Some((first, last)) = d.years
            && last > first
        {
            let input_label = ui.label(tl!("Year"));
            let all = d.id == "alphaearth" && st.all_years;
            ui.add_enabled_ui(!all, |ui| {
                egui::ComboBox::from_id_salt("emb_year")
                    .selected_text(if all { format!("{first}–{last}") } else { st.year.to_string() })
                    .width(w)
                    .show_ui(ui, |ui| {
                        for y in (first..=last).rev() {
                            ui.selectable_value(&mut st.year, y, y.to_string());
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
            });
            ui.end_row();
            if d.id == "alphaearth" {
                ui.label("");
                ui.checkbox(&mut st.all_years, tl!("All years, on the time slider")).on_hover_text(tl!(
                    "Find each tile's file of every year; Add years shows them as one layer that follows the time slider"
                ));
                ui.end_row();
            }
        }
        match d.id {
            "alphaearth" => {
                ui.label(tl!("Bands"));
                band_row(ui, &mut st.aef_bands, 64);
                ui.end_row();
                let input_label = ui.label(tl!("Stretch"));
                ui.add(egui::Slider::new(&mut st.stretch, 0.05..=1.0).step_by(0.05).text(tl!("±, dequantised")))
                    .labelled_by(input_label.id)
                    .on_hover_text(tl!("Values from −stretch to +stretch are shown from black to full colour"));
                ui.end_row();
            }
            "tessera" | "olmoearth" => {
                ui.label(tl!("Bands"));
                band_row(ui, &mut st.zone_bands, tessera::BANDS);
                ui.end_row();
            }
            "earth-index" => {
                ui.label(tl!("Keep value"));
                ui.horizontal(|ui| {
                    let mut on = st.dimension.is_some();
                    if ui.checkbox(&mut on, "").on_hover_text(tl!("Keep one value of each point's vector as its value field")).changed() {
                        st.dimension = on.then_some(st.dimension.unwrap_or(0));
                    }
                    if let Some(dim) = &mut st.dimension {
                        let mut v = *dim + 1;
                        if ui.add(egui::DragValue::new(&mut v).range(1..=384).speed(0.3)).changed() {
                            *dim = v.clamp(1, 384) - 1;
                        }
                    }
                });
                ui.end_row();
            }
            _ => {}
        }
    });
    about(ui, &t, picked(&app.ui.embeddings));
    let d = picked(&app.ui.embeddings);
    if d.search {
        ui.horizontal(|ui| {
            let search = egui::Button::new(egui::RichText::new(tl!("Search where the map is")).color(t.on_accent)).fill(t.accent);
            go = ui.add_enabled(!busy, search).clicked();
            if d.id == "alphaearth"
                && ui
                    .add_enabled(!busy, egui::Button::new(tl!("Apply to layers")))
                    .on_hover_text(tl!("Show these bands and stretch on the AlphaEarth layers on the map"))
                    .clicked()
            {
                restyle(app);
            }
        });
    } else {
        ui.label(
            egui::RichText::new(tl!("Listed for discovery: RustGIS doesn't read this dataset yet. Its data and paper are linked above."))
                .color(t.text_dim),
        );
    }
    if go {
        search(app);
    }
    let st = &app.ui.embeddings;
    let mut cancel = false;
    if busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(match st.reading {
                Some(Read::Search(_) | Read::Series(_)) => tl!("Searching…"),
                _ => tl!("Reading the embeddings…"),
            });
            cancel = ui.button(tl!("Cancel")).clicked();
        });
    }
    if cancel {
        // What the read brings is dropped with its slot, and a data read's download released.
        // A search's read of the index runs on to its end: it is bounded (one read, at most
        // MAX_INDEX_ROWS rows) and the index query has no way to be told to stop yet, as for
        // every other search here.
        if let Some(Read::Data(_, req)) = app.ui.embeddings.reading.take() {
            app.session.download_failed(req.id);
        }
        app.ui.embeddings.reading = None;
        app.ui.embeddings.info = Some(tl!("Cancelled").to_string());
    }
    let st = &app.ui.embeddings;
    if let Some(e) = &st.error {
        ui.label(egui::RichText::new(e).color(t.red));
    }
    if let Some(i) = &st.info {
        ui.label(egui::RichText::new(i).color(t.text_dim));
    }
    let mut act = None;
    for (i, tile) in st.found.iter().enumerate() {
        let years = st.years.get(i).is_some_and(|y| !y.is_empty());
        ui.push_id(i, |ui| {
            if let Some(a) = card(ui, &t, tile, i, busy, years) {
                act = Some(a);
            }
        });
    }
    match act {
        Some(Act::Zoom(b)) => {
            if let Err(e) = app.run("map.zoomToExtent", json!({"extent": b})) {
                app.ui.embeddings.error = Some(e);
            }
        }
        Some(Act::Add(i)) => add(app, i),
        None => {}
    }
}

/// The dataset's facts and links.
fn about(ui: &mut egui::Ui, t: &crate::theme::Tokens, d: &EmbeddingDataset) {
    egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6).inner_margin(egui::Margin::same(8)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(d.name).strong()).wrap());
            let dim = |s: String| egui::RichText::new(s).small().color(t.text_dim);
            ui.add(egui::Label::new(dim(format!("{} · {}", d.provider, d.model))).wrap());
            let kind = if d.kind == "pixel" { tl!("a vector a pixel") } else { tl!("a vector an image patch") };
            let years = match d.years {
                Some((a, b)) if a == b => a.to_string(),
                Some((a, b)) => format!("{a}–{b}"),
                None => tl!("no yearly series").to_string(),
            };
            ui.add(
                egui::Label::new(dim(crate::i18n::fmt(
                    "{kind} · {res} · {dims} values · {years}",
                    &[("kind", kind), ("res", d.resolution), ("dims", &d.dimensions.to_string()), ("years", &years)],
                )))
                .wrap(),
            );
            // The catalog's facts are data, kept as the dataset states them (not translated).
            let cover = if d.license.is_empty() { d.coverage.to_string() } else { format!("{} · {}", d.coverage, d.license) };
            ui.add(egui::Label::new(dim(cover)).wrap());
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to(tl!("Data"), d.data_url);
                ui.hyperlink_to(tl!("Paper"), d.paper_url);
            });
        },
    );
}

/// What a click on a result asks for.
enum Act {
    Zoom([f64; 4]),
    Add(usize),
}

/// A result's card: its title, what it is, and its actions (Zoom, Add, the file's address).
fn card(ui: &mut egui::Ui, t: &crate::theme::Tokens, tile: &Tile, i: usize, busy: bool, years: bool) -> Option<Act> {
    let mut act = None;
    egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6).inner_margin(egui::Margin::same(8)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(&tile.title).strong()).wrap());
            ui.add(egui::Label::new(egui::RichText::new(&tile.detail).small().color(t.text_dim)).wrap());
            if !tile.id.is_empty() && tessera::store(tile.dataset).is_none() {
                ui.add(egui::Label::new(egui::RichText::new(&tile.id).small().color(t.text_dim)).truncate());
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button(tl!("Zoom")).clicked() {
                    act = Some(Act::Zoom(tile.bbox));
                }
                let (label, hint) = match tile.dataset {
                    "alphaearth" if years => (
                        tl!("Add years"),
                        tl!("Show the bands picked of every year on the time slider, read in place as the map needs them"),
                    ),
                    "alphaearth" => (tl!("Add"), tl!("Show the bands picked, read in place as the map needs them")),
                    "earth-index" => (tl!("Add points"), tl!("Read the points in the view, coloured by their principal components")),
                    _ => (tl!("Add view"), tl!("Read the bands picked for the view (at most about 6 km a side)")),
                };
                if ui.add_enabled(!busy, egui::Button::new(label)).on_hover_text(hint).clicked() {
                    act = Some(Act::Add(i));
                }
                if tile.url.starts_with("https://") && ui.button(tl!("Copy address")).on_hover_text(&tile.url).clicked() {
                    ui.ctx().copy_text(tile.url.clone());
                }
            });
        },
    );
    ui.add_space(2.0);
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search says the index's rows it left out, of one year and over the years, and nothing
    /// when there were none.
    #[test]
    fn a_search_says_the_rows_it_left_out() {
        let mut app = RustGisApp::with_sample();
        let ctx = egui::Context::default();
        let mut said = |read: Read| {
            app.ui.embeddings.reading = Some(read);
            poll(&mut app, &ctx);
            let st = &mut app.ui.embeddings;
            (st.info.take().unwrap(), st.years_unusable)
        };
        for (unusable, left) in [(0, false), (4, true)] {
            let one = Found { complete: true, unusable, ..Default::default() };
            let years = SeriesFound { complete: true, unusable, ..Default::default() };
            for (info, handed) in [
                said(Read::Search(std::sync::Arc::new(std::sync::Mutex::new(Some(Ok(one)))))),
                said(Read::Series(std::sync::Arc::new(std::sync::Mutex::new(Some(Ok(years)))))),
            ] {
                assert_eq!(info.ends_with("left out, as their date, address or footprint can't be read: 4"), left, "{info}");
                // What embeddings.addCube is handed with a tile found.
                assert_eq!(handed, unusable);
            }
        }
        // Of a part of the index read: said as one, though nothing is found in it.
        let part = Found { unusable: 4, ..Default::default() };
        let (info, _) = said(Read::Search(std::sync::Arc::new(std::sync::Mutex::new(Some(Ok(part))))));
        assert!(info.contains("more than one search reads") && info.ends_with("can't be read: 4 of those read"), "{info}");
    }
}
