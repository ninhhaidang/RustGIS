//! Find Place and Geocode Addresses: geocoding off the frame, one request at a time (as the
//! public servers ask), through the tile cache's downloads (a thread natively, fetch on the web).

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::{
    cache,
    geocode::{Geocoder, Kind, Place},
};
use serde_json::{Map, Value, json};
use web_time::Instant;

use crate::{RustGisApp, Slot, i18n};

type Answer = Slot<Result<Vec<u8>, String>>;

/// Searches and geocoding runs under way.
#[derive(Default)]
pub struct State {
    /// A Find Place search waiting for its answer.
    search: Option<(Geocoder, Answer)>,
    /// The places the last search found.
    pub places: Vec<Place>,
    /// The kind of geocoder that found them (its credit is shown under them).
    pub found_by: Option<Kind>,
    /// Whether a search is under way.
    pub searching: bool,
    job: Option<Job>,
    /// Batch pacing survives completion and cancellation, so a restart cannot reset it.
    sent: Option<Instant>,
    /// A cancelled run's request must finish before another batch can send anything.
    cancelled: Option<Answer>,
}

/// A table's addresses being geocoded.
struct Job {
    g: Geocoder,
    /// `layer.geocode`'s parameters, given the answers when the last lands.
    params: Value,
    queue: Vec<String>,
    next: usize,
    found: Map<String, Value>,
    waiting: Option<(String, Answer)>,
    /// Requests that failed (not "not found"), and how many in a row; the last error.
    failed: usize,
    in_a_row: usize,
    error: Option<String>,
    /// Places the answers gave without a usable position.
    skipped: usize,
    /// Additional candidates omitted when keeping one match per address.
    limited: usize,
}

