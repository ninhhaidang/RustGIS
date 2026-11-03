//! The Fills section of a polygon layer's symbology (#882 piece 5): fills stacked in one symbol,
//! each a colour, hatch lines, markers on a grid, a gradient or a shapeburst (shaded from the
//! edge inwards), as the Strokes section stacks a line's. Listed
//! top first, as they are seen; a layer with classes can stack fills for one class of its own,
//! picked at the top. Every change runs `layer.fillLayers`.

use egui::{CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::fills::{
    self, FillLayer, FillPattern, GradientPoint, GradientShape, MAX_BURST_BLUR, MAX_BURST_DISTANCE, MAX_DOT_SIZE, MAX_FILLS,
    MAX_HATCH_WIDTH, MAX_PATTERN_OFFSET, PATTERN_SPACING, Spread, Stops,
};
use rustgis_engine::map::symbology::ClassRef;
use rustgis_engine::map::{Layer, Marker, Symbol};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, c32, rgba, semibold};

/// The fill's settings as `layer.fillLayers` takes them.
fn fill_json(f: &FillLayer) -> Value {
    let color = f.color.map(|c| c.hex());
    match f.pattern {
        FillPattern::Solid => json!({"color": color, "pattern": "solid"}),
        FillPattern::Hatch { angle, spacing, width, offset } => {
            json!({"color": color, "pattern": "hatch", "angle": angle, "spacing": spacing, "width": width, "offset": offset})
        }
        FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset } => {
            let mut o = json!({
                "color": color, "pattern": "dots", "marker": marker.label(), "size": size, "outline": outline.map(|c| c.hex()),
                "outlineWidth": outline_width, "spacing": spacing, "displacement": displacement, "offset": offset,
            });
            // A character of a font (from a QGIS font marker): its char and family, as the command takes them.
            if let Marker::Char(g) = marker {
                o["marker"] = Value::Null;
                o["char"] = json!(g.ch.to_string());
                o["font"] = json!(g.font.as_str());
            }
            o
        }
        FillPattern::Gradient { shape, color2, stops, discrete, from, to, spread, angle, in_view } => {
            let point = |p: GradientPoint| match p {
                GradientPoint::At(xy) => json!(xy),
                GradientPoint::Centroid(_) => json!("centroid"),
            };
            json!({
                "color": color, "pattern": "gradient", "color2": color2.hex(), "stops": stops_json(&stops), "discrete": discrete,
                "shape": shape_name(shape).to_lowercase(), "from": point(from), "to": point(to),
                "spread": spread_name(spread).to_lowercase(), "angle": angle, "inView": in_view,
            })
        }
        FillPattern::Shapeburst { color2, stops, discrete, distance, blur, ignore_rings } => json!({
            "color": color, "pattern": "shapeburst", "color2": color2.hex(), "stops": stops_json(&stops), "discrete": discrete,
            "distance": distance, "blur": blur, "ignoreRings": ignore_rings,
        }),
    }
}

fn stops_json(stops: &Stops) -> Value {
    json!(stops.as_slice().iter().map(|s| json!([s.at, s.color.hex()])).collect::<Vec<_>>())
}

/// A gradient's shape, as the Type list names it.
fn shape_name(s: GradientShape) -> &'static str {
    match s {
        GradientShape::Linear => "Linear",
        GradientShape::Radial => "Radial",
        GradientShape::Conical => "Conical",
    }
}

/// What a gradient draws past its points, as the Spread list names it.
fn spread_name(s: Spread) -> &'static str {
    match s {
        Spread::Pad => "Pad",
        Spread::Reflect => "Reflect",
        Spread::Repeat => "Repeat",
    }
}

/// Runs `layer.fillLayers` with `fills` for the class picked (`None`: the layer's).
fn set_fills(app: &mut RustGisApp, class: Option<&ClassRef>, fills: &[FillLayer]) {
    let mut p = json!({"fills": fills.iter().map(fill_json).collect::<Vec<_>>()});
    match class {
        Some(ClassRef::Value(v)) => p["value"] = json!(v),
        Some(ClassRef::Index(i)) => p["index"] = json!(i),
        Some(ClassRef::Other) => p["other"] = json!(true),
        None => {}
    }
    let _ = app.run("layer.fillLayers", p);
}

