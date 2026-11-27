//! The wizard's style step.
use super::*;
use rustgis_engine::cmd::layout::PUBLICATION_STYLES;

/// The style step: each sheet style, by the name and description the menu gives it.
pub(super) fn style_step(ui: &mut egui::Ui, d: &mut Draft, app_accent: egui::Color32) {
    let before = d.style.clone();
    for (id, english) in PUBLICATION_STYLES {
        let (name, about) = crate::ribbon::publication_style(id).unwrap_or_else(|| (tl!(*english), ""));
        ui.radio_value(&mut d.style, (*id).to_string(), name);
        ui.indent(*id, |ui| ui.label(egui::RichText::new(about).small().weak()));
    }
    // If the swatch was only seeded from the previous style's default (never customised), follow
    // the newly chosen style's own accent instead of carrying the old style's colour over.
    if d.style != before && d.picked.is_none() && d.accent == Some(style_accent(&before, app_accent)) {
        d.accent = Some(style_accent(&d.style, app_accent));
    }
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui::vec2;
    use egui_kittest::kittest::Queryable;

    /// The style step lists every sheet style, by the names and descriptions the menu gives them.
    #[test]
    fn the_style_step_lists_every_style_as_the_menu_names_it() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Style;
        let h = harness(app);
        for (id, _) in rustgis_engine::cmd::layout::PUBLICATION_STYLES {
            let (name, about) = crate::ribbon::publication_style(id).unwrap();
            assert!(h.query_by_label(name).is_some(), "{id}");
            assert!(h.query_by_label(about).is_some(), "{id}");
        }
    }

    /// In English the style step fits its box: the wheel over it scrolls nothing, so the last
    /// style's description isn't cut at the bottom.
    #[test]
    fn the_style_step_fits_without_scrolling_in_english() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Style;
        let mut h = harness(app);
        let before = h.get_by_label("Classic").rect();
        h.event(egui::Event::PointerMoved(before.center()));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -400.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        });
        h.run_steps(10);
        assert_eq!(h.get_by_label("Classic").rect(), before, "the styles scroll: they don't fit the box");
    }
}