fn fetch(url: &str) -> Answer {
    let slot: Answer = Slot::default();
    let land = slot.clone();
    cache::download(url, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
    slot
}

fn take(slot: &Answer) -> Option<Result<Vec<u8>, String>> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Lands searches and moves geocoding runs along.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let st = &mut app.ui.geocode;
    if let Some((g, slot)) = &st.search {
        match take(slot) {
            Some(r) => {
                let r = r.and_then(|b| g.read(&b));
                let kind = g.kind;
                st.search = None;
                st.searching = false;
                if let Ok(found) = &r
                    && found.skipped > 0
                {
                    app.session.say(rustgis_engine::data::loss::Note::loss(
                        rustgis_engine::data::loss::LossCode::DroppedFeature,
                        skipped(found.skipped),
                    ));
                }
                match r {
                    Ok(f) if f.places.is_empty() => app.dialogs.error = Some(tl!("No place found").to_string()),
                    Ok(mut f) => {
                        f.limit(8);
                        if f.limited > 0 {
                            app.session.notify(
                                i18n::fmt(
                                    "Search candidates not shown: {n} (limit {limit}).",
                                    &[("n", &f.limited.to_string()), ("limit", "8")],
                                ),
                                false,
                            );
                        }
                        app.dialogs.error = None;
                        st.places = f.places;
                        st.found_by = Some(kind);
                    }
                    Err(e) => app.dialogs.error = Some(ui_error(e)),
                }
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
        }
    }
    if let Some(slot) = &st.cancelled {
        if take(slot).is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        st.cancelled = None;
    }
    let Some(job) = &mut st.job else { return };
    if let Some((q, slot)) = &job.waiting {
        let Some(r) = take(slot) else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        match r.and_then(|b| job.g.read(&b)) {
            Ok(found) => {
                job.skipped += found.skipped;
                job.limited += found.limited + found.places.len().saturating_sub(1);
                let place = found.places.into_iter().next();
                job.found.insert(q.clone(), place.map(|p| json!({"lon": p.lon, "lat": p.lat, "name": p.name})).unwrap_or(Value::Null));
                job.in_a_row = 0;
            }
            // A failed request isn't "not found"; a server refusing several in a row (a rate
            // limit) stops the run.
            Err(e) => {
                job.failed += 1;
                job.in_a_row += 1;
                job.error = Some(ui_error(e));
                // The same rule as the command's: three refusals before any answer, or five in a row.
                if job.in_a_row >= 5 || (job.in_a_row >= 3 && job.found.is_empty()) {
                    job.failed += job.queue.len().saturating_sub(job.next);
                    job.next = job.queue.len();
                }
            }
        }
        job.waiting = None;
    }
    if let Some(q) = job.queue.get(job.next) {
        // An address longer than the server takes isn't sent cut short: it fails.
        if let Err(e) = job.g.check_query(q) {
            job.failed += 1;
            job.error = Some(e);
            job.next += 1;
            ctx.request_repaint();
            return;
        }
        let due = st.sent.is_none_or(|t| t.elapsed().as_millis() as u64 >= job.g.spacing_ms());
        if due {
            job.waiting = Some((q.clone(), fetch(&job.g.url(q, 1))));
            st.sent = Some(Instant::now());
            job.next += 1;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    // Every answer is in: the layer.
    let Some(job) = st.job.take() else { return };
    if job.skipped > 0 {
        app.session.say(rustgis_engine::data::loss::Note::loss(rustgis_engine::data::loss::LossCode::DroppedFeature, skipped(job.skipped)));
    }
    if job.limited > 0 {
        app.session.notify(
            i18n::fmt(
                "{n} additional geocoding candidates were not used; the first match was kept for each address.",
                &[("n", &job.limited.to_string())],
            ),
            false,
        );
    }
    if job.failed > 0 {
        let why = job.error.unwrap_or_default();
        app.session.notify(
            i18n::fmt(
                "{n} addresses couldn't be looked up ({why}); run again later for them",
                &[("n", &job.failed.to_string()), ("why", &why)],
            ),
            true,
        );
    }
    if job.found.is_empty() {
        return;
    }
    let mut p = job.params;
    p["places"] = Value::Object(job.found);
    if app.run("layer.geocode", p).is_ok() {
        app.map.invalidate();
    }
}

/// Translate service errors at the UI boundary; the CLI retains the provider's English keys.
fn ui_error(error: String) -> String {
    tl!(error.as_str()).to_string()
}

/// Progress stays visible after the dialog closes. Cancel discards this run, including an
/// answer already in flight: its download may finish, but cannot add a layer. A restarted batch
/// waits for that download and keeps the previous request's pacing timestamp.
pub fn status(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let Some(job) = &app.ui.geocode.job else { return };
    ui.separator();
    ui.label(tl!("Geocoding…"));
    let done = job.next.saturating_sub(usize::from(job.waiting.is_some()));
    let total = job.queue.len();
    ui.add(
        egui::ProgressBar::new(done as f32 / total.max(1) as f32)
            .text(format!("{done} / {total}"))
            .desired_width(100.0)
            .desired_height(12.0),
    );
    if ui.small_button(tl!("Cancel")).clicked() {
        if let Some(job) = app.ui.geocode.job.take()
            && let Some((_, slot)) = job.waiting
        {
            app.ui.geocode.cancelled = Some(slot);
        }
        app.session.notify(tl!("Geocoding cancelled; no layer was added.").to_string(), false);
    }
}

/// What a search or a run says of the places the geocoder gave without a usable position.
fn skipped(n: usize) -> String {
    i18n::fmt(
        "{n} of the geocoder's places had no usable position (no coordinate, or an unreadable one) and were left out",
        &[("n", &n.to_string())],
    )
}

/// Find Place: a search box, then the places found; one picked is zoomed to.
pub fn find_place(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let mut go = false;
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text(tl!("A place or an address")).desired_width(330.0));
        // Focus the box when the dialog opens, not every frame (the Server box is typed in too).
        if ui.memory(|m| m.focused().is_none()) {
            r.request_focus();
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.add_enabled(!app.ui.geocode.searching, egui::Button::new(tl!("Search"))).clicked() || enter {
            go = true;
        }
    });
    provider(app, ui);
    if go && !app.dialogs.a.trim().is_empty() {
        match Geocoder::new(Some(app.dialogs.mode), Some(app.dialogs.b.as_str())).and_then(|g| {
            g.check_fetch()?;
            g.check_query(&app.dialogs.a)?;
            Ok(g)
        }) {
            Ok(g) => {
                let slot = fetch(&g.url(&app.dialogs.a, 8));
                app.ui.geocode.search = Some((g, slot));
                app.ui.geocode.searching = true;
                app.ui.geocode.places.clear();
                app.dialogs.error = None;
            }
            Err(e) => app.dialogs.error = Some(ui_error(e)),
        }
    }
    if app.ui.geocode.searching {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Searching…"));
        });
    }
    let mut picked = None;
    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
        for (i, p) in app.ui.geocode.places.iter().enumerate() {
            if ui.add(egui::Button::new(&p.name).wrap().min_size(vec2(ui.available_width(), 0.0))).clicked() {
                picked = Some(i);
            }
        }
    });
    let attribution = egui::RichText::new(attribution(&app.ui.geocode, app.dialogs.mode)).small().weak();
    ui.label(attribution);
    if let Some(p) = picked.and_then(|i| app.ui.geocode.places.get(i)).cloned() {
        rustgis_engine::cmd::geocode::go_to_place(&mut app.session, &p);
        app.map.invalidate();
        return true;
    }
    false
}

