//! Session troubleshooting history. Capture lives below the UI and continues with this pane closed.

use egui::{RichText, Ui};
use rustgis_engine::data::diagnostics::{Filter, NetworkKind, Record, Severity};

use crate::{RustGisApp, theme::Tokens};

pub(crate) fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    // Background requests may finish without another UI action (including HEAD probes).
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    let generation = app.session.diagnostics.generation();
    let mut records = match app.ui.diagnostic_snapshot.take() {
        Some((at, records)) if at == generation => records,
        _ => app.session.diagnostics.records(),
    };
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.label(
            RichText::new(tl!("Messages and failed requests from this session. Oldest entries are discarded when the history is full."))
                .small()
                .color(t.text_dim),
        );
        ui.horizontal_wrapped(|ui| {
            // Literal labels, so each is found and translated with the interface's other strings.
            for (filter, label) in [
                (Filter::All, tl_raw!("All")),
                (Filter::Errors, tl_raw!("Errors")),
                (Filter::Warnings, tl_raw!("Warnings")),
                (Filter::Info, tl_raw!("Info")),
                (Filter::Network, tl_raw!("Network failures")),
            ] {
                let count = records.iter().filter(|r| filter.matches(r)).count();
                let text = crate::i18n::fmt("{label} ({count})", &[("label", label), ("count", &count.to_string())]);
                ui.selectable_value(&mut app.ui.diagnostic_filter, filter, text);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let any = records.iter().any(|r| app.ui.diagnostic_filter.matches(r));
            if ui
                .add_enabled(any, egui::Button::new(tl!("Copy diagnostics")))
                .on_hover_text(tl!("Copy the filtered history, including technical details"))
                .clicked()
            {
                ui.ctx().copy_text(app.session.diagnostics.report(app.ui.diagnostic_filter));
            }
            if ui
                .add_enabled(!records.is_empty(), egui::Button::new(tl!("Clear")))
                .on_hover_text(tl!("Clear all diagnostic history for this session"))
                .clicked()
            {
                app.session.diagnostics.clear();
                app.ui.diagnostic_filter = Filter::All;
                records.clear();
            }
        });
    });
    ui.separator();
    egui::ScrollArea::vertical().id_salt("diagnostic_log_entries").auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            let mut shown = false;
            for record in records.iter().rev().filter(|r| app.ui.diagnostic_filter.matches(r)) {
                shown = true;
                ui.push_id(record.id, |ui| entry(ui, record, t));
                ui.separator();
            }
            if !shown {
                ui.label(tl!("No diagnostic entries match this filter."));
            }
        });
    });
    app.ui.diagnostic_snapshot = Some((generation, records));
}

fn entry(ui: &mut Ui, record: &Record, t: &Tokens) {
    // Literal labels, so each is found and translated with the interface's other strings.
    let (severity, color) = match record.severity {
        Severity::Error => (tl!("Error"), t.red),
        Severity::Warning => (tl!("Warning"), t.amber),
        Severity::Info => (tl!("Info"), t.accent_text),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(time(record.timestamp_ms)).small().monospace().color(t.text_dim));
        ui.label(RichText::new(severity).small().strong().color(color));
        ui.label(RichText::new(record.source.as_str()).small().color(t.text_dim));
    });
    ui.add(egui::Label::new(&record.message).wrap().selectable(true));
    if !record.details.is_empty() || record.network.is_some() {
        egui::CollapsingHeader::new(tl!("Technical details")).show(ui, |ui| {
            if let Some(network) = &record.network {
                let kind = match network.kind {
                    NetworkKind::Http => tl!("HTTP failure"),
                    NetworkKind::Connection => tl!("Connection failure"),
                    NetworkKind::Timeout => tl!("Timeout"),
                    NetworkKind::Other => tl!("Request failure"),
                };
                ui.label(kind);
                ui.add(
                    egui::Label::new(RichText::new(format!("{} {}", network.method, network.url)).monospace().small())
                        .wrap()
                        .selectable(true),
                );
                if let Some(status) = network.status {
                    ui.label(crate::i18n::fmt("HTTP status: {status}", &[("status", &status.to_string())]));
                }
                if let Some(duration) = network.duration_ms {
                    ui.label(crate::i18n::fmt("Duration: {duration} ms", &[("duration", &duration.to_string())]));
                }
            }
            if !record.details.is_empty() {
                ui.add(egui::Label::new(RichText::new(&record.details).monospace().small()).wrap().selectable(true));
            }
        });
    }
}

/// The entry's UTC date and time: with the date, a session past UTC midnight reads in order.
fn time(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let (year, month, day) = civil_date(seconds / 86_400);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", seconds / 3600 % 24, seconds / 60 % 60, seconds % 60)
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(days: u64) -> (u64, u64, u64) {
    let z = days.saturating_add(719_468);
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + u64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    #[test]
    fn entry_times_carry_their_utc_date() {
        assert_eq!(super::time(0), "1970-01-01 00:00:00 UTC");
        // A leap day, and the instant a day ends.
        assert_eq!(super::time(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(super::time(951_868_799_999), "2000-02-29 23:59:59 UTC");
        assert_eq!(super::time(4_107_542_400_000), "2100-03-01 00:00:00 UTC");
    }
}
