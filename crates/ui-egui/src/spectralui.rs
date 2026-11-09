//! The spectral profile: a multiband raster's bands charted at points clicked on the map, up to
//! six compared, against wavelength when the file gives one for every band and band number
//! otherwise. It is the Workflows pane's Spectral Analysis section, and Identify's pop-up on a
//! pixel of two or more bands adds that pixel to it. After GeoLibre's spectral profile
//! (opengeos/geolibre#1818, MIT): its TypeScript (`cog-spectral-profile.ts` and the NetCDF
//! profile chart) was read for what it does, and this Rust was written anew, no code copied.

use egui::{Color32, Pos2, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, regular, semibold};

/// The most points compared, a line each (as the Time Series pane keeps).
pub const MAX_POINTS: usize = 6;

/// The Workflows pane's section the profile is.
pub const SECTION: &str = "spectral";

/// What map clicks are for while the section picks points: each adds one.
pub const PURPOSE: &str = "spectral";

/// A point read: where, its pixel, and every band's value there (`None` where it has no data).
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Its number, from 1: it keeps its colour and marker when an older point goes.
    pub n: u32,
    /// Its colour's place in [`colour`]'s list: the first no other point kept has.
    pub slot: usize,
    pub lon: f64,
    pub lat: f64,
    pub pixel: (i64, i64),
    pub values: Vec<Option<f64>>,
}

/// The profile: the raster read, its bands' names and wavelengths, and the points read on it.
#[derive(Default)]
pub struct Spectral {
    pub layer: String,
    pub bands: Vec<String>,
    /// Each band's wavelength in nanometres; empty when the file gives none.
    pub wavelengths: Vec<Option<f64>>,
    /// The map (its place in the project) and layer the points were read on: a raster of the
    /// same name on another map, or added again, isn't the same one.
    pub source: Option<(usize, u32)>,
    /// The pixels the points were read from: a raster edited in place (Pixel Editor, say) keeps
    /// its layer and name but not these, so its points are no longer what it holds.
    read_on: std::sync::Weak<rustgis_engine::data::raster::Raster>,
    /// Oldest first.
    pub samples: Vec<Sample>,
    /// The last point's number.
    last: u32,
    /// What the last action said (an error when true).
    pub message: Option<(String, bool)>,
}

/// Why a profile's points are taken off.
pub enum Restart<'a> {
    /// Another raster, by this name, is read now.
    Other(&'a str),
    /// Another map is shown: the points were read on the one before.
    OtherMap,
    /// Their raster has changed or gone.
    Gone,
}

impl Spectral {
    /// Takes every point off, as they aren't comparable with what is read now, and says how many
    /// went and why.
    pub fn restart(&mut self, why: Restart<'_>) {
        let n = self.samples.len();
        self.samples.clear();
        self.message = (n > 0).then(|| {
            let (n, old) = (n.to_string(), self.layer.clone());
            let text = match why {
                Restart::Other(now) => crate::i18n::fmt(
                    "{n} point(s) taken off: they were read on {old}, not on {now}.",
                    &[("n", &n), ("old", &old), ("now", now)],
                ),
                Restart::OtherMap => {
                    crate::i18n::fmt("{n} point(s) taken off: they were read on {old} on another map.", &[("n", &n), ("old", &old)])
                }
                Restart::Gone => {
                    crate::i18n::fmt("{n} point(s) taken off: {old} has changed or gone since they were read.", &[("n", &n), ("old", &old)])
                }
            };
            (text, false)
        });
    }

    /// Empties it.
    pub fn clear(&mut self) {
        self.samples.clear();
        self.message = None;
    }

    /// The bands' places along the chart's x axis: their wavelengths when every band has one,
    /// else their numbers; and whether they are wavelengths.
    pub fn axis(&self) -> (Vec<f64>, bool) {
        let all: Option<Vec<f64>> = self.wavelengths.iter().copied().collect();
        match all.filter(|w| !w.is_empty() && w.len() == self.bands.len()) {
            Some(w) => (w, true),
            None => ((1..=self.bands.len()).map(|b| b as f64).collect(), false),
        }
    }
}

/// A point's colour, by its slot: the same on the chart, its legend and its marker.
pub fn colour(t: &Tokens, slot: usize) -> Color32 {
    let colours = [
        t.accent,
        Color32::from_rgb(214, 96, 77),
        Color32::from_rgb(77, 146, 33),
        Color32::from_rgb(118, 42, 131),
        Color32::from_rgb(230, 171, 2),
        Color32::from_rgb(27, 158, 119),
    ];
    colours.get(slot % colours.len()).copied().unwrap_or(t.accent)
}

/// The map (its place in the project) and layer the commands read as `layer`.
fn source_of(app: &RustGisApp, layer: &str) -> Option<(usize, u32)> {
    app.session.map().layer_by_name(layer).map(|l| (app.session.project.active_map, l.id))
}

/// The pixels of the raster `layer` names now, for [`Spectral::read_on`] (none when it has none).
fn data_of(app: &RustGisApp, layer: &str) -> std::sync::Weak<rustgis_engine::data::raster::Raster> {
    let data = app.session.map().layer_by_name(layer).and_then(|l| l.raster.as_ref()?.data.as_ref());
    data.map(std::sync::Arc::downgrade).unwrap_or_default()
}