/// A fill's kind, as the Draw list names it.
fn kind(f: &FillLayer) -> &'static str {
    match f.pattern {
        FillPattern::Solid => "Solid",
        FillPattern::Hatch { .. } => "Hatch lines",
        FillPattern::Dots { .. } => "Markers",
        FillPattern::Gradient { .. } => "Gradient",
        FillPattern::Shapeburst { .. } => "Shapeburst",
    }
}

pub(crate) fn fills_editor(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    let classes = crate::panes::stroke_classes(&l.renderer);
    let picked_id = egui::Id::unique(("fill_class", l.id));
    // The class being edited, while the renderer still has it; `None` is the layer's own fills.
    let picked: Option<ClassRef> =
        ui.data(|d| d.get_temp::<Option<ClassRef>>(picked_id)).flatten().filter(|c| classes.iter().any(|(k, ..)| k == c));
    let class = picked.as_ref();
    let base = class.and_then(|c| classes.iter().find(|(k, ..)| k == c)).map_or_else(|| l.renderer.base_symbol(), |(.., s)| *s);
    let own = class.and_then(|c| l.renderer.class_fills(c).ok()).map(<[FillLayer]>::to_vec);
    // What the class draws: its own fills, else the layer's.
    let fills = match &own {
        Some(v) if !v.is_empty() => v.clone(),
        Some(_) | None => l.fill_layers.clone(),
    };
    let borrowed = own.as_ref().is_some_and(Vec::is_empty);
    egui::Frame::NONE.fill(t.pane_header).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Fills")).font(semibold(12.5)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let add = ui.add_enabled(fills.len() < MAX_FILLS, egui::Button::new(tl!("Add Fill")).small());
                if add.on_hover_text(tl!("Stack another fill on top")).clicked() {
                    let mut next = fills.clone();
                    // The first stands for the symbol's own fill; the next hatches over it.
                    next.push(if next.is_empty() { FillLayer::default() } else { FillLayer::hatch(45.0, 6.0, 1.0) });
                    set_fills(app, class, &next);
                }
            });
        });
        if !classes.is_empty() {
            ui.horizontal(|ui| {
                let input_label = ui.label(egui::RichText::new(tl!("Fills for")).size(11.5).color(t.text_dim));
                let name = |c: Option<&ClassRef>| match c.and_then(|c| classes.iter().find(|(k, ..)| k == c)) {
                    Some((_, label, _)) => label.clone(),
                    None => tl!("All classes").to_string(),
                };
                egui::ComboBox::from_id_salt(("fill_class_pick", l.id))
                    .selected_text(name(class))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(class.is_none(), tl!("All classes")).on_hover_text(tl!("The layer's fills")).clicked() {
                            ui.data_mut(|d| d.insert_temp::<Option<ClassRef>>(picked_id, None));
                        }
                        for (k, label, _) in &classes {
                            // A class with fills of its own is marked.
                            let has_own = l.renderer.class_fills(k).is_ok_and(|f| !f.is_empty());
                            let text = if has_own { crate::i18n::fmt("{label} •", &[("label", label)]) } else { label.clone() };
                            if ui.selectable_label(class == Some(k), text).clicked() {
                                ui.data_mut(|d| d.insert_temp(picked_id, Some(k.clone())));
                            }
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
            });
        }
        // The stacked fills on a square, bottom first, as the map draws them.
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::same(4), t.ribbon);
        let swatch = Rect::from_center_size(r.center(), vec2(r.width().min(120.0) - 8.0, r.height() - 10.0));
        preview(ui, swatch, &fills, &base);
        if borrowed {
            ui.label(
                egui::RichText::new(tl!("This class is drawn with the layer's fills. Changing them here gives it its own."))
                    .size(11.5)
                    .color(t.text_dim),
            );
        }
        if fills.is_empty() {
            ui.label(
                egui::RichText::new(tl!("One fill, from the symbol. Add fills to stack colours, hatch lines and markers."))
                    .size(11.5)
                    .color(t.text_dim),
            );
            return;
        }
        let n = fills.len();
        for i in (0..n).rev() {
            let Some(f) = fills.get(i).copied() else { continue };
            ui.add_space(4.0);
            ui.separator();
            ui.horizontal(|ui| {
                let title = crate::i18n::fmt("Fill {n}", &[("n", &(n - i).to_string())]);
                ui.label(egui::RichText::new(title).size(11.5).color(t.text_dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::widgets::icon_button(ui, "close", tl!("Remove")).clicked() {
                        let mut next = fills.clone();
                        next.remove(i);
                        set_fills(app, class, &next);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("↓").small()).on_hover_text(tl!("Move down")).clicked() {
                        let mut next = fills.clone();
                        next.swap(i, i - 1);
                        set_fills(app, class, &next);
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("↑").small()).on_hover_text(tl!("Move up")).clicked() {
                        let mut next = fills.clone();
                        next.swap(i, i + 1);
                        set_fills(app, class, &next);
                    }
                });
            });
            let set = |app: &mut RustGisApp, new: FillLayer| {
                let mut next = fills.clone();
                if let Some(slot) = next.get_mut(i) {
                    *slot = new;
                }
                set_fills(app, class, &next);
            };
            egui::Grid::new(("fill_grid", i)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                let input_label = ui.label(tl!("Draw"));
                egui::ComboBox::from_id_salt(("fill_draw", i))
                    .selected_text(tl!(kind(&f)))
                    .show_ui(ui, |ui| {
                        let options = [
                            FillLayer { pattern: FillPattern::Solid, ..f },
                            FillLayer::hatch(45.0, 6.0, 1.0),
                            FillLayer {
                                pattern: FillPattern::Dots {
                                    marker: Marker::Circle,
                                    size: 3.0,
                                    outline: None,
                                    outline_width: 0.0,
                                    spacing: [8.0, 8.0],
                                    displacement: [4.0, 0.0],
                                    offset: [0.0, 0.0],
                                },
                                ..f
                            },
                            FillLayer {
                                pattern: FillPattern::Gradient {
                                    shape: GradientShape::Linear,
                                    color2: rustgis_engine::map::Rgba::rgb(255, 255, 255),
                                    stops: Stops::default(),
                                    discrete: false,
                                    from: GradientPoint::At([0.5, 0.0]),
                                    to: GradientPoint::At([0.5, 1.0]),
                                    spread: Spread::Pad,
                                    angle: 0.0,
                                    in_view: false,
                                },
                                ..f
                            },
                            FillLayer {
                                pattern: FillPattern::Shapeburst {
                                    color2: rustgis_engine::map::Rgba::rgb(255, 255, 255),
                                    stops: Stops::default(),
                                    discrete: false,
                                    // QGIS's: over the whole shape.
                                    distance: None,
                                    blur: 0,
                                    ignore_rings: false,
                                },
                                ..f
                            },
                        ];
                        for option in options {
                            let option = FillLayer { color: f.color, ..option };
                            if ui.selectable_label(kind(&option) == kind(&f), tl!(kind(&option))).clicked() && kind(&option) != kind(&f) {
                                set(app, option);
                            }
                        }
                    })
                    .response
                    .on_hover_text(tl!(
                        "A colour, hatch lines, markers on a grid, a gradient, or a shapeburst (shaded from the edge inwards)"
                    ))
                    .labelled_by(input_label.id);
                ui.end_row();
                ui.label(tl!("Color"));
                ui.horizontal(|ui| {
                    let c = f.color.unwrap_or(base.fill);
                    let mut cc = c32(c);
                    let resp = crate::widgets::color_button(ui, &mut cc);
                    if resp.changed() {
                        crate::widgets::one_step_per_press(app, &resp);
                        set(app, FillLayer { color: Some(rgba(cc)), ..f });
                    }
                    let mut from_symbol = f.color.is_none();
                    if ui.checkbox(&mut from_symbol, tl!("From symbol")).on_hover_text(tl!("Use each feature's fill color")).changed() {
                        set(app, FillLayer { color: if from_symbol { None } else { Some(c) }, ..f });
                    }
                });
                ui.end_row();
                // A number changed, its command run straight after (one step to undo a drag).
                let number =
                    |app: &mut RustGisApp, ui: &mut Ui, label: &str, v: f32, range: std::ops::RangeInclusive<f32>, suffix: &str| {
                        let input_label = ui.label(tl!(label));
                        let mut x = v;
                        let resp = ui
                            .add(egui::DragValue::new(&mut x).update_while_editing(false).range(range).speed(0.1).suffix(suffix))
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let changed = crate::widgets::changed(&resp) && x != v;
                        if changed {
                            crate::widgets::one_step_per_press(app, &resp);
                        }
                        changed.then_some(x)
                    };
                // A gradient's or shapeburst's second colour, and its stops between (set by command).
                if let FillPattern::Gradient { color2, stops, .. } | FillPattern::Shapeburst { color2, stops, .. } = f.pattern {
                    let input_label = ui.label(tl!("Second color"));
                    ui.horizontal(|ui| {
                        let mut cc = c32(color2);
                        let resp = crate::widgets::color_button(ui, &mut cc).labelled_by(input_label.id);
                        if resp.changed() {
                            crate::widgets::one_step_per_press(app, &resp);
                            let mut pattern = f.pattern;
                            if let FillPattern::Gradient { color2, .. } | FillPattern::Shapeburst { color2, .. } = &mut pattern {
                                *color2 = rgba(cc);
                            }
                            set(app, FillLayer { pattern, ..f });
                        }
                        if !stops.is_empty() {
                            let n = stops.as_slice().len().to_string();
                            ui.label(
                                egui::RichText::new(crate::i18n::fmt("{n} colors between", &[("n", &n)])).size(11.5).color(t.text_dim),
                            );
                        }
                    });
                    ui.end_row();
                }
                match f.pattern {
                    FillPattern::Solid => {}
                    FillPattern::Gradient { shape, spread, angle, .. } => {
                        let with = |shape: GradientShape, spread: Spread, a: f32| {
                            let mut pattern = f.pattern;
                            if let FillPattern::Gradient { shape: s, spread: p, angle, .. } = &mut pattern {
                                (*s, *p, *angle) = (shape, spread, a);
                            }
                            FillLayer { pattern, ..f }
                        };
                        let input_label = ui.label(tl!("Type"));
                        egui::ComboBox::from_id_salt(("fill_gradient", i))
                            .selected_text(tl!(shape_name(shape)))
                            .show_ui(ui, |ui| {
                                for s in [GradientShape::Linear, GradientShape::Radial, GradientShape::Conical] {
                                    if ui.selectable_label(s == shape, tl!(shape_name(s))).clicked() && s != shape {
                                        set(app, with(s, spread, angle));
                                    }
                                }
                            })
                            .response
                            .on_hover_text(tl!("Straight across, in rings from its first point, or round its first point"))
                            .labelled_by(input_label.id);
                        ui.end_row();
                        if let Some(a) = number(app, ui, "Angle", angle, 0.0..=360.0, "°") {
                            set(app, with(shape, spread, a));
                        }
                        let input_label = ui.label(tl!("Spread"));
                        egui::ComboBox::from_id_salt(("fill_spread", i))
                            .selected_text(tl!(spread_name(spread)))
                            .show_ui(ui, |ui| {
                                for s in [Spread::Pad, Spread::Reflect, Spread::Repeat] {
                                    if ui.selectable_label(s == spread, tl!(spread_name(s))).clicked() && s != spread {
                                        set(app, with(shape, s, angle));
                                    }
                                }
                            })
                            .response
                            .on_hover_text(tl!("Past its points: its end colors, back and forth, or over again"))
                            .labelled_by(input_label.id);
                        ui.end_row();
                    }
                    FillPattern::Shapeburst { distance, blur, ignore_rings, .. } => {
                        let with = |d: Option<f32>, b: u8, ig: bool| {
                            let mut pattern = f.pattern;
                            if let FillPattern::Shapeburst { distance, blur, ignore_rings, .. } = &mut pattern {
                                (*distance, *blur, *ignore_rings) = (d, b, ig);
                            }
                            FillLayer { pattern, ..f }
                        };
                        ui.label("");
                        let mut whole = distance.is_none();
                        if ui.checkbox(&mut whole, tl!("Whole shape")).on_hover_text(tl!("Shade to the point deepest inside")).changed() {
                            set(app, with(if whole { None } else { Some(14.0) }, blur, ignore_rings));
                        }
                        ui.end_row();
                        if let Some(d) = distance
                            && let Some(d) = number(app, ui, "Distance", d, 0.0..=MAX_BURST_DISTANCE, " pt")
                        {
                            set(app, with(Some(d), blur, ignore_rings));
                        }
                        if let Some(b) = number(app, ui, "Blur", f32::from(blur), 0.0..=f32::from(MAX_BURST_BLUR), " px") {
                            set(app, with(distance, b.round() as u8, ignore_rings));
                        }
                        ui.label("");
                        let mut ig = ignore_rings;
                        if ui.checkbox(&mut ig, tl!("Ignore holes")).on_hover_text(tl!("Shade from the outer edge alone")).changed() {
                            set(app, with(distance, blur, ig));
                        }
                        ui.end_row();
                    }
                    FillPattern::Hatch { angle, spacing, width, offset } => {
                        let hatch = |angle, spacing, width, offset| FillLayer {
                            pattern: FillPattern::Hatch { angle, spacing, width, offset },
                            ..f
                        };
                        if let Some(a) = number(app, ui, "Angle", angle, 0.0..=360.0, "°") {
                            set(app, hatch(a, spacing, width, offset));
                        }
                        if let Some(s) = number(app, ui, "Spacing", spacing, PATTERN_SPACING, " pt") {
                            set(app, hatch(angle, s, width, offset));
                        }
                        if let Some(w) = number(app, ui, "Line width", width, 0.0..=MAX_HATCH_WIDTH, " pt") {
                            set(app, hatch(angle, spacing, w, offset));
                        }
                        if let Some(o) = number(app, ui, "Offset", offset, -MAX_PATTERN_OFFSET..=MAX_PATTERN_OFFSET, " pt") {
                            set(app, hatch(angle, spacing, width, o));
                        }
                    }
                    FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset } => {
                        let dots = |marker, size, spacing: [f32; 2], displacement: [f32; 2]| FillLayer {
                            pattern: FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset },
                            ..f
                        };
                        let input_label = ui.label(tl!("Marker"));
                        egui::ComboBox::from_id_salt(("fill_marker", i))
                            .selected_text(tl!(marker.label()))
                            .show_ui(ui, |ui| {
                                for m in Marker::ALL {
                                    if ui.selectable_label(m == marker, tl!(m.label())).clicked() && m != marker {
                                        set(app, dots(m, size, spacing, displacement));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        if let Some(s) = number(app, ui, "Size", size, 0.0..=MAX_DOT_SIZE, " pt") {
                            set(app, dots(marker, s, spacing, displacement));
                        }
                        if let Some(s) = number(app, ui, "Spacing across", spacing[0], PATTERN_SPACING, " pt") {
                            set(app, dots(marker, size, [s, spacing[1]], displacement));
                        }
                        if let Some(s) = number(app, ui, "Spacing down", spacing[1], PATTERN_SPACING, " pt") {
                            set(app, dots(marker, size, [spacing[0], s], displacement));
                        }
                        if let Some(d) = number(app, ui, "Rows shifted", displacement[0], 0.0..=(spacing[0] - 0.01).max(0.0), " pt") {
                            set(app, dots(marker, size, spacing, [d, displacement[1]]));
                        }
                    }
                }
            });
        }
    });
}

