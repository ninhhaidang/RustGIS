//! Publication Layout Wizard: `layout.publication` asked step by step (size, style, texts, accent),
//! then made at once, as the New Layout › Publication Layout menu makes it (#2850).
use crate::{RustGisApp, i18n, theme::Tokens};
use rustgis_engine::Session;
use rustgis_engine::cmd::layout::{
    MAX_PUBLICATION_TEXT, PUBLICATION_DEFAULT_CREDIT, PUBLICATION_PROFILES, PUBLICATION_STYLES, publication_style_accent,
};
use rustgis_engine::map::layout::PAGES;
use serde_json::{Value, json};

/// The wizard's steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Size,
    Style,
    Texts,
    Accent,
    Review,
}

/// How a sheet is turned: `Auto` leaves it to `layout.publication` (the way the map comes out larger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Auto,
    Portrait,
    Landscape,
}

/// What the wizard has asked so far: `layout.publication`'s fields, under the command's names.
pub struct Draft {
    pub step: Step,
    /// A page size's id (`a4`, from [`PAGES`]) or a figure's profile id (`figure-1col`).
    pub size: String,
    pub orientation: Orientation,
    pub style: String,
    pub title: String,
    /// Whether the title was typed; until then it is the active map's name, as the menu's.
    pub title_typed: bool,
    pub subtitle: String,
    pub note: String,
    pub sources: String,
    pub author: String,
    /// No credit line at all (`"author": ""`), rather than the command's default one.
    pub no_author: bool,
    /// The style's colour, or `None` for the style's own.
    pub accent: Option<egui::Color32>,
    /// The colour last picked, which the swatch comes back with when the box is ticked again.
    picked: Option<egui::Color32>,
}

impl Draft {
    /// Where the wizard starts: an A4 sheet turned Auto (as `layout.publication` turns a size
    /// given alone, the way the map comes out larger; the menu lists Portrait and Landscape
    /// instead), the classic style and the map's name as the title, as the menu titles a sheet.
    pub fn new(s: &Session) -> Self {
        Self {
            step: Step::Size,
            size: "a4".into(),
            orientation: Orientation::Auto,
            style: "classic".into(),
            title: s.map().name.clone(),
            title_typed: false,
            subtitle: String::new(),
            note: String::new(),
            sources: String::new(),
            author: String::new(),
            no_author: false,
            accent: None,
            picked: None,
        }
    }

    /// Whether the size is a figure, which takes no style, texts or accent: the command draws the
    /// map edge to edge and leaves the rest to the caption.
    fn figure(&self) -> bool {
        PUBLICATION_PROFILES.iter().any(|p| !p.sheet && p.id == self.size)
    }

    /// The steps this draft goes through, in order.
    fn steps(&self) -> &'static [Step] {
        if self.figure() { &[Step::Size, Step::Review] } else { &[Step::Size, Step::Style, Step::Texts, Step::Accent, Step::Review] }
    }

    /// The step `by` places from this one, if there is one.
    fn moved(&self, by: isize) -> Option<Step> {
        let steps = self.steps();
        let at = steps.iter().position(|s| *s == self.step)?;
        steps.get(at.checked_add_signed(by)?).copied()
    }

    /// `layout.publication`'s parameters: the fields set; one left empty is left to the command's
    /// default, but for the title, which empty means none.
    pub fn params(&self) -> Value {
        let profile = match (self.figure(), self.orientation) {
            (true, _) | (false, Orientation::Auto) => self.size.clone(),
            (false, Orientation::Portrait) => format!("{}-portrait", self.size),
            (false, Orientation::Landscape) => format!("{}-landscape", self.size),
        };
        let mut p = json!({"profile": profile});
        if self.figure() {
            return p;
        }
        p["style"] = json!(self.style);
        p["title"] = json!(self.title.trim());
        for (key, text) in [("subtitle", &self.subtitle), ("note", &self.note), ("sources", &self.sources), ("author", &self.author)] {
            if !text.trim().is_empty() {
                p[key] = json!(text.trim());
            }
        }
        if self.no_author {
            p["author"] = json!("");
        }
        if let Some(c) = self.accent {
            p["accent"] = json!(hex(c));
        }
        p
    }
}