/// The raster the points were read on, renamed since: the profile follows its new name (when
/// commands find it by that name), so its points stay.
pub fn follow_rename(app: &mut RustGisApp) {
    let Some((map, id)) = app.workflow.spectral.source else { return };
    if map != app.session.project.active_map {
        return;
    }
    let Some(name) = app.session.map().layer(id).map(|l| l.name.clone()) else { return };
    if name == app.workflow.spectral.layer || app.session.map().layer_by_name(&name).map(|l| l.id) != Some(id) {
        return;
    }
    if app.workflow.layer == app.workflow.spectral.layer {
        app.workflow.layer = name.clone();
    }
    app.workflow.spectral.layer = name;
}

/// Whether the points were read on the raster the profile's name finds now, as it is now. (A
/// `Weak` keeps its allocation, so another raster can't take the address the points' one had.)
fn fresh(app: &RustGisApp) -> bool {
    let sp = &app.workflow.spectral;
    source_of(app, &sp.layer) == sp.source && std::sync::Weak::ptr_eq(&data_of(app, &sp.layer), &sp.read_on)
}

/// The rasters a profile can be read on, and the ones left out of them.
pub struct Rasters {
    /// Those of two bands or more, by name.
    pub names: Vec<String>,
    /// Left out because an earlier layer has their name (commands find a layer by its name, so
    /// they would read that one).
    pub shadowed: usize,
    /// Left out because their pixels couldn't be read (their bands aren't known).
    pub unread: usize,
}

/// The rasters of the active map a profile can be read on (see [`Rasters`]).
pub fn multiband(app: &RustGisApp) -> Rasters {
    let map = app.session.map();
    let mut out = Rasters { names: Vec::new(), shadowed: 0, unread: 0 };
    for l in &map.layers {
        let Some(r) = l.raster.as_ref() else { continue };
        match r.data.as_ref() {
            Some(data) if data.band_count() >= 2 => {
                if map.layer_by_name(&l.name).map(|f| f.id) == Some(l.id) {
                    out.names.push(l.name.clone());
                } else {
                    out.shadowed += 1;
                }
            }
            Some(_) => {}
            None => out.unread += usize::from(r.error.is_some()),
        }
    }
    out
}

/// Whether Identify's result `info` is a pixel of a raster of two bands or more: its layer's id.
pub fn offered(app: &RustGisApp, info: &Value) -> Option<u32> {
    if info["raster"].as_bool() != Some(true) {
        return None;
    }
    let id = info["layerId"].as_u64().and_then(|id| u32::try_from(id).ok())?;
    let layer = app.session.map().layer(id)?;
    layer.raster.as_ref().and_then(|r| r.data.as_ref()).filter(|r| r.band_count() >= 2).map(|_| id)
}

/// Identify's Spectral Profile button: opens the section on the layer `id` and adds the point.
pub fn from_identify(app: &mut RustGisApp, id: u32, at: rustgis_engine::geom::Coord) {
    let Some(name) = app.session.map().layer(id).map(|l| l.name.clone()) else { return };
    // Commands find a raster by its name: an earlier layer sharing it would be read instead.
    if app.session.map().layer_by_name(&name).map(|l| l.id) != Some(id) {
        let text = crate::i18n::fmt(
            "{name}: another layer has this name, so its profile would be read from that one: rename one of them",
            &[("name", &name)],
        );
        app.session.notify(text, true);
        return;
    }
    // Opening the section ends a map pick; with it already shown, Pick Points stays as it is.
    let shown = app.ui.right == Some(crate::RightPane::Workflows) && app.workflow.section == SECTION;
    if !shown && let Err(e) = app.run("ui.pane.workflows", json!({"section": SECTION})) {
        app.session.notify(e, true);
        return;
    }
    app.workflow.layer = name.clone();
    add_point(app, &name, at);
}