/// The credit for the places shown: those of the geocoder that found them (another may have been
/// picked since), or of the one picked when none are shown.
fn attribution(st: &State, mode: &str) -> &'static str {
    let kind = st.found_by.filter(|_| !st.places.is_empty()).unwrap_or(match mode {
        "locatieserver" => Kind::Locatieserver,
        "census" => Kind::Census,
        "photon" => Kind::Photon,
        _ => Kind::Nominatim,
    });
    match kind {
        Kind::Locatieserver => tl!("Search results: PDOK Locatieserver (Kadaster)"),
        Kind::Census => tl!("Search results: U.S. Census Bureau (address ranges)"),
        _ => tl!("Search results © OpenStreetMap contributors"),
    }
}

/// The geocoder choice shared by both dialogs: OpenStreetMap's Nominatim, Photon, PDOK's
/// Locatieserver (the Netherlands), Census, or a server of one's own.
fn provider(app: &mut RustGisApp, ui: &mut egui::Ui) {
    if !matches!(app.dialogs.mode, "nominatim" | "photon" | "locatieserver" | "census") {
        app.dialogs.mode = "nominatim";
    }
    let census = tl!("U.S. Census (addresses)");
    let locatieserver = tl!("Locatieserver (the Netherlands)");
    egui::Grid::new("geocoder").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Geocoder"));
        egui::ComboBox::from_id_salt("geocoder_kind")
            .selected_text(match app.dialogs.mode {
                "photon" => "Photon",
                "locatieserver" => locatieserver,
                "census" => census,
                _ => "Nominatim",
            })
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.dialogs.mode, "nominatim", "Nominatim");
                ui.selectable_value(&mut app.dialogs.mode, "photon", "Photon");
                ui.selectable_value(&mut app.dialogs.mode, "locatieserver", locatieserver);
                ui.selectable_value(&mut app.dialogs.mode, "census", census);
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Server"));
        let hint = match app.dialogs.mode {
            "locatieserver" => tl!("PDOK's public server"),
            "census" => tl!("Census Bureau's public server"),
            _ => tl!("OpenStreetMap's public server"),
        };
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.b).hint_text(hint).desired_width(280.0)).labelled_by(input_label.id);
        ui.end_row();
    });
    if app.dialogs.mode == "census" {
        ui.label(tl!("Street addresses only; positions are calculated along address ranges."));
        #[cfg(target_arch = "wasm32")]
        ui.label(tl!("In the web app, give a CORS-enabled proxy server; direct Census access needs the desktop app or CLI."));
    }
}

