//! The Report pane: pick a report, set what it lists (fields, groups, statistics, pictures,
//! attachments, a chart), add sections, and page through a preview.

use egui::{Color32, Ui, vec2};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};
use rustgis_engine::map::report::Section;

/// State of the Report pane.
#[derive(Default)]
pub struct ReportState {
    pub page: usize,
    /// The preview: what it shows (report JSON and page), its texture and the page count.
    pub preview: Option<(String, egui::TextureHandle, usize)>,
    /// The built report and its pages, for the report and project revision they were built from:
    /// turning pages only redraws a page, it doesn't rebuild the report.
    pub built: Option<((usize, u64), Result<(rustgis_engine::map::report::Doc, Vec<rustgis_engine::render::report::Page>), String>)>,
    pub stat_field: String,
    pub stat_kind: String,
    pub page_title: String,
    pub page_text: String,
    pub message: Option<(String, bool)>,
    /// The report whose Title and Where are being edited, and the text typed into them.
    pub editing: Option<usize>,
    pub title: String,
    pub where_clause: String,
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!(text)).font(semibold(12.5)));
    ui.add_space(2.0);
}

fn run(app: &mut RustGisApp, id: &str, p: Value) -> Option<Value> {
    match app.run(id, p) {
        Ok(v) => {
            app.reports.message = None;
            Some(v)
        }
        Err(e) => {
            app.reports.message = Some((e, true));
            None
        }
    }
}

