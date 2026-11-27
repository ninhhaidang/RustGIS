//! The wizard's style step.
use super::*;
use rustgis_engine::cmd::layout::PUBLICATION_STYLES;

/// The style step: each sheet style's outline, with the accent its sheet would show, by the name
/// and description the menu gives it; a click on the outline picks the style, as on its name.
pub(super) fn style_step(ui: &mut egui::Ui, d: &mut Draft, app_accent: egui::Color32) {
    let before = d.style.clone();
    let chosen = d.chosen_accent(app_accent);
    // Room for the picked style's frame, which the box the step scrolls in would cut.
    ui.add_space(4.0);
    for (id, english) in PUBLICATION_STYLES {
        let (name, about) = crate::ribbon::publication_style(id).unwrap_or_else(|| (tl!(*english), ""));
        ui.horizontal(|ui| {
            // Not focusable: it shows no focus, and the radio button is the keyboard's way.
            let (rect, response) = ui.allocate_exact_size(OUTLINE, egui::Sense::CLICK);
            // In logical order, as a screen reader reads it.
            let label = i18n::fmt_raw("{style}: outline", &[("style", tl_raw!(*english))]);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &label));
            ui.painter().extend(outline(id, rect, chosen.unwrap_or_else(|| style_accent(id, app_accent))));
            if d.style == *id {
                let stroke = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
                ui.painter().rect_stroke(rect.expand(2.0), egui::CornerRadius::same(2), stroke, egui::StrokeKind::Outside);
            }
            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                d.style = (*id).to_string();
            }
            ui.vertical(|ui| {
                ui.radio_value(&mut d.style, (*id).to_string(), name);
                ui.label(egui::RichText::new(about).small().weak());
            });
        });
    }
    // If the swatch was only seeded from the previous style's default (never customised), follow
    // the newly chosen style's own accent instead of carrying the old style's colour over.
    if d.style != before && d.accent.is_some() && chosen.is_none() {
        d.accent = Some(style_accent(&d.style, app_accent));
    }
}

/// An outline's size: an A4 sheet turned to portrait.
const OUTLINE: egui::Vec2 = egui::vec2(34.0, 48.0);

/// An outline's map, and the grey of the texts it stands for.
const MAP_FILL: egui::Color32 = egui::Color32::from_rgb(0xCF, 0xE3, 0xE8);
const INK: egui::Color32 = egui::Color32::from_gray(150);