/// Reads every band of `layer` at `at` and adds it to the profile, a raster other than the one
/// read so far starting it again; the oldest point goes past [`MAX_POINTS`]. What goes wrong is
/// said in the section.
pub fn add_point(app: &mut RustGisApp, layer: &str, at: rustgis_engine::geom::Coord) {
    // A rename since the points were read (with the section not drawn, say, and the point from
    // Identify) is the same raster: its points stay.
    follow_rename(app);
    let r = app.run("raster.spectral", json!({"layer": layer, "lon": at.x, "lat": at.y}));
    let (source, data) = (source_of(app, layer), data_of(app, layer));
    let sp = &mut app.workflow.spectral;
    let v = match r {
        Ok(v) => v,
        Err(e) => {
            sp.message = Some((e, true));
            return;
        }
    };
    let bands: Vec<String> = v["bands"].as_array().into_iter().flatten().map(|b| b.as_str().unwrap_or_default().to_string()).collect();
    let wavelengths: Vec<Option<f64>> = v["wavelengths"].as_array().into_iter().flatten().map(Value::as_f64).collect();
    // Another raster, or the same one's pixels or bands changed: its points aren't comparable,
    // and that they went is said. Names are compared as commands find layers, so the same raster
    // written in another case keeps its points.
    let mut restarted = None;
    let changed = !std::sync::Weak::ptr_eq(&sp.read_on, &data) || sp.bands != bands || sp.wavelengths != wavelengths;
    if !sp.layer.eq_ignore_ascii_case(layer) || sp.source != source || changed {
        let why = if !sp.layer.eq_ignore_ascii_case(layer) {
            Restart::Other(layer)
        } else if sp.source.map(|s| s.0) != source.map(|s| s.0) {
            Restart::OtherMap
        } else {
            Restart::Gone
        };
        sp.restart(why);
        restarted = sp.message.take();
        sp.source = source;
        sp.read_on = data;
        sp.bands = bands;
        sp.wavelengths = wavelengths;
    }
    sp.layer = layer.to_string();
    let pixel = (v["pixel"][0].as_i64().unwrap_or(0), v["pixel"][1].as_i64().unwrap_or(0));
    let values: Vec<Option<f64>> = v["values"].as_array().into_iter().flatten().map(Value::as_f64).collect();
    sp.last += 1;
    // The oldest goes first, so the new point may take its colour.
    let excess = (sp.samples.len() + 1).saturating_sub(MAX_POINTS);
    sp.samples.drain(..excess.min(sp.samples.len()));
    let slot = (0..MAX_POINTS).find(|k| sp.samples.iter().all(|s| s.slot != *k)).unwrap_or(0);
    sp.samples.push(Sample { n: sp.last, slot, lon: at.x, lat: at.y, pixel, values });
    // What this point did is said: an older one going, or the points before it taken off.
    sp.message = (excess > 0)
        .then(|| {
            let n = MAX_POINTS.to_string();
            (crate::i18n::fmt("Up to {n} points are compared: the oldest one was taken off.", &[("n", &n)]), false)
        })
        .or(restarted);
}