pub fn report(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if let Some((m, err)) = &app.reports.message {
        let c = if *err { Color32::from_rgb(0xC0, 0x30, 0x30) } else { t.text_dim };
        ui.label(egui::RichText::new(m.as_str()).small().color(c));
    }
    if app.session.project.reports.is_empty() {
        ui.label(egui::RichText::new(tl!("No reports yet. Publish › New Report lists the selected layer's records.")).weak());
        if ui.button(tl!("New Report")).clicked() {
            run(app, "report.new", Value::Null);
        }
        return;
    }
    let Ok(i) = rustgis_engine::cmd::report::index(&app.session, &Value::Null) else { return };
    let names: Vec<String> = app.session.project.reports.iter().map(|r| r.name.clone()).collect();
    let current = names.get(i).cloned().unwrap_or_default();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("report_pick")
            .selected_text(current.as_str())
            .width(ui.available_width() - 70.0)
            .show_ui(ui, |ui| {
                for (j, n) in names.iter().enumerate() {
                    if ui.selectable_label(j == i, n.as_str()).clicked() {
                        let _ = app.run("report.activate", json!({"report": j}));
                        app.reports.page = 0;
                    }
                }
            })
            .response
            .on_hover_text(tl!("Report"));
        if ui.small_button(tl!("Delete")).clicked() {
            run(app, "report.remove", json!({"report": i}));
            app.reports.page = 0;
        }
    });
    let Some(r) = app.session.project.reports.get(i).cloned() else { return };
    // The same layer the engine reports on.
    let fields: Vec<String> = rustgis_engine::cmd::report::layer_of(&app.session, i)
        .map(|l| l.dataset.fields.iter().map(|f| f.name.clone()).collect())
        .unwrap_or_default();
    preview(app, ui, i, t);

    heading(ui, "Report");
    // Text being typed lives in the pane's state (rebuilt each frame it would lose keystrokes);
    // it's saved when the field loses focus and follows the report otherwise.
    if app.reports.editing != Some(i) {
        app.reports.editing = Some(i);
        app.reports.title = r.title.clone();
        app.reports.where_clause = r.where_clause.clone();
    }
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Title"));
        let resp =
            ui.add(egui::TextEdit::singleline(&mut app.reports.title).desired_width(ui.available_width())).labelled_by(input_label.id);
        if resp.lost_focus() {
            if app.reports.title != r.title {
                let title = app.reports.title.clone();
                run(app, "report.update", json!({"report": i, "title": title}));
            }
        } else if !resp.has_focus() {
            app.reports.title = r.title.clone();
        }
    });
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Where"));
        let resp = ui
            .add(
                egui::TextEdit::singleline(&mut app.reports.where_clause)
                    .hint_text("pop_max > 1000000")
                    .desired_width(ui.available_width()),
            )
            .labelled_by(input_label.id);
        if resp.lost_focus() {
            if app.reports.where_clause != r.where_clause {
                let w = app.reports.where_clause.clone();
                run(app, "report.update", json!({"report": i, "where": w}));
            }
        } else if !resp.has_focus() {
            app.reports.where_clause = r.where_clause.clone();
        }
    });
    let mut sel = r.selected_only;
    if ui.checkbox(&mut sel, tl!("Selected records only")).changed() {
        run(app, "report.update", json!({"report": i, "selectedOnly": sel}));
    }

    heading(ui, "Fields");
    ui.horizontal_wrapped(|ui| {
        for f in &fields {
            let mut on = r.fields.iter().any(|x| x == f);
            if ui.checkbox(&mut on, f.as_str()).changed() {
                run(app, "report.field", json!({"report": i, "field": f, "remove": !on}));
            }
        }
    });
    ui.horizontal(|ui| {
        if ui.small_button(tl!("Autofill Fields")).clicked() {
            run(app, "report.autofill", json!({"report": i}));
        }
        let sort = r.sort.clone().unwrap_or_default();
        egui::ComboBox::from_id_salt("report_sort")
            .selected_text(crate::i18n::fmt("Sort: {field}", &[("field", &sort)]))
            .show_ui(ui, |ui| {
                if ui.selectable_label(r.sort.is_none(), tl!("None")).clicked() {
                    run(app, "report.update", json!({"report": i, "sort": null}));
                }
                for f in &fields {
                    if ui.selectable_label(r.sort.as_deref() == Some(f), f.as_str()).clicked() {
                        run(app, "report.update", json!({"report": i, "sort": f}));
                    }
                }
            })
            .response
            .on_hover_text(tl!("Sort"));
        let mut desc = r.descending;
        if ui.checkbox(&mut desc, tl!("Descending")).changed() {
            run(app, "report.update", json!({"report": i, "descending": desc}));
        }
    });

    heading(ui, "Groups");
    for g in &r.groups {
        ui.horizontal(|ui| {
            ui.label(g.as_str());
            if ui.small_button(tl!("Remove")).clicked() {
                run(app, "report.group", json!({"report": i, "field": g, "remove": true}));
            }
        });
    }
    egui::ComboBox::from_id_salt("report_group")
        .selected_text(tl!("Add a group…"))
        .show_ui(ui, |ui| {
            for f in &fields {
                if ui.selectable_label(false, f.as_str()).clicked() {
                    run(app, "report.group", json!({"report": i, "field": f}));
                }
            }
        })
        .response
        .on_hover_text(tl!("Add a group…"));

    heading(ui, "Statistics");
    for st in &r.stats {
        ui.horizontal(|ui| {
            ui.label(format!("{} — {}", tl!(st.kind.label()), st.field));
            if ui.small_button(tl!("Remove")).clicked() {
                let kind = format!("{:?}", st.kind).to_ascii_lowercase();
                run(app, "report.statistics", json!({"report": i, "field": st.field, "kind": kind, "remove": true}));
            }
        });
    }
    ui.horizontal(|ui| {
        if app.reports.stat_kind.is_empty() {
            app.reports.stat_kind = "sum".into();
        }
        egui::ComboBox::from_id_salt("report_stat_kind")
            .selected_text(app.reports.stat_kind.as_str())
            .width(70.0)
            .show_ui(ui, |ui| {
                for k in ["count", "sum", "mean", "min", "max"] {
                    ui.selectable_value(&mut app.reports.stat_kind, k.to_string(), k);
                }
            })
            .response
            .on_hover_text(tl!("Statistic"));
        egui::ComboBox::from_id_salt("report_stat_field")
            .selected_text(app.reports.stat_field.as_str())
            .width(120.0)
            .show_ui(ui, |ui| {
                for f in &fields {
                    ui.selectable_value(&mut app.reports.stat_field, f.clone(), f.as_str());
                }
            })
            .response
            .on_hover_text(tl!("Field"));
        if ui.small_button(tl!("Add")).clicked() {
            let (field, kind) = (app.reports.stat_field.clone(), app.reports.stat_kind.clone());
            run(app, "report.statistics", json!({"report": i, "field": field, "kind": kind}));
        }
    });

    heading(ui, "Pictures, attachments and chart");
    for (label, id, cur) in [
        ("Dynamic picture", "report.dynamicPicture", r.picture_field.clone()),
        ("Attachments", "report.attachments", r.attachments_field.clone()),
        ("Chart categories", "report.chart", r.chart.as_ref().map(|c| c.category.clone())),
    ] {
        ui.horizontal(|ui| {
            let input_label = ui.label(tl!(label));
            let key = if id == "report.chart" { "category" } else { "field" };
            egui::ComboBox::from_id_salt(id)
                .selected_text(cur.clone().unwrap_or_else(|| tl!("None").to_string()))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_none(), tl!("None")).clicked() {
                        run(app, id, json!({"report": i, key: null}));
                    }
                    for f in &fields {
                        if ui.selectable_label(cur.as_deref() == Some(f), f.as_str()).clicked() {
                            run(app, id, json!({"report": i, key: f}));
                        }
                    }
                })
                .response
                .labelled_by(input_label.id);
        });
    }

    heading(ui, "Sections");
    for (k, sec) in r.sections.iter().enumerate() {
        let text = match sec {
            Section::Page { title, end, .. } => {
                crate::i18n::fmt(if *end { "Page after: {name}" } else { "Page before: {name}" }, &[("name", title)])
            }
            Section::Layout { layout, end } => {
                crate::i18n::fmt(if *end { "Layout after: {name}" } else { "Layout before: {name}" }, &[("name", layout)])
            }
            Section::Related { report } => crate::i18n::fmt("Related report: {name}", &[("name", report)]),
            Section::Subreport { relationship, .. } => crate::i18n::fmt("Subreport: {name}", &[("name", relationship)]),
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(text).small());
            if ui.small_button(tl!("Remove")).clicked() {
                run(app, "report.removeSection", json!({"report": i, "index": k}));
            }
        });
    }
    ui.add(egui::TextEdit::singleline(&mut app.reports.page_title).hint_text(tl!("Page title")).desired_width(ui.available_width()));
    ui.add(
        egui::TextEdit::multiline(&mut app.reports.page_text)
            .hint_text(tl!("Page text"))
            .desired_rows(2)
            .desired_width(ui.available_width()),
    );
    ui.horizontal_wrapped(|ui| {
        for (label, end) in [("Add Page Before", false), ("Add Page After", true)] {
            if ui.small_button(tl!(label)).clicked() {
                let (title, text) = (app.reports.page_title.clone(), app.reports.page_text.clone());
                let title = if title.trim().is_empty() { None } else { Some(title) };
                if run(app, "report.supplementalPage", json!({"report": i, "title": title, "text": text, "end": end})).is_some() {
                    app.reports.page_title.clear();
                    app.reports.page_text.clear();
                }
            }
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(tl!("Export Report…")).clicked() {
            let _ = app.run("ui.exportReport", Value::Null);
        }
        if ui.button(tl!("Save As…")).clicked() {
            let _ = app.run("ui.saveItem", json!({"kind": "report"}));
        }
    });
}