/// A sheet style's outline on `page`: where its title, map and footer go, and the column or panel
/// a style keeps for its locator and legend, after `layout.publication`'s arrangement of the style
/// (`cmd/layout/publication`), with `accent` where the sheet takes it, which Classic's nowhere
/// does. A locator or legend over the map isn't drawn: its corner follows the data. An outline, not
/// the sheet: laying the real one out for every style on every change would run the placement that many times
/// (#2850).
fn outline(style: &str, page: egui::Rect, accent: egui::Color32) -> Vec<egui::Shape> {
    use egui::{Color32, CornerRadius, Shape, Stroke, StrokeKind};
    let (ink, map, rule) = (INK, MAP_FILL, Color32::from_gray(190));
    let light = Color32::from_gray(245);
    // A box at fractions of the page, as the styles lay theirs out on a sheet.
    let at = |x: f32, y: f32, w: f32, h: f32| {
        egui::Rect::from_min_size(
            page.min + egui::vec2(x * page.width(), y * page.height()),
            egui::vec2(w * page.width(), h * page.height()),
        )
    };
    let fill = |r: egui::Rect, c: Color32| Shape::rect_filled(r, CornerRadius::ZERO, c);
    let card = |r: egui::Rect, c: Color32| Shape::rect_filled(r, CornerRadius::same(1), c);
    let boxed = |r: egui::Rect| Shape::rect_stroke(r, CornerRadius::ZERO, Stroke::new(0.5, rule), StrokeKind::Inside);
    let mut s = vec![fill(page, Color32::WHITE)];
    // The sources' and reference's boxes side by side across the foot.
    let foot = |y: f32| [boxed(at(0.06, y, 0.43, 0.11)), boxed(at(0.51, y, 0.43, 0.11))];
    match style {
        "banner" => {
            s.push(fill(at(0.0, 0.0, 1.0, 0.16), accent));
            s.push(fill(at(0.1, 0.05, 0.6, 0.04), light));
            s.push(fill(at(0.06, 0.2, 0.88, 0.59), map));
            s.extend(foot(0.83));
        }
        "panel" => {
            s.push(fill(at(0.05, 0.05, 0.6, 0.9), map));
            s.push(fill(at(0.68, 0.05, 0.27, 0.9), accent.gamma_multiply(0.2)));
            s.push(fill(at(0.71, 0.08, 0.2, 0.04), ink));
            s.push(fill(at(0.71, 0.14, 0.21, 0.025), accent));
            s.push(boxed(at(0.71, 0.2, 0.21, 0.14)));
            for y in [0.4, 0.46, 0.52, 0.82, 0.88] {
                s.push(fill(at(0.71, y, 0.18, 0.025), ink));
            }
        }
        "editorial" => {
            s.push(fill(at(0.06, 0.05, 0.5, 0.07), accent));
            s.push(fill(at(0.06, 0.135, 0.3, 0.025), ink));
            s.push(fill(at(0.06, 0.18, 0.58, 0.62), map));
            s.push(fill(at(0.68, 0.05, 0.26, 0.025), accent));
            s.push(fill(at(0.68, 0.1, 0.22, 0.025), ink));
            s.push(boxed(at(0.7, 0.18, 0.22, 0.16)));
            for y in [0.42, 0.48, 0.54, 0.6] {
                s.push(fill(at(0.68, y, 0.2, 0.025), ink));
            }
            s.extend(foot(0.84));
        }
        "modular" => {
            s.push(fill(page, Color32::from_gray(236)));
            s.push(fill(at(0.06, 0.04, 0.45, 0.045), ink));
            s.push(fill(at(0.06, 0.11, 0.16, 0.025), accent));
            s.push(card(at(0.06, 0.17, 0.6, 0.62), Color32::WHITE));
            s.push(fill(at(0.09, 0.19, 0.54, 0.58), map));
            s.push(card(at(0.69, 0.17, 0.25, 0.19), Color32::WHITE));
            s.push(card(at(0.69, 0.39, 0.25, 0.24), Color32::WHITE));
            for x in [0.06, 0.51] {
                s.push(card(at(x, 0.83, 0.43, 0.12), accent.gamma_multiply(0.25)));
                s.push(fill(at(x + 0.04, 0.855, 0.2, 0.025), accent));
            }
        }
        "poster" => {
            s.push(fill(page, accent));
            s.push(fill(at(0.1, 0.05, 0.6, 0.05), light));
            s.push(fill(at(0.1, 0.12, 0.4, 0.025), light.gamma_multiply(0.7)));
            s.push(card(at(0.08, 0.18, 0.84, 0.58), Color32::WHITE));
            s.push(fill(at(0.11, 0.21, 0.78, 0.52), map));
            let dark = Color32::from_black_alpha(70);
            s.push(card(at(0.08, 0.8, 0.4, 0.13), dark));
            s.push(card(at(0.52, 0.8, 0.4, 0.13), dark));
        }
        "edge" => {
            s.push(fill(at(0.08, 0.04, 0.3, 0.025), accent));
            s.push(fill(at(0.08, 0.085, 0.55, 0.05), ink));
            s.push(fill(at(0.08, 0.155, 0.4, 0.025), ink));
            s.push(fill(at(0.0, 0.21, 1.0, 0.6), map));
            s.push(Shape::line_segment(
                [at(0.08, 0.85, 0.84, 0.0).left_top(), at(0.08, 0.85, 0.84, 0.0).right_top()],
                Stroke::new(0.5, rule),
            ));
            for x in [0.08, 0.52] {
                s.push(fill(at(x, 0.875, 0.3, 0.025), ink));
                s.push(fill(at(x, 0.92, 0.24, 0.025), ink));
            }
        }
        // Classic, which takes the accent nowhere, and a style added without an outline of its own.
        _ => {
            s.push(fill(at(0.3, 0.04, 0.4, 0.04), ink));
            s.push(fill(at(0.38, 0.09, 0.24, 0.025), ink));
            s.push(fill(at(0.06, 0.13, 0.88, 0.66), map));
            s.extend(foot(0.83));
        }
    }
    // The sheet's edge, on every style.
    s.push(Shape::rect_stroke(page, CornerRadius::ZERO, Stroke::new(0.5, Color32::from_gray(120)), StrokeKind::Inside));
    s
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::kittest::{NodeT, Queryable};

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

    /// A page for an outline, off the origin so a shape placed from zero shows.
    fn page() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), vec2(44.0, 62.0))
    }

    const RED: egui::Color32 = egui::Color32::from_rgb(0xC0, 0x30, 0x30);

    /// Each sheet style has an outline of its own, inside its page.
    #[test]
    fn every_style_has_an_outline_of_its_own_inside_its_page() {
        let mut seen: Vec<String> = Vec::new();
        for (id, _) in PUBLICATION_STYLES {
            let shapes = outline(id, page(), RED);
            assert!(!shapes.is_empty(), "{id}");
            for s in &shapes {
                let r = s.visual_bounding_rect();
                assert!(page().expand(0.5).contains_rect(r), "{id}: {r:?}");
            }
            let drawn = format!("{shapes:?}");
            assert!(!seen.contains(&drawn), "{id} looks like another style");
            seen.push(drawn);
        }
    }

    /// The styles whose sheets are drawn in the accent: every one but Classic.
    fn accented() -> impl Iterator<Item = &'static str> {
        PUBLICATION_STYLES.iter().map(|(id, _)| *id).filter(|id| *id != "classic")
    }

    /// An outline's filled rectangles, in fractions of its page, and their colours.
    fn boxes(style: &str) -> Vec<(egui::Rect, egui::Color32)> {
        let unit = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(100.0, 100.0));
        outline(style, unit, RED)
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT => Some((
                    egui::Rect::from_min_max((r.rect.min.to_vec2() / 100.0).to_pos2(), (r.rect.max.to_vec2() / 100.0).to_pos2()),
                    r.fill,
                )),
                _ => None,
            })
            .collect()
    }

    fn map_of(style: &str) -> egui::Rect {
        boxes(style).into_iter().find(|(_, c)| *c == MAP_FILL).expect("a map").0
    }

    /// Classic's sheet takes the accent nowhere, so neither does its outline.
    #[test]
    fn classics_outline_takes_no_accent() {
        assert_eq!(format!("{:?}", outline("classic", page(), RED)), format!("{:?}", outline("classic", page(), egui::Color32::BLUE)));
    }

    /// Editorial's sheet: the title in the accent at the top left, over the map; a rule in the
    /// accent over a column at the map's right.
    #[test]
    fn editorials_outline_has_its_title_over_the_map_and_a_column_at_the_right() {
        let map = map_of("editorial");
        let marks: Vec<egui::Rect> = boxes("editorial").into_iter().filter(|(_, c)| *c == RED).map(|(r, _)| r).collect();
        assert_eq!(marks.len(), 2, "{marks:?}");
        assert!(marks.iter().any(|r| r.bottom() <= map.top() && r.right() <= map.right() && r.height() > 0.05), "title {marks:?}");
        assert!(marks.iter().any(|r| r.left() > map.right() && r.bottom() <= map.top()), "column rule {marks:?}");
    }

    /// Edge's sheet: the map from one side of the page to the other, the kicker in the accent
    /// above it.
    #[test]
    fn edges_outline_runs_its_map_from_side_to_side_under_its_kicker() {
        let map = map_of("edge");
        assert_eq!((map.left(), map.right()), (0.0, 1.0), "{map:?}");
        let marks: Vec<egui::Rect> = boxes("edge").into_iter().filter(|(_, c)| *c == RED).map(|(r, _)| r).collect();
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert!(marks[0].bottom() <= map.top(), "{marks:?}");
    }

    /// Modular's sheet: the map's card at the left, the locator's and the legend's cards at its
    /// right, one over the other.
    #[test]
    fn modulars_outline_has_two_cards_at_the_maps_right() {
        let map = map_of("modular");
        let beside: Vec<egui::Rect> = boxes("modular")
            .into_iter()
            .filter(|(r, c)| *c == egui::Color32::WHITE && r.left() > map.right() && r.top() < map.bottom())
            .map(|(r, _)| r)
            .collect();
        assert_eq!(beside.len(), 2, "{beside:?}");
        assert!(beside[0].bottom() < beside[1].top(), "{beside:?}");
    }

    /// Panel's sheet: the title in ink at the panel's top, a rule in the accent under it.
    #[test]
    fn panels_outline_has_a_rule_in_the_accent_under_its_title() {
        let map = map_of("panel");
        let marks: Vec<egui::Rect> = boxes("panel").into_iter().filter(|(_, c)| *c == RED).map(|(r, _)| r).collect();
        assert_eq!(marks.len(), 1, "{marks:?}");
        let title = boxes("panel").into_iter().any(|(r, c)| c == INK && r.left() > map.right() && r.bottom() <= marks[0].top());
        assert!(title, "no title over the rule at {:?}", marks[0]);
    }

    /// Every filled mark is thick enough to be seen on an outline of the size drawn: a point at
    /// least (the strokes are the sheet's edge and its boxes').
    #[test]
    fn every_filled_mark_of_an_outline_is_at_least_a_point_thick() {
        for (id, _) in PUBLICATION_STYLES {
            for (r, c) in boxes(id) {
                let thick = (r.width() * OUTLINE.x).min(r.height() * OUTLINE.y);
                assert!(thick >= 1.0, "{id}: {c:?} {thick} pt thick");
            }
        }
    }

    /// Each outline but Classic's is drawn in the accent given: its band, panel, rules or page.
    #[test]
    fn every_outline_takes_the_accent_given() {
        for accent in [RED, egui::Color32::from_rgb(0x20, 0x60, 0xA0)] {
            for id in accented() {
                let drawn = format!("{:?}", outline(id, page(), accent));
                assert!(drawn.contains(&format!("{accent:?}")), "{id} in {accent:?}");
            }
        }
    }

    /// A style without an outline of its own takes Classic's.
    #[test]
    fn a_style_without_an_outline_takes_classics() {
        let classic = outline("classic", page(), RED);
        assert!(!classic.is_empty());
        assert_eq!(format!("{:?}", outline("new-style", page(), RED)), format!("{classic:?}"));
    }

    /// How many filled rectangles of the frame drawn are in `colour`.
    fn filled(h: &egui_kittest::Harness<'static, RustGisApp>, colour: egui::Color32) -> usize {
        h.output().shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == colour)).count()
    }

    fn at_the_style_step(set: impl FnOnce(&mut Draft)) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut app = app();
        open(&mut app);
        let d = app.dialogs.publication.as_mut().unwrap();
        d.step = Step::Style;
        set(d);
        harness(app)
    }

    fn own(style: &str) -> egui::Color32 {
        style_accent(style, egui::Color32::TRANSPARENT)
    }

    /// With no colour picked, each outline is in its style's own accent.
    #[test]
    fn without_a_colour_picked_each_outline_is_in_its_styles_accent() {
        let h = at_the_style_step(|_| {});
        for id in accented() {
            assert!(filled(&h, own(id)) > 0, "{id}");
        }
        assert_eq!(filled(&h, RED), 0);
    }

    /// A colour picked is drawn on every outline whose sheet takes one.
    #[test]
    fn a_colour_picked_is_drawn_on_every_outline() {
        let h = at_the_style_step(|d| d.accent = Some(RED));
        assert!(filled(&h, RED) >= accented().count(), "{}", filled(&h, RED));
        assert_eq!(filled(&h, own("poster")), 0);
    }

    /// The accent only seeded from the selected style, never picked, isn't a colour of every
    /// style: each outline stays in its own.
    #[test]
    fn a_seeded_accent_leaves_each_outline_in_its_own() {
        let h = at_the_style_step(|d| d.accent = Some(own("classic")));
        for id in accented() {
            assert!(filled(&h, own(id)) > 0, "{id}");
        }
    }

    /// A click on a style's outline, an image named after the style, picks it.
    #[test]
    fn a_click_on_a_styles_outline_picks_it() {
        let mut h = at_the_style_step(|_| {});
        for (id, _) in PUBLICATION_STYLES {
            let (name, _) = crate::ribbon::publication_style(id).unwrap();
            assert!(h.query_by_role_and_label(Role::Image, &format!("{name}: outline")).is_some(), "{id}");
        }
        h.get_by_role_and_label(Role::Image, "Panel: outline").click();
        h.run_steps(4);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().style, "panel");
    }

    /// The frame around the picked style's outline is drawn whole: the first one isn't cut at the
    /// top of the box the step scrolls in.
    #[test]
    fn the_picked_outlines_frame_isnt_cut() {
        let h = at_the_style_step(|_| {});
        let frames: Vec<_> = h
            .output()
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.stroke.width == 1.5 && r.stroke_kind == egui::StrokeKind::Outside))
            .collect();
        assert_eq!(frames.len(), 1, "one style is picked");
        let drawn = frames[0].shape.visual_bounding_rect();
        assert!(frames[0].clip_rect.contains_rect(drawn), "{drawn:?} outside {:?}", frames[0].clip_rect);
    }

    /// Tab stops at each style's radio button, never at its outline, which shows no focus: the
    /// radio is the keyboard's way to pick a style.
    #[test]
    fn tab_never_stops_at_an_outline() {
        let mut h = at_the_style_step(|_| {});
        for _ in 0..3 * PUBLICATION_STYLES.len() {
            h.key_press(egui::Key::Tab);
            h.run_steps(2);
            for (id, _) in PUBLICATION_STYLES {
                let (name, _) = crate::ribbon::publication_style(id).unwrap();
                let label = format!("{name}: outline");
                let outline = h.get_by_role_and_label(Role::Image, &label);
                assert!(!outline.accesskit_node().is_focused(), "{id}'s outline takes the focus");
            }
        }
    }

    /// In a right-to-left language an outline is named in logical order, as a screen reader
    /// reads it, not in the display form its Latin runs are reversed in.
    #[test]
    fn an_outline_is_named_in_logical_order() {
        let previous = crate::i18n::current();
        crate::i18n::set_current(crate::i18n::Lang::from_code("fa").unwrap());
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Style;
        // The name alone is checked, which works on runners without Persian fonts.
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(vec2(1400.0, 1000.0))
            .build_ui_state(|ui, app: &mut RustGisApp| crate::dialogs::show(app, &ui.ctx().clone()), app);
        h.run_steps(4);
        let raw = tl_raw!("Classic");
        let name = i18n::fmt_raw("{style}: outline", &[("style", raw)]);
        let shown = i18n::fmt("{style}: outline", &[("style", raw)]);
        let found = h.query_by_role_and_label(Role::Image, &name).is_some();
        crate::i18n::set_current(previous);
        assert!(raw != "Classic" && name != shown, "the display form differs");
        assert!(found, "no outline named {name:?}");
    }
}
