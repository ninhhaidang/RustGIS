//! The wizard's accent step.
use super::*;

/// The accent step: the style's own colour, or one picked.
pub(super) fn accent_step(ui: &mut egui::Ui, d: &mut Draft, app_accent: egui::Color32) {
    let mut ticked = d.accent.is_some();
    if ui
        .checkbox(&mut ticked, tl!("Pick a colour of your own"))
        .on_hover_text(tl!(
            "Starts from the colour picked last, or the style's own accent colour; the swatch shows the colour the layout takes."
        ))
        .changed()
    {
        // Unticked, keep a custom colour for the next tick; leave an untouched style default
        // unremembered so switching styles starts from the new style's accent.
        let default = style_accent(&d.style, app_accent);
        if !ticked && d.picked.is_none() {
            d.picked = d.accent.filter(|&c| c != default);
        }
        d.accent = ticked.then(|| d.picked.unwrap_or(default));
    }
    if let Some(c) = &mut d.accent {
        if egui::color_picker::color_edit_button_srgba(ui, c, egui::color_picker::Alpha::Opaque).changed() {
            d.picked = Some(*c);
        }
    } else {
        ui.label(egui::RichText::new(tl!("The style's own colour.")).small().weak());
    }
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// A colour picked stays picked: unticked, the style's own colour is sent; ticked again, the
    /// swatch comes back with the colour picked before.
    #[test]
    fn a_colour_picked_comes_back_when_ticked_again() {
        let mut app = app();
        open(&mut app);
        let red = egui::Color32::from_rgb(0xC0, 0x30, 0x30);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.step = Step::Accent;
            d.accent = Some(red);
        }
        let mut h = harness(app);
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, None, "the style's own");
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(red), "the colour picked before");
    }

    /// Ticking "Pick a colour of your own" starts from the chosen style's own accent from the
    /// engine, not the app's accent; unticking without picking a custom colour lets a newly
    /// chosen style start from its own accent too.
    #[test]
    fn accent_starts_from_the_chosen_styles_own_colour() {
        let mut app = app();
        open(&mut app);
        let banner = publication_style_accent("banner").unwrap();
        let banner = egui::Color32::from_rgb(banner.r(), banner.g(), banner.b());
        let poster = publication_style_accent("poster").unwrap();
        let poster = egui::Color32::from_rgb(poster.r(), poster.g(), poster.b());
        assert_ne!(banner, poster);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.style = "banner".into();
            d.step = Step::Accent;
        }
        let mut h = harness(app);
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(banner), "starts from Banner's accent");
        // Unticking without editing leaves no custom colour remembered, so switching to Poster
        // starts from Poster's own accent.
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, None);
        h.state_mut().dialogs.publication.as_mut().unwrap().style = "poster".into();
        h.run_steps(2);
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(poster), "starts from Poster's accent");
    }

    /// Switching styles while "Pick a colour of your own" is ticked re-seeds an untouched style
    /// default to the new style's accent (so unticking afterward never remembers the old style's
    /// colour), while preserving a custom colour the user picked.
    #[test]
    fn switching_style_while_accent_is_ticked_re_seeds_untouched_default() {
        let mut app = app();
        open(&mut app);
        let accent_of = |id: &str| {
            let c = publication_style_accent(id).unwrap();
            egui::Color32::from_rgb(c.r(), c.g(), c.b())
        };
        let (banner, poster, editorial) = (accent_of("banner"), accent_of("poster"), accent_of("editorial"));
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.style = "banner".into();
            d.step = Step::Accent;
        }
        let mut h = harness(app);
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(banner));
        // Go back to Style while still ticked and switch to Poster: accent follows Poster.
        click(&mut h, "Back");
        click(&mut h, "Back");
        h.get_by_label("Poster").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(poster), "re-seeded to Poster");
        // Unticking on Poster must not remember Banner's or Poster's default as a custom colour.
        click(&mut h, "Next");
        click(&mut h, "Next");
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, None);
        click(&mut h, "Back");
        click(&mut h, "Back");
        h.get_by_label("Editorial").click();
        h.run_steps(2);
        click(&mut h, "Next");
        click(&mut h, "Next");
        h.get_by_label("Pick a colour of your own").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(editorial), "starts from Editorial");
        // A custom colour is kept when switching styles.
        let red = egui::Color32::from_rgb(0xC0, 0x30, 0x30);
        h.state_mut().dialogs.publication.as_mut().unwrap().accent = Some(red);
        click(&mut h, "Back");
        click(&mut h, "Back");
        h.get_by_label("Banner").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().accent, Some(red), "custom colour kept");
    }
}
