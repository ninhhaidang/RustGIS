//! Publication Layout Wizard: `layout.publication` asked step by step (size, style, texts, accent),
//! then made at once, as the New Layout › Publication Layout menu makes it (#2850).
mod accent;
mod review;
mod size;
mod style;
mod texts;

use crate::{RustGisApp, i18n, theme::Tokens};
use accent::accent_step;
use review::review_step;
use rustgis_engine::Session;
use rustgis_engine::cmd::layout::{PUBLICATION_PROFILES, publication_style_accent};
use rustgis_engine::map::layout::PAGES;
use serde_json::{Value, json};
use size::size_step;
use style::style_step;
use texts::texts_step;

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

    /// The accent every style would take, or `None` where each keeps its own: none picked, or
    /// one only seeded from the selected style's default (never customised).
    fn chosen_accent(&self, app_accent: egui::Color32) -> Option<egui::Color32> {
        let own = style_accent(&self.style, app_accent);
        self.accent.filter(|&c| self.picked.is_some() || c != own)
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

/// What a figure leaves out, said where its size is picked and in the review: the texts and
/// accent typed for a sheet aren't drawn on it.
fn figure_note(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(tl!("A figure is the map edge to edge: its title, texts and style go in its caption.")).small().weak());
}

/// A style's default accent as an egui colour, falling back to `fallback` if the style is unknown.
fn style_accent(style: &str, fallback: egui::Color32) -> egui::Color32 {
    publication_style_accent(style).map_or(fallback, |c| egui::Color32::from_rgb(c.r(), c.g(), c.b()))
}

/// How the wizard names the credit line `layout.publication` writes when none is given: kept as a
/// literal here so catalog extraction sees it, and verified against the engine's
/// `PUBLICATION_DEFAULT_CREDIT` in tests.
fn default_credit() -> String {
    i18n::fmt("Made with RustGIS · {date}", &[("date", tl_raw!("today's date"))])
}

/// A field's width beside its label: 260 points, or what the row leaves once a longer label
/// (Greek "Orientation", in a wider interface font) takes its share, so the row never widens the
/// window.
fn field_width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(260.0)
}

/// The wizard's width, where the screen has room for it.
const WIDTH: f32 = 380.0;

/// The height every step takes: the style step's seven styles, their outlines and descriptions,
/// with room for the rounding a high-density screen adds to each line.
const STEP_HEIGHT: f32 = 415.0;

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
                    Step::Style => style_step(ui, &mut d, t.accent),
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

/// What the tests of the module's files share.
#[cfg(test)]
mod harness {
    use super::*;
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::kittest::Queryable;

    pub(super) fn app() -> RustGisApp {
        RustGisApp::with_sample()
    }

    pub(super) fn open(app: &mut RustGisApp) {
        app.run("ui.dialog", json!({"name": "publicationWizard"})).unwrap();
    }

    pub(super) fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        harness_sized(app, vec2(1400.0, 1000.0))
    }

    pub(super) fn harness_sized(app: RustGisApp, size: egui::Vec2) -> egui_kittest::Harness<'static, RustGisApp> {
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

    pub(super) fn click(h: &mut egui_kittest::Harness<'static, RustGisApp>, label: &str) {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run_steps(4);
    }

    pub(super) fn step(h: &egui_kittest::Harness<'static, RustGisApp>) -> Step {
        h.state().dialogs.publication.as_ref().expect("the wizard is open").step
    }
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::kittest::{NodeT, Queryable};

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

    /// The review's default credit line matches the engine's `PUBLICATION_DEFAULT_CREDIT`.
    #[test]
    fn default_credit_matches_the_engines_constant() {
        use rustgis_engine::cmd::layout::PUBLICATION_DEFAULT_CREDIT;
        assert_eq!(PUBLICATION_DEFAULT_CREDIT, "Made with RustGIS · {date}");
        assert_eq!(default_credit(), PUBLICATION_DEFAULT_CREDIT.replace("{date}", "today's date"));
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