/// The section: the raster, Pick Points, the chart and its points.
pub fn section(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Spectral profile")).font(semibold(12.5)));
    ui.add_space(2.0);
    // Points read on a raster renamed since stay; those read on one that has since gone
    // (removed, added again, on another map) are dropped, even when no raster is left to choose.
    follow_rename(app);
    let sp = &app.workflow.spectral;
    if !sp.samples.is_empty() && !fresh(app) {
        let other_map = sp.source.is_some_and(|(map, _)| map != app.session.project.active_map);
        app.workflow.spectral.restart(if other_map { Restart::OtherMap } else { Restart::Gone });
    }
    let Rasters { names: rasters, shadowed, unread } = multiband(app);
    if shadowed > 0 {
        let n = shadowed.to_string();
        let text = crate::i18n::fmt("{n} raster(s) left out: another layer has the same name. Rename them to chart them.", &[("n", &n)]);
        ui.label(egui::RichText::new(text).small().color(t.red));
    }
    if unread > 0 {
        let n = unread.to_string();
        let text = crate::i18n::fmt("{n} raster(s) couldn't be read, so they are left out.", &[("n", &n)]);
        ui.label(egui::RichText::new(text).small().color(t.red));
    }
    if rasters.is_empty() {
        ui.label(
            egui::RichText::new(tl!("Add a raster of two or more bands first, such as a Landsat or Sentinel-2 scene.")).weak().small(),
        );
        // Points just taken off are said here too.
        if let Some((text, _)) = &app.workflow.spectral.message {
            ui.label(egui::RichText::new(text.as_str()).small().color(t.text_dim));
        }
        // With nothing to read, picking ends: a click would be taken and go nowhere.
        if app.workflow.pick.as_ref().is_some_and(|p| p.purpose == PURPOSE) {
            app.workflow.pick = None;
        }
        return;
    }
    if !rasters.contains(&app.workflow.layer) {
        app.workflow.layer = if rasters.contains(&app.workflow.spectral.layer) {
            app.workflow.spectral.layer.clone()
        } else {
            rasters.first().cloned().unwrap_or_default()
        };
    }
    egui::ComboBox::from_id_salt("wf_sp")
        .selected_text(app.workflow.layer.as_str())
        .width(180.0)
        .show_ui(ui, |ui| {
            for o in &rasters {
                ui.selectable_value(&mut app.workflow.layer, o.clone(), o.as_str());
            }
        })
        .response
        .on_hover_text(tl!("Layer"));
    // Points on another raster aren't comparable with this one's.
    if !app.workflow.spectral.samples.is_empty() && app.workflow.spectral.layer != app.workflow.layer {
        let now = app.workflow.layer.clone();
        app.workflow.spectral.restart(Restart::Other(&now));
    }
    // Points clicked on the map while picking: each is read, and picking goes on.
    let clicked = match &mut app.workflow.pick {
        Some(pick) if pick.purpose == PURPOSE => std::mem::take(&mut pick.points),
        _ => Vec::new(),
    };
    let layer = app.workflow.layer.clone();
    for c in clicked {
        add_point(app, &layer, c);
    }
    ui.horizontal_wrapped(|ui| {
        let picking = app.workflow.pick.as_ref().is_some_and(|p| p.purpose == PURPOSE);
        if ui
            .selectable_label(picking, tl!("Pick Points"))
            .on_hover_text(tl!("Each click on the map adds a point, until you click this again."))
            .clicked()
        {
            app.workflow.pick =
                if picking { None } else { Some(crate::analysispane::Pick { purpose: PURPOSE, needed: 1, points: Vec::new() }) };
        }
        if !app.workflow.spectral.samples.is_empty() {
            if ui.button(tl!("Clear")).clicked() {
                app.workflow.spectral.clear();
            }
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button(tl!("Export CSV…")).clicked()
                && let Some(path) = crate::dialogs::pick_save("spectral_profile.csv", &["csv"], tl!("Spectral profile"))
            {
                let sp = &mut app.workflow.spectral;
                sp.message = Some(match std::fs::write(&path, csv(sp)) {
                    Ok(()) => (crate::i18n::fmt("Saved {path}", &[("path", &path)]), false),
                    Err(e) => (crate::i18n::fmt("{path}: {error}", &[("path", &path), ("error", &e.to_string())]), true),
                });
            }
        }
    });
    if let Some((text, error)) = &app.workflow.spectral.message {
        let colour = if *error { t.red } else { t.text_dim };
        ui.label(egui::RichText::new(text.as_str()).small().color(colour));
    }
    if app.workflow.spectral.samples.is_empty() {
        ui.label(
            egui::RichText::new(tl!(
                "Pick points on the map, or click a pixel with Explore and press Spectral Profile in its pop-up, to chart every band there."
            ))
            .weak()
            .small(),
        );
        return;
    }
    chart(&app.workflow.spectral, ui, t);
    // The points, in their lines' colours, each with a button to take it off.
    let mut remove = None;
    for (i, s) in app.workflow.spectral.samples.iter().enumerate() {
        ui.horizontal(|ui| {
            let c = colour(t, s.slot);
            let (n, lon, lat) = (s.n.to_string(), format!("{:.5}", s.lon), format!("{:.5}", s.lat));
            let (col, row) = (s.pixel.0.to_string(), s.pixel.1.to_string());
            let text = crate::i18n::fmt(
                "● {n}  {lon}°, {lat}°  ·  pixel {col}, {row}",
                &[("n", &n), ("lon", &lon), ("lat", &lat), ("col", &col), ("row", &row)],
            );
            ui.label(egui::RichText::new(text).small().color(c));
            if ui
                .add(egui::Button::new(egui::RichText::new(tl!("×")).small()).frame(false))
                .on_hover_text(tl!("Take this point off"))
                .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        app.workflow.spectral.samples.remove(i);
        // What the last point added said is no longer about these points.
        app.workflow.spectral.message = None;
    }
}

/// The range a chart axis is drawn over: `lo` to `hi`, or one either side of a single value, so a
/// flat profile is drawn across the middle.
fn scale(lo: f64, hi: f64) -> (f64, f64) {
    if hi > lo { (lo, hi) } else { (lo - 1.0, hi + 1.0) }
}

/// The values written beside the chart: the highest and lowest read, or the one value when every
/// point gives the same.
fn value_labels(lo: f64, hi: f64) -> Vec<f64> {
    if hi > lo { vec![hi, lo] } else { vec![lo] }
}

/// The x axis' title: the wavelengths the bands span (one when they share it), or their numbers.
fn axis_label(x0: f64, x1: f64, by_wavelength: bool, bands: usize) -> String {
    let fmt = |v: f64| rustgis_engine::map::symbology::fmt_num(v);
    if !by_wavelength {
        let n = bands.to_string();
        crate::i18n::fmt("Bands 1–{n}", &[("n", &n)])
    } else if x1 > x0 {
        crate::i18n::fmt("Wavelength {from}–{to} nm", &[("from", &fmt(x0)), ("to", &fmt(x1))])
    } else {
        crate::i18n::fmt("Wavelength {nm} nm", &[("nm", &fmt(x0))])
    }
}

/// The chart: a line for each point through its bands, in the order of the x axis; hovering
/// shows a band's name, wavelength and the points' values.
fn chart(sp: &Spectral, ui: &mut Ui, t: &Tokens) {
    let (xs, by_wavelength) = sp.axis();
    // The bands in the order of the axis: a stack written red, green, blue draws no zigzag.
    let mut order: Vec<usize> = (0..xs.len()).collect();
    order.sort_by(|a, b| xs.get(*a).copied().unwrap_or(0.0).total_cmp(&xs.get(*b).copied().unwrap_or(0.0)));
    let (x0, x1) = xs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(*x), b.max(*x)));
    let values = sp.samples.iter().flat_map(|s| s.values.iter().flatten());
    let (lo, hi) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    if !(x0.is_finite() && lo.is_finite()) {
        ui.label(egui::RichText::new(tl!("No data at the points picked.")).weak().small());
        return;
    }
    let labels = value_labels(lo, hi);
    let axis = axis_label(x0, x1, by_wavelength, sp.bands.len());
    let ((x0, x1), (lo, hi)) = (scale(x0, x1), scale(lo, hi));
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 190.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, t.canvas);
    let fmt = |v: f64| rustgis_engine::map::symbology::fmt_num(v);
    // A gutter on the left for the values at the top and bottom, clear of the lines, and room for
    // the axis' range under it.
    let gutter = labels.iter().map(|v| crate::widgets::text_w(ui, &fmt(*v), regular(10.0))).fold(0.0_f32, f32::max) + 12.0;
    let plot = egui::Rect::from_min_max(rect.min + vec2(gutter, 12.0), rect.max - vec2(12.0, 30.0));
    let to = |x: f64, v: f64| {
        pos2(plot.min.x + ((x - x0) / (x1 - x0)) as f32 * plot.width(), plot.max.y - ((v - lo) / (hi - lo)) as f32 * plot.height())
    };
    // A tick under each band.
    for x in &xs {
        let px = to(*x, lo).x;
        painter.line_segment([pos2(px, plot.max.y + 2.0), pos2(px, plot.max.y + 6.0)], Stroke::new(1.0, t.text_dim));
    }
    painter.line_segment([pos2(plot.min.x, plot.max.y + 2.0), pos2(plot.max.x, plot.max.y + 2.0)], Stroke::new(1.0, t.border_strong));
    for s in &sp.samples {
        let c = colour(t, s.slot);
        // Runs between bands with no data, so a gap shows as one.
        let mut runs: Vec<Vec<Pos2>> = vec![Vec::new()];
        for b in &order {
            match (xs.get(*b), s.values.get(*b).copied().flatten()) {
                (Some(x), Some(v)) => runs.last_mut().into_iter().for_each(|r| r.push(to(*x, v))),
                _ if runs.last().is_some_and(|r| !r.is_empty()) => runs.push(Vec::new()),
                _ => {}
            }
        }
        for run in runs.into_iter().filter(|r| !r.is_empty()) {
            for p in &run {
                painter.circle_filled(*p, 2.5, c);
            }
            if run.len() > 1 {
                painter.add(egui::Shape::line(run, Stroke::new(1.6, c)));
            }
        }
    }
    for v in &labels {
        painter.text(pos2(plot.min.x - 6.0, to(x0, *v).y), egui::Align2::RIGHT_CENTER, fmt(*v), regular(10.0), t.text_dim);
    }
    painter.text(rect.center_bottom() + vec2(0.0, -4.0), egui::Align2::CENTER_BOTTOM, axis, regular(10.0), t.text_dim);
    // Hovering: the band nearest the pointer, marked, and every point's value there.
    let near = resp.hover_pos().and_then(|pos| {
        (0..xs.len()).min_by(|a, b| {
            let d = |i: &usize| xs.get(*i).map_or(f32::INFINITY, |x| (to(*x, lo).x - pos.x).abs());
            d(a).total_cmp(&d(b))
        })
    });
    if let Some(b) = near {
        let px = xs.get(b).map_or(plot.min.x, |x| to(*x, lo).x);
        painter.line_segment([pos2(px, plot.min.y), pos2(px, plot.max.y)], Stroke::new(1.0, t.text_dim));
        // Bands of one wavelength sit at one place: each is shown.
        let at: Vec<usize> = (0..xs.len()).filter(|i| xs.get(*i) == xs.get(b)).collect();
        resp.on_hover_ui_at_pointer(|ui| {
            for b in at {
                let name = sp.bands.get(b).map_or("", String::as_str);
                let number = (b + 1).to_string();
                let title = match sp.wavelengths.get(b).copied().flatten() {
                    Some(w) => {
                        crate::i18n::fmt("Band {n}: {name}, {wavelength} nm", &[("n", &number), ("name", name), ("wavelength", &fmt(w))])
                    }
                    None => crate::i18n::fmt("Band {n}: {name}", &[("n", &number), ("name", name)]),
                };
                ui.label(egui::RichText::new(title).small().strong());
                for s in &sp.samples {
                    let v = s.values.get(b).copied().flatten().map_or_else(|| tl!("no data").to_string(), fmt);
                    let n = s.n.to_string();
                    ui.label(egui::RichText::new(crate::i18n::fmt("● {n}  {v}", &[("n", &n), ("v", &v)])).small().color(colour(t, s.slot)));
                }
            }
        });
    }
}

