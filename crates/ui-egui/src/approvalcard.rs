//! Shared presentation of frozen engine reviews; display data never grants approval.

use egui::{RichText, Ui};
use rustgis_engine::{Registry, policy::Review};

/// Registry labels name operations without interpreting the request's script or parameters.
fn operation(registry: &Registry, id: &str) -> String {
    registry.get(id).map_or_else(|| id.to_string(), |spec| tl_raw!(spec.label).to_string())
}

/// Shows consequences first and keeps the complete sanitized vehicle under Details.
/// Callers bound the scroll area and keep their existing approval controls outside it.
pub(crate) fn show(ui: &mut Ui, registry: &Registry, review: &Review) {
    ui.push_id((&review.what, &review.steps, &review.defaults, &review.lines), |ui| {
        let mut operations = Vec::new();
        for id in review.details.iter().map(|d| &d.operation).chain(review.lines.iter().map(|(_, id)| id)) {
            if !id.is_empty() && !operations.contains(id) {
                operations.push(id.clone());
            }
        }
        let actions = if operations.is_empty() {
            tl_raw!("Review this request").to_string()
        } else {
            operations.iter().map(|id| operation(registry, id)).collect::<Vec<_>>().join(", ")
        };
        let title = match review.edits {
            0 => crate::i18n::fmt("{actions}", &[("actions", &actions)]),
            1 => crate::i18n::fmt("{actions} · 1 project edit", &[("actions", &actions)]),
            count => crate::i18n::fmt("{actions} · {count} project edits", &[("actions", &actions), ("count", &count.to_string())]),
        };
        ui.add(egui::Label::new(RichText::new(title).strong()).wrap());
        // These are starting defaults, not an exhaustive target list for a composite request.
        if let Some((active, selected)) = &review.defaults {
            ui.add(
                egui::Label::new(crate::i18n::fmt(
                    "Starting active layer: {active}; selected layers: [{selected}]",
                    &[("active", &active.clone().unwrap_or_else(|| tl_raw!("(none)").to_string())), ("selected", &selected.join(", "))],
                ))
                .wrap(),
            );
        }
        ui.add(egui::Label::new(RichText::new(reason(review)).small()).wrap());
        for detail in &review.details {
            let name = operation(registry, &detail.operation);
            if !detail.targets.is_empty() {
                ui.add(
                    egui::Label::new(crate::i18n::fmt(
                        "Target details ({operation}): {targets}",
                        &[("operation", &name), ("targets", &detail.targets.join(", "))],
                    ))
                    .wrap(),
                );
            }
            if !detail.places.is_empty() {
                ui.add(
                    egui::Label::new(crate::i18n::fmt(
                        "Destinations ({operation}): {places}",
                        &[("operation", &name), ("places", &detail.places.join(", "))],
                    ))
                    .wrap(),
                );
            }
        }
        egui::CollapsingHeader::new(tl!("Details")).default_open(false).show(ui, |ui| {
            ui.label(tl!("Exact request"));
            ui.add(egui::Label::new(RichText::new(&review.what).monospace().small()).wrap().selectable(true));
            // Direct commands repeat their vehicle as an asking step; show it only once.
            let steps = review.steps.iter().filter(|step| *step != &review.what).collect::<Vec<_>>();
            if !steps.is_empty() {
                ui.label(tl!("Steps needing access"));
                for step in steps {
                    ui.add(egui::Label::new(RichText::new(step).monospace().small()).wrap().selectable(true));
                }
            }
        });
    });
}

/// Translates the engine's reason and script locations without parsing the displayed vehicle.
fn reason(review: &Review) -> String {
    let fields = review.explanation.1.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>();
    let reason = crate::i18n::fmt(review.explanation.0, &fields);
    if review.lines.is_empty() {
        return reason;
    }
    let steps = review
        .lines
        .iter()
        .map(|(line, command)| crate::i18n::fmt("line {line} ({command})", &[("line", &line.to_string()), ("command", command)]))
        .collect::<Vec<_>>()
        .join("; ");
    crate::i18n::fmt("{steps}: {reason}", &[("steps", &steps), ("reason", &reason)])
}