/// A colour as `layout.publication` takes it: `#RRGGBB`.
fn hex(c: egui::Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// A page size's or a figure's name, as New Layout › Publication Layout lists it.
fn size_label(id: &str) -> String {
    if let Some((_, label, w, h)) = PAGES.iter().find(|p| p.0 == id) {
        return crate::ribbon::page_label(label, *w, *h);
    }
    PUBLICATION_PROFILES.iter().find(|p| p.id == id).map_or_else(|| id.to_string(), |p| crate::ribbon::profile_label(p).to_string())
}

/// A step's name, as its heading gives it: in logical order (`tl_raw!`), as `i18n::fmt` takes
/// its arguments, converting the whole heading for display once.
fn step_name(step: Step) -> &'static str {
    match step {
        Step::Size => tl_raw!("Size"),
        Step::Style => tl_raw!("Style"),
        Step::Texts => tl_raw!("Texts"),
        Step::Accent => tl_raw!("Accent"),
        Step::Review => tl_raw!("Review"),
    }
}

/// The size step: a sheet on any page size New Layout offers, turned as asked, or a figure.
fn size_step(ui: &mut egui::Ui, d: &mut Draft) {
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

/// What a figure leaves out, said where its size is picked and in the review: the texts and
/// accent typed for a sheet aren't drawn on it.
fn figure_note(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(tl!("A figure is the map edge to edge: its title, texts and style go in its caption.")).small().weak());
}

/// The style step: each sheet style, by the name and description the menu gives it.
fn style_step(ui: &mut egui::Ui, d: &mut Draft) {
    for (id, english) in PUBLICATION_STYLES {
        let (name, about) = crate::ribbon::publication_style(id).unwrap_or_else(|| (tl!(*english), ""));
        ui.radio_value(&mut d.style, (*id).to_string(), name);
        ui.indent(*id, |ui| ui.label(egui::RichText::new(about).small().weak()));
    }
}

/// The texts step: the title, subtitle, note, sources and credit line.
fn texts_step(ui: &mut egui::Ui, d: &mut Draft) {
    egui::Grid::new("publication_wizard_texts").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
        let line = |ui: &mut egui::Ui, label: &str, text: &mut String| {
            let caption = ui.label(label);
            ui.add(egui::TextEdit::singleline(text).char_limit(MAX_PUBLICATION_TEXT).desired_width(field_width(ui)))
                .labelled_by(caption.id);
            ui.end_row();
        };
        let label = ui.label(tl!("Title"));
        let title = ui
            .add(
                egui::TextEdit::singleline(&mut d.title)
                    .hint_text(tl!("No title"))
                    .char_limit(MAX_PUBLICATION_TEXT)
                    .desired_width(field_width(ui)),
            )
            .labelled_by(label.id);
        d.title_typed |= title.changed();
        ui.end_row();
        line(ui, tl!("Subtitle"), &mut d.subtitle);
        let label = ui.label(tl!("Note"));
        ui.add(egui::TextEdit::multiline(&mut d.note).desired_rows(3).char_limit(MAX_PUBLICATION_TEXT).desired_width(field_width(ui)))
            .labelled_by(label.id);
        ui.end_row();
        line(ui, tl!("Sources"), &mut d.sources);
        let label = ui.label(tl!("Credit line"));
        ui.add_enabled(
            !d.no_author,
            egui::TextEdit::singleline(&mut d.author)
                .hint_text(default_credit())
                .char_limit(MAX_PUBLICATION_TEXT)
                .desired_width(field_width(ui)),
        )
        .labelled_by(label.id);
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut d.no_author, tl!("No credit line"));
        ui.end_row();
    });
    // The boxes take no more, and cut a longer paste to fit: said, not done unseen.
    let at_limit = [&d.title, &d.subtitle, &d.note, &d.sources].iter().any(|t| t.chars().count() >= MAX_PUBLICATION_TEXT)
        || (!d.no_author && d.author.chars().count() >= MAX_PUBLICATION_TEXT);
    if at_limit {
        ui.label(
            egui::RichText::new(i18n::fmt("Each field takes at most {limit} characters.", &[("limit", &MAX_PUBLICATION_TEXT.to_string())]))
                .small()
                .weak(),
        );
    }
}