/// The stacked `fills` on `r`, bottom first: a colour fills it, hatch lines cross it at their
/// angle and spacing, markers sit on their grid (as circles), a gradient or shapeburst shades it
/// (in cells), each in its colour or `base`'s.
fn preview(ui: &Ui, r: Rect, fills: &[FillLayer], base: &Symbol) {
    let p = ui.painter().with_clip_rect(r);
    let one = [FillLayer::default()];
    let shown = if fills.is_empty() { &one[..] } else { fills };
    // (Only what the map draws: a hatch of no width, a marker of no size, no colour to show.)
    for f in shown.iter().filter(|f| f.draws(base.fill)) {
        let c = c32(f.color.unwrap_or(base.fill));
        match f.pattern {
            FillPattern::Solid => {
                p.rect_filled(r, CornerRadius::ZERO, c);
            }
            FillPattern::Hatch { angle, spacing, width, .. } => {
                // Lines across the swatch, `spacing` apart along their normal.
                let (sin, cos) = (-angle.to_radians()).sin_cos();
                let (dir, normal) = (vec2(cos, sin), vec2(-sin, cos));
                let reach = r.width().hypot(r.height());
                let step = spacing.max(1.0);
                let mut k = -reach;
                while k <= reach {
                    let mid = r.center() + normal * k;
                    p.line_segment([mid - dir * reach, mid + dir * reach], Stroke::new(width.max(0.5), c));
                    k += step;
                }
            }
            FillPattern::Dots { size, spacing, displacement, .. } => {
                // (No finer than 4 points in the swatch: it is a picture, not every dot.)
                let [dx, dy] = spacing.map(|s| s.max(4.0));
                // (Shifts widened with the spacing, so a half shift stays a half.)
                let shift = [0, 1].map(|i| displacement[i] * if spacing[i] > 0.0 { [dx, dy][i] / spacing[i] } else { 1.0 });
                // As the map lays them: odd rows shifted across, odd columns down (a row and a
                // column more each way for the shifts).
                let (cols, rows) = ((r.width() / dx).ceil() as i32 + 1, (r.height() / dy).ceil() as i32 + 1);
                for row in -1..=rows {
                    for col in -1..=cols {
                        let x = r.min.x + (col as f32 + 0.5) * dx + if row.rem_euclid(2) == 1 { shift[0] } else { 0.0 };
                        let y = r.min.y + (row as f32 + 0.5) * dy + if col.rem_euclid(2) == 1 { shift[1] } else { 0.0 };
                        p.circle_filled(pos2(x, y), (size / 2.0).max(0.5), c);
                    }
                }
            }
            FillPattern::Gradient { color2, stops, discrete, .. } | FillPattern::Shapeburst { color2, stops, discrete, .. } => {
                let ramp = fills::ramp(f.color.unwrap_or(base.fill), color2, &stops, discrete);
                // Cells about 3 points across, each in the colour at its middle.
                let (cols, rows) = ((r.width() / 3.0).ceil().max(1.0) as usize, (r.height() / 3.0).ceil().max(1.0) as usize);
                let (w, h) = (r.width() / cols as f32, r.height() / rows as f32);
                for row in 0..rows {
                    for col in 0..cols {
                        let (u, v) = ((col as f32 + 0.5) / cols as f32, (row as f32 + 0.5) / rows as f32);
                        let cell = Rect::from_min_size(pos2(r.min.x + col as f32 * w, r.min.y + row as f32 * h), vec2(w + 0.5, h + 0.5));
                        p.rect_filled(cell, CornerRadius::ZERO, c32(fills::color_at(&ramp, along(&f.pattern, u, v, r))));
                    }
                }
            }
        }
    }
    // The symbol's outline over them.
    if base.stroke_width > 0.0 {
        p.rect_stroke(r, CornerRadius::ZERO, Stroke::new(base.stroke_width.min(3.0), c32(base.stroke)), egui::StrokeKind::Inside);
    }
}

