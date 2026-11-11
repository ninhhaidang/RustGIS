//! Home › Web Data › Data Cubes (opengeos/RustGIS#683): the catalog of data cubes, filtered by
//! theme, by where the map is and by words; each cube's provider, period, step, resolution and
//! licence; and the cube added to the time slider (`cube.open`, so it is undoable like any other
//! command) or taken to Trend Analysis. The list is the hosted one once read, else the built-in
//! one, and JAXA Earth's datasets once its catalog is read (opengeos/RustGIS#1532): the pane says
//! how many, which of its datasets can't be cubes and why, and each one's credit and licence.

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::cubes::{self, Entry, Source};
use rustgis_engine::render::rustgis_tiles::jaxa;
use serde_json::json;

use crate::RustGisApp;

/// The pane's filters and period, and what the last action said.
pub struct State {
    /// The theme shown (empty: all).
    pub theme: String,
    /// Only the cubes with data where the map is.
    pub here: bool,
    pub query: String,
    /// The period a cube is added over (both empty: its last full year).
    pub start: String,
    pub end: String,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State { theme: String::new(), here: true, query: String::new(), start: String::new(), end: String::new(), error: None, info: None }
    }
}

/// The map's extent, in longitude/latitude within the globe.
fn view_bbox(app: &RustGisApp) -> Option<[f64; 4]> {
    let e = rustgis_engine::cmd::workspace::view_extent(&app.session);
    (!e.is_empty()).then(|| [e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0)])
}

/// A cube's period, as people read it.
fn period(e: &Entry) -> String {
    match &e.end {
        Some(end) if *end == e.start => e.start.clone(),
        Some(end) => format!("{} – {end}", e.start),
        None => crate::i18n::fmt("{start} – to date", &[("start", &e.start)]),
    }
}

/// The pane.
pub fn pane(app: &mut RustGisApp, ui: &mut egui::Ui) {
    // The hosted list, asked for once a run of the app; not in tests, which keep to the built-in
    // one.
    #[cfg(not(test))]
    {
        cubes::fetch_hosted();
        jaxa::fetch();
    }
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(
        egui::RichText::new(tl!("Data cubes ready to open: a series of images over time, with its collection, band and range filled in."))
            .color(t.text_dim),
    );
    let view = view_bbox(app);
    egui::Grid::new("cubes_filters").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let st = &mut app.ui.cubes;
        let input_label = ui.label(tl!("Theme"));
        let shown = if st.theme.is_empty() { tl!("All").to_string() } else { tl!(st.theme.as_str()).to_string() };
        egui::ComboBox::from_id_salt("cubes_theme")
            .selected_text(shown)
            .width(180.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut st.theme, String::new(), tl!("All"));
                for th in cubes::THEMES {
                    ui.selectable_value(&mut st.theme, th.to_string(), tl!(*th));
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Search"));
        ui.add(egui::TextEdit::singleline(&mut st.query).hint_text(tl!("NDVI, MODIS, rain…")).desired_width(180.0))
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("From"));
        ui.add(egui::TextEdit::singleline(&mut st.start).hint_text("2024-01-01").desired_width(110.0)).labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("To"));
        ui.add(egui::TextEdit::singleline(&mut st.end).hint_text("2024-12-31").desired_width(110.0)).labelled_by(input_label.id);
        ui.end_row();
    });
    ui.checkbox(&mut app.ui.cubes.here, tl!("Only cubes with data where the map is"));
    ui.label(egui::RichText::new(tl!("With no period, a cube is added over its last full calendar year.")).small().color(t.text_dim));
    jaxa_line(ui, &t);
    let all = cubes::list();
    let st = &app.ui.cubes;
    let area = if st.here { view } else { None };
    let shown = cubes::select(&all, Some(st.theme.as_str()), area, Some(st.query.as_str())).unwrap_or_default();
    if let Some(e) = &st.error {
        ui.label(egui::RichText::new(e.as_str()).color(egui::Color32::from_rgb(0xC0, 0x30, 0x30)).small());
    }
    if let Some(i) = &st.info {
        ui.label(egui::RichText::new(i.as_str()).weak());
    }
    if shown.is_empty() {
        ui.label(egui::RichText::new(tl!("No data cube matches")).weak());
        return;
    }
    let mut action: Option<(bool, Entry)> = None;
    egui::ScrollArea::vertical().id_salt("cubes_list").show(ui, |ui| {
        for e in &shown {
            ui.separator();
            let title = ui.label(egui::RichText::new(e.name.as_str()).strong());
            // A JAXA Earth collection's licence is read once its entry comes into view.
            let shown = ui.is_rect_visible(title.rect);
            ui.label(egui::RichText::new(format!("{} · {} · {} · {}", e.provider, period(e), e.step, e.resolution)).small());
            let source = match &e.source {
                Source::Stac { collection, asset } => format!("STAC {collection} · {asset}"),
                Source::Zarr { variable } => format!("Zarr · {variable}"),
            };
            let terms =
                if e.credit.is_empty() { e.license.clone() } else { crate::i18n::fmt("Credit: {credit}", &[("credit", &e.credit)]) };
            ui.label(egui::RichText::new(format!("{source} · {terms}")).small().color(t.text_dim));
            ui.horizontal_wrapped(|ui| {
                // A cube is read on the desktop: the browser can't add one yet, nor work out a
                // trend over a collection.
                let desktop = !cfg!(target_arch = "wasm32");
                let why = tl!("The desktop app adds data cubes");
                // A STAC cube is read where the map is; a Zarr variable whole.
                let stac = matches!(e.source, Source::Stac { .. });
                let (add_tip, trend_tip) = if stac {
                    (
                        tl!("Adds the cube where the map is, following the time slider through its dates"),
                        tl!("Opens Trend Analysis with this cube, where the map is"),
                    )
                } else {
                    (
                        tl!("Adds the whole cube, following the time slider through its dates"),
                        tl!("Opens Trend Analysis with this cube, read whole"),
                    )
                };
                if ui
                    .add_enabled(desktop, egui::Button::new(tl!("Add to Time Slider")))
                    .on_hover_text(add_tip)
                    .on_disabled_hover_text(why)
                    .clicked()
                {
                    action = Some((false, (*e).clone()));
                }
                if ui
                    .add_enabled(desktop, egui::Button::new(tl!("Trend Analysis…")))
                    .on_hover_text(trend_tip)
                    .on_disabled_hover_text(why)
                    .clicked()
                {
                    action = Some((true, (*e).clone()));
                }
                ui.hyperlink_to(tl!("About"), e.about.as_str());
                if !e.credit.is_empty() {
                    licence_link(ui, e, shown);
                }
            });
        }
    });
    match action {
        Some((false, e)) => add(app, &e),
        Some((true, e)) => trend(app, &e),
        None => {}
    }
}