/// A style's default accent as an egui colour, falling back to `fallback` if the style is unknown.
fn style_accent(style: &str, fallback: egui::Color32) -> egui::Color32 {
    publication_style_accent(style).map_or(fallback, |c| egui::Color32::from_rgb(c.r(), c.g(), c.b()))
}

/// The accent step: the style's own colour, or one picked.
fn accent_step(ui: &mut egui::Ui, d: &mut Draft, app_accent: egui::Color32) {
    let mut ticked = d.accent.is_some();
    if ui
        .checkbox(&mut ticked, tl!("Pick a colour of your own"))
        .on_hover_text(tl!(
            "Starts from the colour picked last, or the style's own accent colour; the swatch shows the colour the layout takes."
        ))
        .changed()
    {
        // Unticked, keep a custom colour for the next tick; leave the style's own unremembered so
        // switching styles starts from the new style's accent.
        let default = style_accent(&d.style, app_accent);
        if !ticked {
            d.picked = d.accent.filter(|&c| c != default);
        }
        d.accent = ticked.then(|| d.picked.unwrap_or(default));
    }
    if let Some(c) = &mut d.accent {
        egui::color_picker::color_edit_button_srgba(ui, c, egui::color_picker::Alpha::Opaque);
    } else {
        ui.label(egui::RichText::new(tl!("The style's own colour.")).small().weak());
    }
}

/// The review step: what was picked, step by step.
fn review_step(ui: &mut egui::Ui, d: &Draft) {
    let none = tl!("(none)");
    let or_none = |t: &str| if t.trim().is_empty() { none.to_string() } else { t.trim().to_string() };
    egui::Grid::new("publication_wizard_review").num_columns(2).spacing(egui::vec2(12.0, 6.0)).show(ui, |ui| {
        let mut row = |key: &str, value: String| {
            ui.label(egui::RichText::new(key).weak());
            ui.label(value);
            ui.end_row();
        };
        row(tl!("Size"), size_label(&d.size));
        if d.figure() {
            return;
        }
        let turned = match d.orientation {
            Orientation::Auto => tl!("Auto: the way the map comes out larger"),
            Orientation::Portrait => tl!("Portrait"),
            Orientation::Landscape => tl!("Landscape"),
        };
        row(tl!("Orientation"), turned.to_string());
        let style = crate::ribbon::publication_style(&d.style).map(|s| s.0.to_string()).unwrap_or_else(|| d.style.clone());
        row(tl!("Style"), style);
        row(tl!("Title"), or_none(&d.title));
        row(tl!("Subtitle"), or_none(&d.subtitle));
        row(tl!("Note"), or_none(&d.note));
        row(tl!("Sources"), or_none(&d.sources));
        let credit = if d.no_author {
            none.to_string()
        } else if d.author.trim().is_empty() {
            default_credit()
        } else {
            d.author.trim().to_string()
        };
        row(tl!("Credit line"), credit);
        let accent = d.accent.map(hex).unwrap_or_else(|| tl!("The style's own").to_string());
        row(tl!("Accent"), accent);
    });
    if d.figure() {
        figure_note(ui);
    }
}

/// How the wizard names the credit line `layout.publication` writes when none is given,
/// built from the engine's [`PUBLICATION_DEFAULT_CREDIT`] with `{date}` named for display.
fn default_credit() -> String {
    i18n::fmt(PUBLICATION_DEFAULT_CREDIT, &[("date", tl_raw!("today's date"))])
}

/// A field's width beside its label: 260 points, or what the row leaves once a longer label
/// (Greek "Orientation", in a wider interface font) takes its share, so the row never widens the
/// window.
fn field_width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(260.0)
}