/// A small preview of the current page, with page buttons. It's redrawn when the report or the
/// page changes.
fn preview(app: &mut RustGisApp, ui: &mut Ui, i: usize, t: &Tokens) {
    // Any change to the report, its data or the selection runs a command, which moves the
    // revision on.
    let key = format!("{i}#{}#{}", app.reports.page, app.session.revision);
    if app.reports.preview.as_ref().is_none_or(|p| p.0 != key) {
        app.reports.preview = None;
        let built_for = (i, app.session.revision);
        if app.reports.built.as_ref().is_none_or(|b| b.0 != built_for) {
            let s = &app.session;
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::report::pages(s, i)))
                .unwrap_or_else(|_| Err("the preview failed unexpectedly".into()));
            // A failure is kept too, so it isn't retried every frame until something changes.
            app.reports.built = Some((built_for, built));
        }
        let (doc, pages) = match &app.reports.built {
            Some((_, Ok((doc, pages)))) => (doc, pages),
            Some((_, Err(e))) => {
                ui.label(egui::RichText::new(e.as_str()).small().color(Color32::from_rgb(0xC0, 0x30, 0x30)));
                return;
            }
            None => return,
        };
        let (s, page) = (&mut app.session, app.reports.page);
        let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_, String> {
            let page = page.min(pages.len().saturating_sub(1));
            let (pm, _, _) = rustgis_engine::render::report::draw_page(doc, pages, page, 60.0, &s.project.layouts, &mut s.project.maps)?;
            Ok((pm, page, pages.len()))
        }))
        .unwrap_or_else(|_| Err("the preview failed unexpectedly".into()));
        match drawn {
            Ok((pm, page, total)) => {
                app.reports.page = page;
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                let tex = ui.ctx().load_texture("report_preview", img, egui::TextureOptions::LINEAR);
                let key = format!("{i}#{page}#{}", app.session.revision);
                app.reports.preview = Some((key, tex, total));
            }
            Err(e) => {
                ui.label(egui::RichText::new(e).small().color(Color32::from_rgb(0xC0, 0x30, 0x30)));
                return;
            }
        }
    }
    let Some((_, tex, total)) = &app.reports.preview else { return };
    let (tex, total) = (tex.clone(), *total);
    let size = tex.size_vec2();
    let w = ui.available_width().min(300.0);
    let h = w * size.y / size.x.max(1.0);
    egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).show(ui, |ui| {
        ui.image((tex.id(), vec2(w, h)));
    });
    ui.horizontal(|ui| {
        if ui.add_enabled(app.reports.page > 0, egui::Button::new("◀")).clicked() {
            app.reports.page -= 1;
        }
        ui.label(crate::i18n::fmt("Page {n} of {total}", &[("n", &(app.reports.page + 1).to_string()), ("total", &total.to_string())]));
        if ui.add_enabled(app.reports.page + 1 < total, egui::Button::new("▶")).clicked() {
            app.reports.page += 1;
        }
    });
}