/// What JAXA Earth's catalog adds to the list, or why it doesn't yet.
fn jaxa_line(ui: &mut egui::Ui, t: &crate::theme::Tokens) {
    match jaxa::status() {
        jaxa::Status::Unread => {}
        jaxa::Status::Reading => {
            ui.label(egui::RichText::new(tl!("Reading JAXA Earth's catalog…")).small().color(t.text_dim));
            // The tile cache's waker comes with the map view, which a layout, the model builder
            // or the catalog view keeps from drawing: look again until the catalog is read.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        jaxa::Status::Read(c) => {
            let text = crate::i18n::fmt(
                "JAXA Earth: {cubes} cubes from its catalog; {left} of its {datasets} datasets are left out (hover for why)",
                &[("cubes", &c.entries.len().to_string()), ("left", &c.left_out.len().to_string()), ("datasets", &c.datasets.to_string())],
            );
            let why: Vec<String> = c.left_out.iter().map(|l| format!("{} ({}): {}", l.title, l.id, l.why)).collect();
            ui.label(egui::RichText::new(text).small().color(t.text_dim)).on_hover_text(why.join("\n"));
            if !c.cut.is_empty() {
                let text = crate::i18n::fmt(
                    "JAXA Earth: {n} of its catalog's texts are cut or not read (hover for which)",
                    &[("n", &c.cut.len().to_string())],
                );
                ui.label(egui::RichText::new(text).small().color(t.text_dim)).on_hover_text(c.cut.join("\n"));
            }
            let hidden = cubes::jaxa_hidden();
            if !hidden.is_empty() {
                let text = crate::i18n::fmt(
                    "JAXA Earth: {n} of its cubes have the id of one of the list's own and are left out (hover for which)",
                    &[("n", &hidden.len().to_string())],
                );
                ui.label(egui::RichText::new(text).small().color(t.text_dim)).on_hover_text(hidden.join("\n"));
            }
        }
        jaxa::Status::Failed(why) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt("JAXA Earth's catalog couldn't be read: {why}", &[("why", &why)]))
                        .small()
                        .color(egui::Color32::from_rgb(0xC0, 0x30, 0x30)),
                );
                if ui.small_button(tl!("Try again")).clicked() {
                    jaxa::retry();
                }
            });
        }
    }
}