/// The profile as CSV: a row a band (its number, name and wavelength in nanometres), a column a
/// point, headed with its number and place. A name a spreadsheet would run as a formula is
/// written with a leading `'`, as the table's export writes it.
#[cfg(not(target_arch = "wasm32"))]
pub fn csv(sp: &Spectral) -> String {
    let text = |s: &str| {
        let guarded = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
        format!("\"{}\"", guarded.replace('"', "\"\""))
    };
    let mut out = String::from("band,name,wavelength_nm");
    for s in &sp.samples {
        out.push(',');
        out.push_str(&text(&format!("point {} ({}, {})", s.n, s.lon, s.lat)));
    }
    out.push('\n');
    for (b, name) in sp.bands.iter().enumerate() {
        let w = sp.wavelengths.get(b).copied().flatten().map(|w| w.to_string()).unwrap_or_default();
        out.push_str(&format!("{},{},{w}", b + 1, text(name)));
        for s in &sp.samples {
            out.push(',');
            if let Some(v) = s.values.get(b).copied().flatten() {
                out.push_str(&v.to_string());
            }
        }
        out.push('\n');
    }
    out
}

/// The points' numbered markers on the map, in their colours, while the section shows.
pub fn paint(app: &RustGisApp, p: &egui::Painter, t: &Tokens, to_screen: impl Fn(rustgis_engine::geom::Coord) -> Option<Pos2>) {
    // Points read on a raster since gone aren't drawn on the one there now.
    if app.ui.right != Some(crate::RightPane::Workflows) || app.workflow.section != SECTION || !fresh(app) {
        return;
    }
    for s in &app.workflow.spectral.samples {
        let Some(at) = to_screen(rustgis_engine::geom::Coord::new(s.lon, s.lat)) else { continue };
        let c = colour(t, s.slot);
        p.circle_filled(at, 7.0, c);
        p.circle_stroke(at, 7.0, Stroke::new(1.5, Color32::WHITE));
        p.text(at, egui::Align2::CENTER_CENTER, s.n.to_string(), semibold(10.0), Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A colour GeoTIFF of three bands whose wavelengths are given (blue, green, red, written in
    /// that order red first), 4 × 4 pixels near 10°E 45°N, as base64.
    fn rgb_tiff_b64(wavelengths: bool) -> String {
        use base64::Engine;
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        let gt = GeoTransform { origin_x: 10.0, origin_y: 45.0, pixel_width: 0.01, pixel_height: -0.01 };
        // Red, green, blue: 30 + i, 20 + i, 10 + i at pixel i.
        let bands = (0..3).map(|b| (0..16).map(|i| (30 - b * 10 + i) as f32).collect()).collect();
        let mut r = Raster::new("scene", 4, 4, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
        if wavelengths {
            for (band, nm) in r.bands.iter_mut().zip([665.0, 560.0, 490.0]) {
                band.wavelength = Some(nm);
            }
        }
        base64::engine::general_purpose::STANDARD.encode(geotiff::write(&r, 0).unwrap())
    }

    fn app_with_scene(wavelengths: bool) -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        app.run("layer.addDataBytes", json!({"fileName": "scene.tif", "data": rgb_tiff_b64(wavelengths)})).unwrap();
        app
    }

    /// Pick Points: two clicks are read and charted, against wavelength when the file gives it;
    /// a point is taken off; the CSV has a row a band; Clear empties it.
    #[test]
    fn the_spectral_profile_charts_points_picked() {
        use egui_kittest::kittest::Queryable;
        let mut app = app_with_scene(true);
        app.run("ui.pane.workflows", json!({"section": SECTION})).unwrap();
        app.workflow.pick =
            Some(crate::analysispane::Pick { purpose: PURPOSE, needed: 1, points: vec![rustgis_engine::geom::Coord::new(10.005, 44.995)] });
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(420.0, 640.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let t = crate::theme::Tokens::get(ui.ctx());
                section(app, ui, &t);
            },
            app,
        );
        h.run_steps(2);
        assert_eq!(h.state().workflow.layer, "scene");
        assert_eq!(h.state().workflow.spectral.samples.len(), 1);
        assert!(h.state().workflow.pick.as_ref().is_some_and(|p| p.purpose == PURPOSE), "picking goes on");
        // A second click, at pixel 5 (column 1, row 1).
        if let Some(pick) = &mut h.state_mut().workflow.pick {
            pick.points.push(rustgis_engine::geom::Coord::new(10.015, 44.985));
        }
        h.run_steps(2);
        let sp = &h.state().workflow.spectral;
        assert_eq!(sp.samples.len(), 2);
        assert_eq!(sp.samples[1].pixel, (1, 1));
        assert_eq!(sp.samples[1].values, vec![Some(35.0), Some(25.0), Some(15.0)]);
        assert_eq!(sp.wavelengths, vec![Some(665.0), Some(560.0), Some(490.0)]);
        assert_eq!(sp.axis(), (vec![665.0, 560.0, 490.0], true));
        // The CSV: a header and a row a band.
        let text = csv(sp);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].starts_with("band,name,wavelength_nm,\"point 1 (10.005, 44.995)\""), "{text}");
        assert!(lines[3].starts_with("3,\"Band 3\",490") && lines[3].ends_with(",10,15"), "{text}");
        // The first point taken off: the second keeps its number, so its colour.
        h.get_all_by_label("×").next().unwrap().click();
        h.run_steps(2);
        assert_eq!(h.state().workflow.spectral.samples.iter().map(|s| s.n).collect::<Vec<_>>(), [2]);
        h.get_by_label("Clear").click();
        h.run_steps(2);
        assert!(h.state().workflow.spectral.samples.is_empty());
    }

    /// Without wavelengths the axis is the band numbers; past six points the oldest goes, and
    /// that is said; a point off the raster is said, not added.
    #[test]
    fn the_profile_keeps_six_points_and_says_what_it_leaves_out() {
        let mut app = app_with_scene(false);
        for i in 0..7 {
            add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005 + 0.001 * f64::from(i), 44.995));
        }
        let sp = &app.workflow.spectral;
        assert_eq!(sp.samples.len(), MAX_POINTS);
        assert_eq!(sp.samples.first().map(|s| s.n), Some(2));
        assert!(sp.message.as_ref().is_some_and(|(m, error)| !error && m.contains("oldest")), "{:?}", sp.message);
        assert!(sp.wavelengths.is_empty());
        assert_eq!(sp.axis(), (vec![1.0, 2.0, 3.0], false));
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(20.0, 10.0));
        let sp = &app.workflow.spectral;
        assert_eq!(sp.samples.len(), MAX_POINTS);
        assert!(sp.message.as_ref().is_some_and(|(m, error)| *error && m.contains("outside")), "{:?}", sp.message);
        // Another raster starts again.
        app.run("layer.addDataBytes", json!({"fileName": "other.tif", "data": rgb_tiff_b64(true)})).unwrap();
        add_point(&mut app, "other", rustgis_engine::geom::Coord::new(10.005, 44.995));
        assert_eq!(app.workflow.spectral.samples.len(), 1);
        assert_eq!(app.workflow.spectral.layer, "other");
        assert_eq!(
            app.workflow.spectral.message,
            Some(("6 point(s) taken off: they were read on scene, not on other.".to_string(), false)),
            "the points that went are said"
        );
    }

    /// A point added after one is taken off takes a colour no point kept has; a raster of the
    /// same name added again starts the profile again.
    #[test]
    fn a_new_point_takes_a_free_colour_and_a_new_raster_starts_again() {
        let mut app = app_with_scene(false);
        for i in 0..MAX_POINTS {
            add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005 + 0.001 * i as f64, 44.995));
        }
        app.workflow.spectral.samples.remove(1);
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        let slots: Vec<usize> = app.workflow.spectral.samples.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [0, 2, 3, 4, 5, 1], "the new point takes the colour point 2 left");
        // Past six, the oldest goes and the new point takes its colour.
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        let slots: Vec<usize> = app.workflow.spectral.samples.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [2, 3, 4, 5, 1, 0]);
        // The scene renamed: its points stay, under its new name.
        app.run("layer.rename", json!({"layer": "scene", "name": "renamed"})).unwrap();
        follow_rename(&mut app);
        assert!(fresh(&app));
        assert_eq!((app.workflow.spectral.layer.as_str(), app.workflow.spectral.samples.len()), ("renamed", MAX_POINTS));
        app.run("layer.rename", json!({"layer": "renamed", "name": "scene"})).unwrap();
        follow_rename(&mut app);
        // The scene removed and another of its name added: its points aren't the old ones.
        app.run("layer.remove", json!({"layer": "scene"})).unwrap();
        app.run("layer.addDataBytes", json!({"fileName": "scene.tif", "data": rgb_tiff_b64(false)})).unwrap();
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        assert_eq!(app.workflow.spectral.samples.len(), 1);
        // The scene removed with no raster left: the section drops its points.
        app.run("layer.remove", json!({"layer": "scene"})).unwrap();
        assert!(!fresh(&app), "its points aren't drawn");
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(420.0, 320.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let t = crate::theme::Tokens::get(ui.ctx());
                section(app, ui, &t);
            },
            app,
        );
        h.run_steps(1);
        assert!(h.state().workflow.spectral.samples.is_empty());
        assert_eq!(
            h.state().workflow.spectral.message,
            Some(("1 point(s) taken off: scene has changed or gone since they were read.".to_string(), false))
        );
    }

    /// Identify's pop-up offers the profile on a pixel of two bands or more, and its button opens
    /// the section there with the point read.
    #[test]
    fn identify_offers_the_profile_on_multiband_pixels() {
        let mut app = app_with_scene(true);
        // The pixel Identify reports where no feature is found.
        let info = rustgis_engine::cmd::raster::identify(&app.session, 10.005, 44.995).unwrap();
        let id = offered(&app, &info).expect("a pixel of three bands");
        from_identify(&mut app, id, rustgis_engine::geom::Coord::new(10.005, 44.995));
        assert_eq!(app.ui.right, Some(crate::RightPane::Workflows));
        assert_eq!(app.workflow.section, SECTION);
        assert_eq!(app.workflow.spectral.samples.len(), 1);
        // A feature isn't offered it.
        assert_eq!(offered(&app, &json!({"layer": "Cities", "layerId": id, "attributes": {}})), None);
        // A raster named as an earlier one (case aside) isn't read from that one: it is said.
        app.run("layer.addDataBytes", json!({"fileName": "copy.tif", "data": rgb_tiff_b64(true)})).unwrap();
        let copy = app.session.map().layer_by_name("copy").map(|l| l.id).expect("the copy");
        app.run("layer.rename", json!({"layer": "copy", "name": "Scene"})).unwrap();
        // The layer the name finds is read; the other one, which commands can't reach by its name.
        let found = app.session.map().layer_by_name("Scene").map(|l| l.id).expect("a scene");
        let (shadowed, name) = if found == copy { (id, "scene") } else { (copy, "Scene") };
        let before = app.workflow.spectral.samples.len();
        let mark = app.session.notice_mark();
        from_identify(&mut app, shadowed, rustgis_engine::geom::Coord::new(10.005, 44.995));
        assert_eq!(app.workflow.spectral.samples.len(), before, "nothing read from the other scene");
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said, [format!("{name}: another layer has this name, so its profile would be read from that one: rename one of them")]);
        // The section's list leaves it out too, and counts it.
        let rasters = multiband(&app);
        assert_eq!((rasters.names.len(), rasters.shadowed, rasters.unread), (1, 1, 0), "{:?}", rasters.names);
    }

    /// A raster whose pixels couldn't be read is left out of the list, and counted.
    #[test]
    fn an_unread_raster_is_counted() {
        let mut app = app_with_scene(false);
        app.run("layer.addDataBytes", json!({"fileName": "broken.tif", "data": rgb_tiff_b64(false)})).unwrap();
        let id = app.session.map().layer_by_name("broken").map(|l| l.id).expect("the second scene");
        let r = app.session.map_mut().layer_mut(id).and_then(|l| l.raster.as_mut()).expect("a raster");
        r.data = None;
        r.fail("the file is gone");
        let rasters = multiband(&app);
        assert_eq!((rasters.names, rasters.shadowed, rasters.unread), (vec!["scene".to_string()], 0, 1));
    }

    /// A flat profile's labels say what was read, not the room drawn around it.
    #[test]
    fn a_flat_profile_is_labelled_with_its_values() {
        assert_eq!(value_labels(0.0, 0.0), [0.0]);
        assert_eq!(value_labels(10.0, 35.0), [35.0, 10.0]);
        assert_eq!(scale(0.0, 0.0), (-1.0, 1.0));
        assert_eq!(scale(10.0, 35.0), (10.0, 35.0));
        assert_eq!(axis_label(500.0, 500.0, true, 3), "Wavelength 500 nm");
        assert_eq!(axis_label(490.0, 665.0, true, 3), "Wavelength 490–665 nm");
        assert_eq!(axis_label(1.0, 1.0, false, 1), "Bands 1–1");
    }

    /// The raster the points were read on, written in another case, is the same raster: they stay.
    #[test]
    fn the_same_raster_in_another_case_keeps_its_points() {
        let mut app = app_with_scene(false);
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        add_point(&mut app, "SCENE", rustgis_engine::geom::Coord::new(10.015, 44.985));
        let sp = &app.workflow.spectral;
        assert_eq!(sp.samples.len(), 2, "{:?}", sp.message);
        assert_eq!(sp.message, None);
    }

    /// Identify's button with the section already shown and Pick Points on: picking stays on.
    #[test]
    fn identify_keeps_pick_points_on() {
        let mut app = app_with_scene(false);
        app.run("ui.pane.workflows", json!({"section": SECTION})).unwrap();
        app.workflow.pick = Some(crate::analysispane::Pick { purpose: PURPOSE, needed: 1, points: Vec::new() });
        let id = app.session.map().layer_by_name("scene").map(|l| l.id).unwrap();
        from_identify(&mut app, id, rustgis_engine::geom::Coord::new(10.005, 44.995));
        assert_eq!(app.workflow.spectral.samples.len(), 1);
        assert!(app.workflow.pick.as_ref().is_some_and(|p| p.purpose == PURPOSE), "Pick Points stays on");
    }

    /// A point from Identify on a raster renamed while the section wasn't drawn: the same raster,
    /// so the points read before stay.
    #[test]
    fn identify_on_a_renamed_raster_keeps_its_points() {
        let mut app = app_with_scene(false);
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        app.run("layer.rename", json!({"layer": "scene", "name": "renamed"})).unwrap();
        let id = app.session.map().layer_by_name("renamed").map(|l| l.id).unwrap();
        from_identify(&mut app, id, rustgis_engine::geom::Coord::new(10.015, 44.985));
        let sp = &app.workflow.spectral;
        assert_eq!((sp.layer.as_str(), sp.samples.len()), ("renamed", 2), "{:?}", sp.message);
        assert_eq!(sp.message, None);
    }

    /// Pixels edited in place (same layer, same name): the points read before are taken off, and
    /// that is said; a point read after is kept.
    #[test]
    fn an_edited_raster_takes_the_points_off_and_says_so() {
        let mut app = app_with_scene(false);
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.015, 44.985));
        let ring = json!([[10.0, 45.0], [10.04, 45.0], [10.04, 44.96]]);
        app.run("raster.editPixels", json!({"layer": "scene", "polygon": ring, "value": 99})).unwrap();
        assert!(!fresh(&app), "the points were read on the pixels before the edit");
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(420.0, 320.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let t = crate::theme::Tokens::get(ui.ctx());
                section(app, ui, &t);
            },
            app,
        );
        h.run_steps(1);
        let sp = &h.state().workflow.spectral;
        assert!(sp.samples.is_empty());
        assert_eq!(sp.message, Some(("2 point(s) taken off: scene has changed or gone since they were read.".to_string(), false)));
        // Read again on the edited pixels: the point is kept, and fresh.
        add_point(h.state_mut(), "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        assert_eq!(h.state().workflow.spectral.samples.len(), 1);
        assert!(fresh(h.state()));
    }

    /// Another map shown: the points read on the one before are taken off, and that is said.
    #[test]
    fn another_map_takes_the_points_off_and_says_so() {
        let mut app = app_with_scene(false);
        add_point(&mut app, "scene", rustgis_engine::geom::Coord::new(10.005, 44.995));
        app.run("map.new", json!({"name": "Second"})).unwrap();
        assert_ne!(app.session.project.active_map, 0, "the new map is shown");
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(420.0, 320.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let t = crate::theme::Tokens::get(ui.ctx());
                section(app, ui, &t);
            },
            app,
        );
        h.run_steps(1);
        let sp = &h.state().workflow.spectral;
        assert!(sp.samples.is_empty());
        assert_eq!(sp.message, Some(("1 point(s) taken off: they were read on scene on another map.".to_string(), false)));
    }

    /// With no raster of two bands or more, the section says so and ends a pick left on.
    #[test]
    fn the_profile_ends_its_pick_when_no_raster_is_left() {
        let mut app = RustGisApp::with_sample();
        app.workflow.pick = Some(crate::analysispane::Pick { purpose: PURPOSE, needed: 1, points: Vec::new() });
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(420.0, 320.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let t = crate::theme::Tokens::get(ui.ctx());
                section(app, ui, &t);
            },
            app,
        );
        h.run_steps(1);
        assert!(h.state().workflow.pick.is_none());
    }
}