/// The wizard's width, where the screen has room for it.
const WIDTH: f32 = 380.0;

/// The height every step takes: the style step's six styles and their descriptions, with room
/// for the rounding a high-density screen adds to each line.
const STEP_HEIGHT: f32 = 320.0;

/// Shows the wizard while it is open; Create runs `layout.publication` as the menu does, and the
/// wizard stays open on a layout the command refuses.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut d) = app.dialogs.publication.take() else {
        app.dialogs.open = None;
        return;
    };
    // A map opened meanwhile titles its sheet, as the menu titles it at the click.
    if !d.title_typed {
        d.title.clone_from(&app.session.map().name);
    }
    let t = Tokens::get(ctx);
    let (mut open, mut close, mut create) = (true, false, false);
    // Narrower on a screen too narrow for it, as the other dialogs are.
    let frame = egui::Frame::window(&ctx.global_style());
    let width = crate::widgets::dialog_width(ctx, WIDTH, frame);
    egui::Window::new(tl!("Publication Layout Wizard"))
        .id(egui::Id::unique("publication_wizard"))
        .frame(frame)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .auto_sized()
        // One width on every step and in every language: a long row wraps instead of widening it.
        .max_width(width)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.set_width(width);
            let steps = d.steps();
            let at = steps.iter().position(|s| *s == d.step).unwrap_or(0);
            let heading = i18n::fmt(
                "Step {n} of {count}: {step}",
                &[("n", &(at + 1).to_string()), ("count", &steps.len().to_string()), ("step", step_name(d.step))],
            );
            ui.label(egui::RichText::new(heading).strong());
            ui.separator();
            // Every step in a box of one size, so the window keeps its size and Next stays under
            // the pointer from step to step: as tall as the tallest step (the styles and their
            // descriptions) in English, a step that runs longer in another language scrolling
            // inside it, its text wrapped to its width rather than widening the window. Each step
            // scrolls on its own, so one scrolled down doesn't open the next partway down.
            egui::ScrollArea::vertical()
                .id_salt(("publication_wizard_step", d.step))
                .max_height(STEP_HEIGHT)
                .auto_shrink([false, false])
                .show(ui, |ui| match d.step {
                    Step::Size => size_step(ui, &mut d),
                    Step::Style => style_step(ui, &mut d),
                    Step::Texts => texts_step(ui, &mut d),
                    Step::Accent => accent_step(ui, &mut d, t.accent),
                    Step::Review => review_step(ui, &d),
                });
            ui.separator();
            let size = egui::vec2(84.0, 28.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    match d.moved(1) {
                        Some(next) => {
                            if crate::widgets::dialog_button(ui, tl!("Next"), Some(t.accent), size).clicked() {
                                d.step = next;
                            }
                        }
                        None => create = crate::widgets::dialog_button(ui, tl!("Create"), Some(t.accent), size).clicked(),
                    }
                    let back = d.moved(-1);
                    if ui.add_enabled_ui(back.is_some(), |ui| crate::widgets::dialog_button(ui, tl!("Back"), None, size).clicked()).inner
                        && let Some(back) = back
                    {
                        d.step = back;
                    }
                    close = crate::widgets::dialog_button(ui, tl!("Cancel"), None, size).clicked();
                });
            });
        });
    // A layout the command refuses is said (a map with no extent), and the wizard stays.
    if create && crate::ribbon::make_publication(app, d.params()) {
        close = true;
    }
    if !open || close {
        app.dialogs.open = None;
    } else {
        app.dialogs.publication = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::kittest::{NodeT, Queryable};

    fn app() -> RustGisApp {
        RustGisApp::with_sample()
    }

    fn open(app: &mut RustGisApp) {
        app.run("ui.dialog", json!({"name": "publicationWizard"})).unwrap();
    }

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        harness_sized(app, vec2(1400.0, 1000.0))
    }

    fn harness_sized(app: RustGisApp, size: egui::Vec2) -> egui_kittest::Harness<'static, RustGisApp> {
        // The dialogs alone: the ribbon has a Next of its own (the next extent).
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(2.0).with_max_steps(64).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    // The app's fonts and theme at a high-density screen's scale, so sizes, spacing
                    // and the rounding of each line are the ones the app draws there.
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    let theme = app.current_theme();
                    crate::theme::apply(ui.ctx(), &theme.tokens, &theme.metrics);
                    fonts = true;
                    return;
                }
                crate::dialogs::show(app, &ui.ctx().clone());
            },
            app,
        );
        h.run_steps(4);
        h
    }

    fn click(h: &mut egui_kittest::Harness<'static, RustGisApp>, label: &str) {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run_steps(4);
    }

    fn step(h: &egui_kittest::Harness<'static, RustGisApp>) -> Step {
        h.state().dialogs.publication.as_ref().expect("the wizard is open").step
    }

    /// It opens on its first step: an A4 sheet turned Auto, the classic style, the map's name as
    /// the title and nothing else set; opened again while open, it keeps what was entered.
    #[test]
    fn opens_on_its_defaults_and_keeps_its_draft() {
        let mut app = app();
        open(&mut app);
        let name = app.session.map().name.clone();
        let d = app.dialogs.publication.as_mut().expect("opened");
        assert_eq!((d.step, d.size.as_str(), d.orientation, d.style.as_str()), (Step::Size, "a4", Orientation::Auto, "classic"));
        assert_eq!(d.title, name);
        assert!(d.subtitle.is_empty() && d.note.is_empty() && d.sources.is_empty() && d.author.is_empty());
        assert!(d.accent.is_none() && !d.no_author);
        assert_eq!(app.dialogs.open, Some("publicationWizard"));
        app.dialogs.publication.as_mut().unwrap().subtitle = "Kept".into();
        open(&mut app);
        assert_eq!(app.dialogs.publication.as_ref().unwrap().subtitle, "Kept", "not started over");
    }

    /// What Create runs: the fields set, under the command's names; what is left empty is left to
    /// the command's default, but for the title, which empty means none.
    #[test]
    fn create_sends_the_fields_set_and_nothing_else() {
        let app = app();
        let mut d = Draft::new(&app.session);
        d.title = "Rivers".into();
        assert_eq!(d.params(), json!({"profile": "a4", "style": "classic", "title": "Rivers"}));
        d.orientation = Orientation::Landscape;
        d.size = "letter".into();
        d.style = "panel".into();
        d.title.clear();
        d.subtitle = "2026".into();
        d.note = "A note".into();
        d.sources = "Natural Earth".into();
        d.author = "J. G.".into();
        d.accent = Some(egui::Color32::from_rgb(0x0B, 0x1B, 0x2B));
        assert_eq!(
            d.params(),
            json!({"profile": "letter-landscape", "style": "panel", "title": "", "subtitle": "2026", "note": "A note",
                "sources": "Natural Earth", "author": "J. G.", "accent": "#0B1B2B"})
        );
        d.orientation = Orientation::Portrait;
        d.no_author = true;
        assert_eq!(d.params()["profile"], "letter-portrait");
        assert_eq!(d.params()["author"], "", "no credit line");
        // A figure takes no style, texts or accent: the engine draws none of them on it.
        d.size = "figure-1col".into();
        assert_eq!(d.params(), json!({"profile": "figure-1col"}));
    }

    /// A sheet goes through the five steps; a figure goes from its size to the review, the steps
    /// it takes nothing from skipped both ways. Back is off on the first step; Next becomes
    /// Create on the last.
    #[test]
    fn a_sheet_takes_every_step_and_a_figure_skips_what_it_doesnt_draw() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        assert!(h.get_by_role_and_label(Role::Button, "Back").accesskit_node().is_disabled(), "Back is off");
        for want in [Step::Style, Step::Texts, Step::Accent, Step::Review] {
            click(&mut h, "Next");
            assert_eq!(step(&h), want);
        }
        assert!(h.query_by_role_and_label(Role::Button, "Next").is_none());
        assert!(h.query_by_role_and_label(Role::Button, "Create").is_some());
        for want in [Step::Accent, Step::Texts, Step::Style, Step::Size] {
            click(&mut h, "Back");
            assert_eq!(step(&h), want);
        }
        h.state_mut().dialogs.publication.as_mut().unwrap().size = "figure-2col".into();
        h.run_steps(2);
        click(&mut h, "Next");
        assert_eq!(step(&h), Step::Review);
        click(&mut h, "Back");
        assert_eq!(step(&h), Step::Size);
    }

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

    /// Create makes the layout in one step, closes the wizard and opens the Publish tab, as the
    /// menu does; Undo takes the layout away.
    #[test]
    fn create_makes_the_layout_and_undo_takes_it_away() {
        let mut app = app();
        open(&mut app);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.style = "banner".into();
            d.subtitle = "From the wizard".into();
            d.step = Step::Review;
        }
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Create");
        let app = h.state();
        assert_eq!(app.session.project.layouts.len(), before + 1);
        let l = app.session.project.layouts.last().unwrap();
        assert!(
            l.elements.iter().any(
                |e| matches!(&e.kind, rustgis_engine::map::layout::ElementKind::Text { text, .. } if text.contains("From the wizard"))
            ),
            "the subtitle given"
        );
        assert_eq!((app.dialogs.open, app.ui.tab.as_str()), (None, "Publish"));
        assert!(app.dialogs.publication.is_none());
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
    }

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

    /// A layout the command refuses (here a style it doesn't know) is said, and the wizard stays
    /// open on its review with nothing made.
    #[test]
    fn a_refused_layout_is_said_and_the_wizard_stays_open() {
        let mut app = app();
        open(&mut app);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.style = "unknown".into();
            d.step = Step::Review;
        }
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        let said = h.state().session.notices.len();
        click(&mut h, "Create");
        let app = h.state();
        assert_eq!(app.session.project.layouts.len(), before, "nothing made");
        assert!(app.session.notices[said..].iter().any(|n| n.text.contains("unknown")), "said");
        assert_eq!(app.dialogs.open, Some("publicationWizard"));
        assert_eq!(app.dialogs.publication.as_ref().map(|d| d.step), Some(Step::Review));
    }

    /// Next stays where it was from one step to the next: every step is as tall as the tallest.
    #[test]
    fn next_stays_under_the_pointer_from_step_to_step() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        let at = |h: &egui_kittest::Harness<'static, RustGisApp>| h.get_by_role_and_label(Role::Button, "Next").rect();
        let first = at(&h);
        for _ in 0..3 {
            click(&mut h, "Next");
            assert_eq!(at(&h), first, "{:?}", step(&h));
        }
    }

    /// Until the title is edited, it is the active map's name, as the menu titles a sheet: a map
    /// opened while the wizard is open titles its sheet; a title typed stays.
    #[test]
    fn the_title_follows_the_active_map_until_it_is_edited() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        h.state_mut().run("map.new", json!({"name": "Rivers"})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().title, "Rivers");
        click(&mut h, "Next");
        click(&mut h, "Next");
        h.get_by_label("Title").focus();
        h.run_steps(1);
        h.get_by_label("Title").type_text(" basins");
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().title, "Rivers basins");
        h.state_mut().run("map.new", json!({"name": "Lakes"})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().title, "Rivers basins", "typed, so kept");
    }

    /// The review says what will be drawn: the default credit line, dated when the layout is made, and for
    /// a figure, that its texts and accent are left to the caption.
    #[test]
    fn the_review_says_what_will_be_drawn() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Review;
        let mut h = harness(app);
        assert!(h.query_by_label("Made with RustGIS · today's date").is_some());
        assert!(h.query_by_label("Auto: the way the map comes out larger").is_some(), "the turn, decided at Create");
        h.state_mut().dialogs.publication.as_mut().unwrap().size = "figure-1col".into();
        h.run_steps(2);
        assert!(h.query_by_label_contains("its caption").is_some(), "a figure's texts aren't drawn");
    }

    /// Next stays where it was in a language whose descriptions run longer: a step taller than
    /// the others scrolls inside its height rather than growing the window.
    #[test]
    fn next_stays_in_place_in_a_longer_language() {
        let previous = crate::i18n::current();
        for code in ["de", "el"] {
            let mut app = app();
            // As the app applies its language each frame (`chosen_lang`), which the harness skips.
            crate::i18n::set_current(crate::i18n::Lang::from_code(code).unwrap());
            open(&mut app);
            let mut h = harness(app);
            let next = crate::i18n::t("Next").to_string();
            assert_ne!(next, "Next", "{code}: the language changed");
            let at = |h: &egui_kittest::Harness<'static, RustGisApp>| h.get_by_role_and_label(Role::Button, &next).rect();
            let first = at(&h);
            for _ in 0..3 {
                h.get_by_role_and_label(Role::Button, &next).click();
                h.run_steps(4);
                assert_eq!(at(&h), first, "{code}: {:?}", step(&h));
            }
        }
        crate::i18n::set_current(previous);
    }

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

    /// The text fields enforce the engine's character limit (`MAX_PUBLICATION_TEXT`) and say so
    /// when a field reaches it, saying nothing on shorter text.
    #[test]
    fn texts_enforce_the_engines_character_limit_and_say_so() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Texts;
        let mut h = harness(app);
        let limit_text = format!("Each field takes at most {MAX_PUBLICATION_TEXT} characters.");
        assert!(h.query_by_label(&limit_text).is_none(), "clean text shows no limit notice");
        // Fill Subtitle to two characters below the limit, then type five more: stopped at the limit.
        h.state_mut().dialogs.publication.as_mut().unwrap().subtitle = "x".repeat(MAX_PUBLICATION_TEXT - 2);
        h.run_steps(2);
        h.get_by_label("Subtitle").focus();
        h.run_steps(1);
        h.get_by_label("Subtitle").type_text("abcde");
        h.run_steps(2);
        let sub = &h.state().dialogs.publication.as_ref().unwrap().subtitle;
        assert_eq!(sub.chars().count(), MAX_PUBLICATION_TEXT, "capped at MAX_PUBLICATION_TEXT");
        assert!(sub.ends_with("xxab"), "kept up to the limit");
        assert!(h.query_by_label(&limit_text).is_some(), "limit notice shown at the cap");
    }

    /// The review's default credit line comes from the engine's `PUBLICATION_DEFAULT_CREDIT`.
    #[test]
    fn default_credit_matches_the_engines_constant() {
        assert_eq!(default_credit(), PUBLICATION_DEFAULT_CREDIT.replace("{date}", "today's date"));
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

    /// On a screen narrower than the wizard, it narrows to fit, as the other dialogs do: Next and
    /// the step's text stay on the screen.
    #[test]
    fn a_narrow_screen_keeps_the_wizard_on_it() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Style;
        let h = harness_sized(app, vec2(320.0, 900.0));
        let next = h.get_by_role_and_label(Role::Button, "Next").rect();
        assert!(next.left() >= 0.0 && next.right() <= 320.0, "Next at {next:?} on a screen 320 wide");
        let (_, about) = crate::ribbon::publication_style("panel").unwrap();
        let text = h.get_by_label(about).rect();
        assert!(text.right() <= 320.0, "{text:?} runs off a screen 320 wide");
    }

    /// Cancel closes the wizard with nothing made, and the next opening starts over.
    #[test]
    fn cancel_makes_nothing_and_the_next_opening_starts_over() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().subtitle = "Dropped".into();
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Cancel");
        assert_eq!(h.state().dialogs.open, None);
        assert_eq!(h.state().session.project.layouts.len(), before);
        h.state_mut().run("ui.dialog", json!({"name": "publicationWizard"})).unwrap();
        assert!(h.state().dialogs.publication.as_ref().unwrap().subtitle.is_empty());
    }
}