/// A JAXA Earth cube's licence: its collection's own once read (asked for when `shown`), JAXA's
/// terms when it can't be.
fn licence_link(ui: &mut egui::Ui, e: &Entry, shown: bool) {
    match jaxa::licence(&e.url) {
        Some(Ok(l)) => {
            let tip = l.title.clone().unwrap_or_else(|| l.url.clone());
            ui.hyperlink_to(tl!("Licence"), l.url.as_str()).on_hover_text(tip);
        }
        Some(Err(why)) => {
            ui.hyperlink_to(tl!("JAXA's terms"), jaxa::TERMS)
                .on_hover_text(crate::i18n::fmt("The collection's own licence wasn't read: {why}", &[("why", &why)]));
        }
        None => {
            // Not in tests, which read nothing from the web.
            #[cfg(not(test))]
            if shown {
                jaxa::fetch_licence(&e.url);
                // As for the catalog: look again until the licence is read.
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
            #[cfg(test)]
            let _ = shown;
            ui.label(egui::RichText::new(tl!("Licence…")).small().weak());
        }
    }
}

/// The period typed, if any: both ends, or neither.
fn typed_period(app: &RustGisApp) -> Result<Option<(String, String)>, String> {
    let st = &app.ui.cubes;
    match (st.start.trim(), st.end.trim()) {
        ("", "") => Ok(None),
        (a, b) if !a.is_empty() && !b.is_empty() => Ok(Some((a.to_string(), b.to_string()))),
        _ => Err(tl!("Give both ends of the period, or neither").to_string()),
    }
}

/// Adds a cube (`cube.open`): a STAC cube where the map is, a Zarr variable whole.
fn add(app: &mut RustGisApp, e: &Entry) {
    app.ui.cubes.info = None;
    let mut p = json!({"id": e.id});
    if matches!(e.source, Source::Stac { .. }) {
        p["bbox"] = json!("view");
    }
    match typed_period(app) {
        Ok(Some((a, b))) => {
            p["start"] = json!(a);
            p["end"] = json!(b);
        }
        Ok(None) => {}
        Err(why) => {
            app.ui.cubes.error = Some(why);
            return;
        }
    }
    match app.run("cube.open", p) {
        Ok(r) => {
            let st = &mut app.ui.cubes;
            st.error = None;
            let (start, end) = (r["start"].as_str().unwrap_or(""), r["end"].as_str().unwrap_or(""));
            st.info = Some(match &e.source {
                Source::Stac { .. } => crate::i18n::fmt(
                    "{layers} tiles, {dates} dates from {start} to {end}: the time slider steps through them",
                    &[("layers", &r["layers"].to_string()), ("dates", &r["dates"].to_string()), ("start", start), ("end", end)],
                ),
                Source::Zarr { .. } => {
                    crate::i18n::fmt("Added from {start} to {end}: the time slider steps through it", &[("start", start), ("end", end)])
                }
            });
            app.map.invalidate();
        }
        Err(why) => app.ui.cubes.error = Some(why),
    }
}

/// Takes a cube to Trend Analysis, with the period typed (a trend needs one).
fn trend(app: &mut RustGisApp, e: &Entry) {
    app.ui.cubes.info = None;
    let period = match typed_period(app) {
        Ok(p) => p,
        Err(why) => {
            app.ui.cubes.error = Some(why);
            return;
        }
    };
    let f = &mut app.workflow.trend;
    f.cube = Some((e.id.clone(), e.name.clone()));
    if let Some((a, b)) = period {
        (f.start, f.end) = (a, b);
    }
    app.workflow.error = None;
    app.ui.cubes.error = None;
    if let Err(why) = app.run("ui.pane.workflows", json!({"section": "trend"})) {
        app.ui.cubes.error = Some(why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn harness(app: RustGisApp, draw: fn(&mut RustGisApp, &mut egui::Ui)) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(520.0, 2400.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                draw(app, ui);
            },
            app,
        )
    }

    /// The Data Cubes pane opens from Web Data, filters by theme and words (anywhere, when not
    /// only where the map is), and refuses half a period before anything runs.
    #[test]
    fn the_pane_lists_cubes_by_theme_and_words() {
        // The list as another test leaves it: with JAXA Earth's datasets read.
        jaxa_read();
        let mut app = RustGisApp::with_sample();
        app.run("ui.dialog", json!({"name": "dataCubes"})).unwrap();
        assert_eq!(app.ui.web_data, "dataCubes");
        (app.ui.cubes.theme, app.ui.cubes.here) = ("water".into(), false);
        let mut h = harness(app, pane);
        h.run();
        assert!(h.query_by_role_and_label(egui::accesskit::Role::ComboBox, "Theme").is_some());
        assert!(h.query_by_role_and_label(egui::accesskit::Role::TextInput, "Search").is_some());
        // Australia's and Africa's water observations, and ERA5's precipitation and soil moisture.
        let water = h.query_all_by_role_and_label(egui::accesskit::Role::Button, "Add to Time Slider").count();
        assert_eq!(water, cubes::select(&cubes::list(), Some("water"), None, None).unwrap().len());
        assert!(water >= 4, "{water}");
        assert_eq!(h.query_all_by_label("Water observations, annual frequency of water seen (Landsat)").count(), 2);
        h.state_mut().ui.cubes.query = "africa".into();
        h.run();
        assert_eq!(h.query_all_by_role_and_label(egui::accesskit::Role::Button, "Add to Time Slider").count(), 1);
        h.state_mut().ui.cubes.query = "no such cube".into();
        h.run();
        assert!(h.query_by_label("No data cube matches").is_some());
        // Half a period is refused, the cube not added.
        let app = h.state_mut();
        app.ui.cubes.start = "2024-01-01".into();
        app.ui.cubes.info = Some("an earlier add".into());
        let e = cubes::find("era5-2m-temperature").unwrap();
        add(app, &e);
        assert!(app.ui.cubes.error.as_deref().is_some_and(|e| e.contains("both ends")));
        // What an earlier add said goes with a failed one.
        assert!(app.ui.cubes.info.is_none());
        assert!(app.session.map().layers.iter().all(|l| l.name != e.name));
    }

    /// JAXA Earth's catalog as read on 2026-11-08 (its fixture in the tiles crate), taken as the
    /// pane takes it: its number of cubes.
    fn jaxa_read() -> usize {
        jaxa::take(include_str!("../../tiles/tests/data/jaxa_catalog.v2.md").as_bytes()).unwrap()
    }

    /// JAXA Earth's catalog, once read, adds its datasets to the list: found by its keywords, each
    /// with its credit, and the pane says how many of its datasets can't be cubes.
    #[test]
    fn jaxa_earths_datasets_join_the_list() {
        assert_eq!(jaxa_read(), 80);
        let mut app = RustGisApp::with_sample();
        (app.ui.cubes.here, app.ui.cubes.query) = (false, "gsmap monthly".into());
        let mut h = harness(app, pane);
        h.run();
        assert!(h.query_by_label("JAXA Earth: 80 cubes from its catalog; 41 of its 119 datasets are left out (hover for why)").is_some());
        assert!(h.query_by_label("Precipitation Rate (Monthly)").is_some());
        assert!(h.query_by_label_contains("Credit: JAXA, Precipitation Rate (Monthly)").is_some());
        // Its licence is read from its collection, which tests leave alone.
        assert!(h.query_all_by_label("Licence…").count() >= 1);
        let shown = h.query_all_by_role_and_label(egui::accesskit::Role::Button, "Add to Time Slider").count();
        assert_eq!(shown, cubes::select(&cubes::list(), None, None, Some("gsmap monthly")).unwrap().len());
        assert!(shown >= 2, "{shown}");
    }

    /// Trend Analysis… opens Trend Analysis on the cube, with the period typed; its form shows the
    /// cube, asks for a period before it runs, and goes back to layers when asked.
    #[test]
    fn a_cube_goes_to_trend_analysis() {
        let mut app = RustGisApp::with_sample();
        (app.ui.cubes.start, app.ui.cubes.end) = ("2001".into(), "2024".into());
        let e = cubes::find("bdc-mod13q1-ndvi").unwrap();
        trend(&mut app, &e);
        assert_eq!(app.workflow.section, "trend");
        assert_eq!(app.workflow.trend.cube.as_ref().map(|c| c.0.as_str()), Some("bdc-mod13q1-ndvi"));
        assert_eq!((app.workflow.trend.start.as_str(), app.workflow.trend.end.as_str()), ("2001", "2024"));
        app.workflow.trend.end.clear();
        let mut h = harness(app, |app, ui| crate::analysispane::workflows(app, ui, &crate::theme::Tokens::light()));
        h.run();
        assert!(h.query_by_label_contains("Data cube: MODIS NDVI").is_some());
        // No layers to pick, nor dates to type, while a cube is the source.
        assert!(h.query_by_label("Layers, a date each").is_none());
        assert!(h.query_by_label("Dates").is_none());
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Run").click();
        h.run();
        let error = h.state().workflow.error.clone().unwrap_or_default();
        assert!(error.contains("From and To"), "{error}");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Use layers instead").click();
        h.run();
        assert!(h.state().workflow.trend.cube.is_none());
        h.run();
        assert!(h.query_by_label("Layers, a date each").is_some());
        assert!(h.query_by_label("Dates").is_some());
        // A Zarr cube is read whole, and the form says so.
        let e = cubes::find("era5-2m-temperature").unwrap();
        trend(h.state_mut(), &e);
        h.run();
        assert!(h.query_by_label(&format!("Data cube: {}, read whole", e.name)).is_some());
        assert!(h.query_by_label_contains("where the map is").is_none());
    }
}