/// How far along its colours a gradient or shapeburst is at (`u`, `v`) in the swatch `r` (0 to 1
/// across and down), as the map shades a feature of the swatch's shape.
fn along(pattern: &FillPattern, u: f32, v: f32, r: Rect) -> f32 {
    let (w, h) = (r.width().max(1.0), r.height().max(1.0));
    match *pattern {
        FillPattern::Gradient { shape, from, to, spread, angle, .. } => {
            let at = |p: GradientPoint| match p {
                GradientPoint::At([x, y]) => (x * w, y * h),
                GradientPoint::Centroid(_) => (w / 2.0, h / 2.0),
            };
            // The point turned back clockwise round the middle, against the gradient's turn.
            let (sin, cos) = angle.to_radians().sin_cos();
            let (dx, dy) = (u * w - w / 2.0, v * h - h / 2.0);
            let (x, y) = (w / 2.0 + dx * cos - dy * sin, h / 2.0 + dx * sin + dy * cos);
            let ((ax, ay), (bx, by)) = (at(from), at(to));
            let (vx, vy) = (bx - ax, by - ay);
            let len2 = vx * vx + vy * vy;
            let t = match shape {
                _ if len2 <= 0.0 => 0.0,
                GradientShape::Linear => ((x - ax) * vx + (y - ay) * vy) / len2,
                GradientShape::Radial => ((x - ax).hypot(y - ay)) / len2.sqrt(),
                GradientShape::Conical => {
                    // Anticlockwise on screen from the direction of `to` (y runs down).
                    let turn = (-(y - ay)).atan2(x - ax) - (-vy).atan2(vx);
                    turn.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
                }
            };
            match spread {
                Spread::Pad => t.clamp(0.0, 1.0),
                Spread::Repeat => t.rem_euclid(1.0),
                Spread::Reflect => 1.0 - (t.rem_euclid(2.0) - 1.0).abs(),
            }
        }
        FillPattern::Shapeburst { distance, .. } => {
            let edge = (u * w).min((1.0 - u) * w).min(v * h).min((1.0 - v) * h);
            let depth = distance.unwrap_or(w.min(h) / 2.0).max(0.5);
            (edge / depth).clamp(0.0, 1.0)
        }
        _ => 0.0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// Fills are added (the first the symbol's colour, the next hatch lines over it), drawn as
    /// markers or removed, each a `layer.fillLayers`.
    #[test]
    fn fills_are_stacked_and_changed_in_the_editor() {
        let app = RustGisApp::with_sample();
        // The section's fonts (its semibold heading) are installed on the first frame, as the app does.
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(420.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let Some(l) = app.session.map().layer_by_name("Countries").cloned() else { return };
                fills_editor(app, ui, &l, &Tokens::light());
            },
            app,
        );
        h.run();
        let fills =
            |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer_by_name("Countries").unwrap().fill_layers.clone();
        assert!(fills(&h).is_empty());
        assert!(h.query_by_label("Fill 1").is_none());
        h.get_by_label("Add Fill").click();
        h.run();
        assert_eq!(fills(&h), [FillLayer::default()], "the symbol's colour first");
        h.get_by_label("Add Fill").click();
        h.run();
        let f = fills(&h);
        assert_eq!(f.len(), 2);
        assert!(matches!(f[1].pattern, FillPattern::Hatch { .. }), "hatch lines over it: {f:?}");
        assert!(h.query_by_label("Fill 2").is_some() && h.query_by_label("Angle").is_some());
        // The top one (listed first) drawn as markers.
        h.query_all_by_label("Draw").next().unwrap().click();
        h.run();
        h.get_by_label("Markers").click();
        h.run();
        assert!(matches!(fills(&h)[1].pattern, FillPattern::Dots { .. }), "{:?}", fills(&h));
        // A gradient, its second colour and type shown; then a shapeburst over the whole shape.
        h.query_all_by_label("Draw").next().unwrap().click();
        h.run();
        h.get_by_label("Gradient").click();
        h.run();
        assert!(matches!(fills(&h)[1].pattern, FillPattern::Gradient { .. }), "{:?}", fills(&h));
        assert!(h.query_by_label("Second color").is_some() && h.query_by_label("Spread").is_some());
        h.query_all_by_label("Draw").next().unwrap().click();
        h.run();
        h.get_by_label("Shapeburst").click();
        h.run();
        assert!(matches!(fills(&h)[1].pattern, FillPattern::Shapeburst { distance: None, .. }), "over the whole shape: {:?}", fills(&h));
        h.get_by_label("Whole shape").click();
        h.run();
        assert!(matches!(fills(&h)[1].pattern, FillPattern::Shapeburst { distance: Some(_), .. }), "{:?}", fills(&h));
        assert!(h.query_by_label("Distance").is_some());
        // Removed: the first remove button is the top fill's.
        h.query_all_by_label("Remove").next().unwrap().click();
        h.run();
        assert_eq!(fills(&h), [FillLayer::default()]);
    }
}