/// Geocode Addresses: a table or layer, its address fields and the geocoder; the run goes on
/// after the dialog closes, a request at a time.
pub fn geocode(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(egui::RichText::new(tl!("Turns a table's addresses into points: each row's address fields are joined with commas (such as street, city, country) and looked up. OpenStreetMap's public Nominatim and Photon take up to 100 addresses a run, one a second; Census (U.S. street addresses) takes up to 10,000, one a second; PDOK and a server of your own take more.")).weak());
    // A table is picked by its id, since names can repeat: `dialogs.flag` says the choice is a
    // table, and `dialogs.layer` then holds its id.
    let tables: Vec<(u32, String)> = app.session.project.tables.iter().map(|t| (t.id, t.name.clone())).collect();
    let layers: Vec<String> = app.session.map().layers.iter().filter(|l| !l.is_raster()).map(|l| l.name.clone()).collect();
    if app.dialogs.layer.is_empty() {
        match tables.first() {
            Some((id, _)) => (app.dialogs.layer, app.dialogs.flag) = (id.to_string(), true),
            None => app.dialogs.layer = layers.first().cloned().unwrap_or_default(),
        }
    }
    let table = if app.dialogs.flag { app.dialogs.layer.parse::<u32>().ok() } else { None };
    let shown = match table {
        Some(id) => tables.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone()).unwrap_or_default(),
        None => app.dialogs.layer.clone(),
    };
    egui::Grid::new("geocode").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Table or layer"));
        egui::ComboBox::from_id_salt("geocode_source")
            .selected_text(shown)
            .width(280.0)
            .show_ui(ui, |ui| {
                for (id, n) in &tables {
                    if ui.selectable_label(table == Some(*id), n).clicked() {
                        (app.dialogs.layer, app.dialogs.flag) = (id.to_string(), true);
                    }
                }
                for n in &layers {
                    if ui.selectable_label(table.is_none() && app.dialogs.layer == *n, n).clicked() {
                        (app.dialogs.layer, app.dialogs.flag) = (n.clone(), false);
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Address fields"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text(tl!("street, city, country")).desired_width(280.0))
            .labelled_by(input_label.id);
        ui.end_row();
    });
    provider(app, ui);
    if app.ui.geocode.job.is_some() {
        ui.label(egui::RichText::new(tl!("Another run is under way.")).weak());
    }
    crate::dialogs::buttons(app, ui, "Geocode", |app| {
        if app.ui.geocode.job.is_some() {
            return Err(tl!("Another run is under way.").to_string());
        }
        let mut params = json!({"address": app.dialogs.a.trim(), "provider": app.dialogs.mode});
        match table {
            Some(id) => params["table"] = json!(id),
            None => params["layer"] = json!(app.dialogs.layer),
        }
        if !app.dialogs.b.trim().is_empty() {
            params["url"] = json!(app.dialogs.b.trim());
        }
        let g = Geocoder::new(Some(app.dialogs.mode), Some(app.dialogs.b.as_str())).map_err(ui_error)?;
        g.check_fetch().map_err(ui_error)?;
        let (_, queue) = rustgis_engine::cmd::geocode::addresses(&app.session, &params)?;
        if queue.is_empty() {
            return Err(tl!("The address fields are empty in every row.").to_string());
        }
        rustgis_engine::cmd::geocode::check_batch(&g, queue.len())?;
        app.session.notify(i18n::fmt("Geocoding {n} addresses…", &[("n", &queue.len().to_string())]), false);
        app.ui.geocode.job = Some(Job {
            g,
            params,
            queue,
            next: 0,
            found: Map::new(),
            waiting: None,
            failed: 0,
            in_a_row: 0,
            error: None,
            skipped: 0,
            limited: 0,
        });
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The actual status bar owns Cancel, even when the original dialog is closed. A late
    /// network answer cannot resume the cancelled job or install its partial output.
    #[test]
    fn cancel_geocoding_from_the_status_bar_ignores_a_late_answer() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let slot = Answer::default();
        app.ui.geocode.sent = Some(Instant::now() - std::time::Duration::from_secs(2));
        app.ui.geocode.job = Some(Job {
            g: Geocoder::new(Some("census"), None).unwrap(),
            params: json!({"provider":"census", "table":"addresses", "address":"address"}),
            queue: vec!["first".into(), "second".into()],
            next: 1,
            found: Map::new(),
            waiting: Some(("first".into(), slot.clone())),
            failed: 0,
            in_a_row: 0,
            error: None,
            skipped: 0,
            limited: 0,
        });
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1480.0, 900.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.step();
        assert!(h.query_by_label("Geocoding…").is_some());
        assert!(h.query_by_label("0 / 2").is_some());
        h.get_by_label("Cancel").click();
        h.step();
        assert!(h.state().ui.geocode.job.is_none());
        assert!(h.state().ui.geocode.cancelled.is_some());
        let sent = h.state().ui.geocode.sent;
        // Restart before the cancelled download answers: even after its pacing interval,
        // a new batch must not put a second request on the wire.
        h.state_mut().ui.geocode.job = Some(Job {
            g: Geocoder::new(Some("census"), Some("http://127.0.0.1:1")).unwrap(),
            params: json!({"provider":"census", "table":"addresses", "address":"address"}),
            queue: vec!["third".into()],
            next: 0,
            found: Map::new(),
            waiting: None,
            failed: 0,
            in_a_row: 0,
            error: None,
            skipped: 0,
            limited: 0,
        });
        h.step();
        let restart = h.state().ui.geocode.job.as_ref().unwrap();
        assert_eq!(restart.next, 0);
        assert!(restart.waiting.is_none());
        assert_eq!(h.state().ui.geocode.sent, sent, "restarting preserves the last send time");
        // Cancelling a run which has not sent anything must retain the older download.
        h.get_by_label("Cancel").click();
        h.step();
        assert!(h.state().ui.geocode.cancelled.is_some());
        *slot.lock().unwrap() = Some(Ok(br#"{"result":{"addressMatches":[{"coordinates":{"x":-77,"y":38}}]}}"#.to_vec()));
        h.step();
        assert!(h.state().ui.geocode.job.is_none());
        assert!(h.state().ui.geocode.cancelled.is_none());
        assert_eq!(h.state().ui.geocode.sent, sent);
        assert!(h.state().session.map().layers.is_empty());
        assert!(h.state().session.notices.iter().any(|n| n.text.contains("Geocoding cancelled")));
    }

    /// A Census search can return more candidates than requested. Report both the UI limit
    /// and the parser's safety cap; a clean one-result answer has no such notice.
    #[test]
    fn census_search_reports_candidates_withheld_by_its_limit() {
        for count in [1, 25] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let p = json!({"matchedAddress":"address", "coordinates":{"x":-77.0,"y":38.0}});
            let response = json!({"result":{"addressMatches":vec![p;count]}});
            let slot = Answer::default();
            *slot.lock().unwrap() = Some(Ok(serde_json::to_vec(&response).unwrap()));
            app.ui.geocode.search = Some((Geocoder::new(Some("census"), None).unwrap(), slot));
            poll(&mut app, &egui::Context::default());
            assert_eq!(app.ui.geocode.places.len(), count.min(8));
            if count == 1 {
                assert!(app.session.notices.is_empty());
            } else {
                assert_eq!(app.session.notices.len(), 1);
                assert_eq!(app.session.notices[0].text, "Search candidates not shown: 17 (limit 8).");
            }
        }
    }

    /// Batch runs report all alternatives omitted, including the parser cap, and keep the
    /// first candidate. A single match produces no omission notice.
    #[test]
    fn census_batch_geocoding_reports_unused_candidates() {
        for count in [1, 3, 25] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("layer.addDataText", json!({"fileName":"addresses.csv", "text":"address\nA\n"})).unwrap();
            let p = json!({"matchedAddress":"A", "coordinates":{"x":-77.0,"y":38.0}});
            let slot = Answer::default();
            *slot.lock().unwrap() = Some(Ok(serde_json::to_vec(&json!({"result":{"addressMatches":vec![p;count]}})).unwrap()));
            app.ui.geocode.job = Some(Job {
                g: Geocoder::new(Some("census"), None).unwrap(),
                params: json!({"provider":"census", "table":"addresses", "address":"address"}),
                queue: vec!["A".into()],
                next: 1,
                found: Map::new(),
                waiting: Some(("A".into(), slot)),
                failed: 0,
                in_a_row: 0,
                error: None,
                skipped: 0,
                limited: 0,
            });
            let mark = app.session.notice_mark();
            poll(&mut app, &egui::Context::default());
            assert!(app.ui.geocode.job.is_none());
            assert_eq!(app.session.map().layers.len(), 1);
            let notices = app.session.notices_since(mark);
            if count == 1 {
                assert!(notices.is_empty(), "{notices:?}");
            } else {
                assert_eq!(notices.len(), 1);
                assert_eq!(
                    notices[0].text,
                    format!("{} additional geocoding candidates were not used; the first match was kept for each address.", count - 1)
                );
            }
        }
    }

    /// The credit under the places is the geocoder's that found them, not the one picked since.
    #[test]
    fn the_credit_follows_the_places_not_the_picked_geocoder() {
        let place = Place { name: "Damrak 1".into(), lon: 4.9, lat: 52.4, bbox: None };
        let osm = tl!("Search results © OpenStreetMap contributors");
        let pdok = tl!("Search results: PDOK Locatieserver (Kadaster)");
        let mut st = State { places: vec![place], found_by: Some(Kind::Nominatim), ..State::default() };
        assert_eq!(attribution(&st, "locatieserver"), osm);
        st.found_by = Some(Kind::Locatieserver);
        assert_eq!(attribution(&st, "nominatim"), pdok);
        assert_eq!(attribution(&st, "locatieserver"), pdok);
        st.found_by = Some(Kind::Census);
        let census = tl!("Search results: U.S. Census Bureau (address ranges)");
        assert_eq!(attribution(&st, "nominatim"), census);
        // No places shown: the picked geocoder's.
        st.places.clear();
        assert_eq!(attribution(&st, "nominatim"), osm);
        assert_eq!(attribution(&st, "locatieserver"), pdok);
        assert_eq!(attribution(&st, "census"), census);
    }

    /// A search's places land with the kind of geocoder that found them.
    #[test]
    fn a_search_records_the_geocoder_that_found_its_places() {
        let mut app = RustGisApp::with_sample();
        let slot: Answer = Slot::default();
        *slot.lock().unwrap() = Some(Ok(br#"[{"display_name":"Damrak, Amsterdam","lat":"52.37","lon":"4.89"}]"#.to_vec()));
        app.ui.geocode.search = Some((Geocoder::new(Some("nominatim"), None).unwrap(), slot));
        app.dialogs.mode = "locatieserver";
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.ui.geocode.places.len(), 1);
        assert_eq!(app.ui.geocode.found_by, Some(Kind::Nominatim));
        assert_eq!(attribution(&app.ui.geocode, app.dialogs.mode), tl!("Search results © OpenStreetMap contributors"));
    }
}
