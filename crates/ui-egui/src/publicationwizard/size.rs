//! The wizard's size step.
use super::*;

/// The size step: a sheet on any page size New Layout offers, turned as asked, or a figure.
pub(super) fn size_step(ui: &mut egui::Ui, d: &mut Draft) {
    egui::Grid::new("publication_wizard_size").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
        let label = ui.label(tl!("Size"));
        egui::ComboBox::from_id_salt("publication_wizard_size_pick")
            .selected_text(size_label(&d.size))
            .width(field_width(ui))
            .show_ui(ui, |ui| {
                for (id, ..) in PAGES {
                    ui.selectable_value(&mut d.size, (*id).to_string(), size_label(id));
                }
                ui.separator();
                for p in PUBLICATION_PROFILES.iter().filter(|p| !p.sheet) {
                    ui.selectable_value(&mut d.size, p.id.to_string(), size_label(p.id));
                }
            })
            .response
            .labelled_by(label.id);
        ui.end_row();
        ui.label(tl!("Orientation"));
        ui.add_enabled_ui(!d.figure(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut d.orientation, Orientation::Auto, tl!("Auto")).on_hover_text(tl!("The way the map comes out larger."));
                ui.radio_value(&mut d.orientation, Orientation::Portrait, tl!("Portrait"));
                ui.radio_value(&mut d.orientation, Orientation::Landscape, tl!("Landscape"));
            });
        });
        ui.end_row();
    });
    if d.figure() {
        figure_note(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;

    /// A figure is made from its size alone, through the same command, and Undo takes it away.
    #[test]
    fn a_figure_is_made_from_its_size_alone() {
        let mut app = app();
        open(&mut app);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.size = "figure-1col".into();
            d.subtitle = "Not drawn".into();
            d.step = Step::Review;
        }
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Create");
        let l = h.state().session.project.layouts.last().unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before + 1);
        assert!((l.width - 90.0).abs() < 0.01, "a 1-column figure: {} mm wide", l.width);
        assert_eq!(h.state().dialogs.open, None);
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
    }
}
