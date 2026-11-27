use super::*;

/// A chart element of layer "t" on frame 1, with `extra` keys (its field among them).
fn chart_kind(extra: serde_json::Value) -> ElementKind {
    let mut chart = serde_json::json!({"frame": 1, "layer": "t"});
    if let (Some(c), Some(e)) = (chart.as_object_mut(), extra.as_object()) {
        c.extend(e.clone());
    }
    serde_json::from_value(serde_json::json!({ "Chart": chart })).unwrap()
}

/// The chart as it was drawn before charts were grouped, ordered or cut by choice, kept to
/// show a chart saved then still draws to the same pixels.
fn chart_before(pm: &mut Canvas, r: Rect, ds: &rustgis_data::Dataset, field: &str, label: Option<&str>, dark: Rgba, ppp: f64) {
    let ppp = ppp as f32;
    let Some(fi) = ds.field_index(field) else {
        placeholder(pm, r, "Chart", ppp as f64);
        return;
    };
    let li = label.and_then(|l| ds.field_index(l));
    let mut vals: Vec<(f64, String)> = (0..ds.features.len())
        .filter_map(|row| {
            ds.value(row, fi).as_f64().filter(|v| v.is_finite()).map(|v| (v, li.map(|i| ds.value(row, i).to_string()).unwrap_or_default()))
        })
        .collect();
    vals.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    vals.truncate(20);
    let max = vals.iter().map(|v| v.0).fold(0.0_f64, f64::max);
    if vals.is_empty() || max <= 0.0 {
        placeholder(pm, r, "Chart", ppp as f64);
        return;
    }
    let px = 7.5 * 1.33 * ppp;
    let title_h = px * 1.6;
    text::text_at(pm, field, r.x(), r.y() + title_h / 2.0, px * 1.15, dark, true, text::Align::Left, false);
    let label_w = if li.is_some() { r.width() * 0.32 } else { 0.0 };
    let bar_area = r.width() - label_w - 4.0;
    let bh = ((r.height() - title_h) / vals.len() as f32).min(18.0 * ppp);
    for (i, (v, name)) in vals.iter().enumerate() {
        let y = r.y() + title_h + bh * i as f32;
        let w = (bar_area * (*v / max) as f32).max(1.0);
        if let Some(b) = Rect::from_xywh(r.x() + label_w, y + bh * 0.15, w, bh * 0.7) {
            pm.fill_rect(b, &paint(Rgba::rgb(0x3B, 0x82, 0xC4), 1.0), Transform::identity(), None);
        }
        if li.is_some() {
            let mut t: String = name.chars().take(30).collect();
            while !t.is_empty() && text::measure(&t, 7.5, false) * ppp > label_w - 4.0 * ppp {
                t.pop();
            }
            text::text_at(pm, &t, r.x(), y + bh / 2.0, px.min(bh * 0.9), dark, false, text::Align::Left, false);
        }
    }
}

/// A chart saved before grouping, order and limits draws to the same pixels: with labels or
/// without, past 20 features, with rows with no number, a missing label field, all zero.
#[test]
fn a_chart_saved_before_draws_to_the_same_pixels() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Double)]);
    for i in 0..27 {
        let n = match i {
            3 => Value::Null,
            5 => Value::Float(f64::NAN),
            _ => Value::Float(((i * 37) % 23) as f64 + 0.5),
        };
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text(format!("Place {i}")), n]);
    }
    let mut zeros = Dataset::new("t", GeometryType::Point, vec![Field::new("n", FieldType::Double)]);
    zeros.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Float(0.0)]);
    let r = Rect::from_xywh(7.0, 5.0, 230.0, 170.0).unwrap();
    for (ds, label, ppp) in [(&ds, Some("name"), 1.0), (&ds, None, 2.0), (&ds, Some("gone"), 1.5), (&zeros, None, 1.0)] {
        let draw = |f: &dyn Fn(&mut Canvas)| {
            let mut c = Canvas::new(480, 360).unwrap();
            f(&mut c);
            c
        };
        let before = draw(&|c| chart_before(c, r, ds, "n", label, INK, ppp));
        let kind = chart_kind(serde_json::json!({"field": "n", "label": label}));
        let now = draw(&|c| match kind.chart_data(ds) {
            Some(data) => chart(c, r, &data, &ChartLook::default(), INK, ppp, false),
            None => placeholder(c, r, "Chart", ppp),
        });
        assert!(before.pixmap().data() == now.pixmap().data(), "{label:?} at {ppp}");
    }
}

/// A negative value runs left of a zero line in the ink; a positive one right of it.
#[test]
fn a_chart_with_a_negative_value_draws_from_a_zero_line() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("n", FieldType::Double)]);
    for n in [3.0, -1.0] {
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Float(n)]);
    }
    let data = chart_kind(serde_json::json!({"field": "n"})).chart_data(&ds).unwrap();
    let mut c = Canvas::new(200, 120).unwrap();
    let r = Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap();
    chart(&mut c, r, &data, &ChartLook::default(), INK, 1.0, false);
    // Bar area 196 px over a span of 4: zero at 49 px. Rows of 18 px under a 16 px title,
    // their bars from 18.7 to 30.6 px and from 36.7 to 48.6 px down.
    assert!(bar_blue(&c, 150, 25) && !bar_blue(&c, 30, 25), "3 runs right of zero");
    assert!(bar_blue(&c, 30, 43) && !bar_blue(&c, 70, 43), "-1 runs left of zero");
    // Between the bars, the zero line (0.75 px wide, so partly covering its pixel).
    let line = c.pixmap().pixel(49, 34).unwrap();
    assert!(line.alpha() > 60 && line.blue() < 60, "the zero line: {line:?}");
}

/// A chart of `vals` (value and name, labelled by the name) drawn with the keys `extra` on a
/// 200 × 120 canvas filling it.
fn chart_drawn(vals: &[(f64, &str)], extra: serde_json::Value) -> Canvas {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Double)]);
    for (n, name) in vals {
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text((*name).into()), Value::Float(*n)]);
    }
    let mut keys = serde_json::json!({"field": "n", "order": "layer"});
    if let (Some(k), Some(e)) = (keys.as_object_mut(), extra.as_object()) {
        k.extend(e.clone());
    }
    let kind = chart_kind(keys);
    let data = kind.chart_data(&ds).unwrap();
    let mut c = Canvas::new(200, 120).unwrap();
    let r = Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap();
    chart(&mut c, r, &data, &kind.chart_look().unwrap(), INK, 1.0, false);
    c
}

/// Each bar in its own colour (#2487 part 3), a bar without one in the look's: across and as
/// columns.
#[test]
fn chart_bars_take_their_own_colours() {
    let red = Rgba::rgb(0xD0, 0x10, 0x10);
    let data = rustgis_map::layout::ChartData {
        bars: vec![(10.0, String::new()), (10.0, String::new())],
        labelled: false,
        left_out: 0,
        out_of_range: 0,
        no_number: 0,
        title: String::new(),
        colors: vec![Some(red), None],
        ..Default::default()
    };
    let is_red = |c: &Canvas, x: u32, y: u32| c.pixmap().pixel(x, y).is_some_and(|p| p.red() > 150 && p.blue() < 60);
    for orientation in [Orientation::Horizontal, Orientation::Vertical] {
        let look = ChartLook { orientation, ..ChartLook::default() };
        let mut c = Canvas::new(200, 120).unwrap();
        chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &data, &look, INK, 1.0, false);
        // Across, two rows 18 px high from the top; as columns, two slots 100 px wide.
        let ((x0, y0), (x1, y1)) = match orientation {
            Orientation::Horizontal => ((20, 9), (20, 27)),
            Orientation::Vertical => ((50, 100), (150, 100)),
        };
        assert!(is_red(&c, x0, y0), "{orientation:?}: the first bar in its own colour");
        assert!(bar_blue(&c, x1, y1) && !is_red(&c, x1, y1), "{orientation:?}: the second in the look's");
    }
}

/// Stacked (#2487 part 3b), a bar is drawn as its stretches in the key's colours, those above
/// zero from it on and those below from it back, with the key over the bars: across and as
/// columns.
#[test]
fn stacked_bars_draw_their_stretches_and_key() {
    let (red, green) = (Rgba::rgb(0xD0, 0x10, 0x10), Rgba::rgb(0x10, 0xC0, 0x10));
    let is = |c: &Canvas, x: u32, y: u32, want: Rgba| {
        c.pixmap()
            .pixel(x, y)
            .is_some_and(|p| p.red().abs_diff(want.0[0]) < 30 && p.green().abs_diff(want.0[1]) < 30 && p.blue().abs_diff(want.0[2]) < 30)
    };
    let stacked = |stretches: Vec<f64>| rustgis_map::layout::ChartData {
        bars: vec![(stretches.iter().sum(), String::new())],
        key: vec![("a".into(), red), ("b".into(), green)],
        stretches: vec![stretches],
        ..Default::default()
    };
    let draw = |data: &rustgis_map::layout::ChartData, orientation: Orientation| {
        let mut c = Canvas::new(200, 120).unwrap();
        let look = ChartLook { orientation, ..ChartLook::default() };
        chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), data, &look, INK, 1.0, false);
        c
    };
    // The key's row (14 px) over a bar 18 px high: 6 of 10 in red from the left, 4 in green.
    let c = draw(&stacked(vec![6.0, 4.0]), Orientation::Horizontal);
    assert!(is(&c, 3, 7, red), "the key's first swatch");
    assert!(is(&c, 50, 23, red) && is(&c, 150, 23, green), "the stretches along the bar");
    assert!(!is(&c, 50, 7, red), "the bar starts under the key");
    // As a column from the bottom up: red at its foot, green over it.
    let c = draw(&stacked(vec![6.0, 4.0]), Orientation::Vertical);
    assert!(is(&c, 100, 100, red) && is(&c, 100, 30, green), "the stretches up the column");
    // Mirrored (#2487 part 4), the stretches run from the right, the key staying where it is.
    let mut c = Canvas::new(200, 120).unwrap();
    let look = ChartLook { mirror: true, ..ChartLook::default() };
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &stacked(vec![6.0, 4.0]), &look, INK, 1.0, false);
    assert!(is(&c, 3, 7, red) && is(&c, 150, 23, red) && is(&c, 50, 23, green), "red from the right, green past it");
    // On a fixed maximum a stretch past it is cut at it, and one wholly past it isn't drawn.
    for (max, far) in [(8.0, green), (5.0, red)] {
        let mut c = Canvas::new(200, 120).unwrap();
        let look = ChartLook { max: Some(max), ..ChartLook::default() };
        chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &stacked(vec![6.0, 4.0]), &look, INK, 1.0, false);
        assert!(is(&c, 190, 23, far) && !is(&c, 199, 23, far), "{max}: the bar ends at the maximum");
    }
    // At 5 the green stretch, from 6 on, is wholly past it: none of it is drawn, anywhere.
    let mut c = Canvas::new(200, 120).unwrap();
    let look = ChartLook { max: Some(5.0), ..ChartLook::default() };
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &stacked(vec![6.0, 4.0]), &look, INK, 1.0, false);
    assert!((0..200).all(|x| !is(&c, x, 23, green)), "no green along the bar");
    // One running back from zero is drawn whole on a fixed maximum too: 2 of a span of 7 (from
    // -2 to 5) left of zero, red cut at 5 on the right.
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &stacked(vec![6.0, -2.0]), &look, INK, 1.0, false);
    assert!(is(&c, 30, 23, green) && is(&c, 150, 23, red) && !is(&c, 199, 23, red), "back from zero, cut at the maximum");
    // Stretches all below zero run back from it, the first one too: red 6 of 10 from the
    // right, green past it.
    let c = draw(&stacked(vec![-6.0, -4.0]), Orientation::Horizontal);
    assert!(is(&c, 150, 23, red) && is(&c, 50, 23, green), "both stretches back from zero");
    // A negative stretch runs back from zero: 2 of a span of 8 left of it, 6 right.
    let c = draw(&stacked(vec![6.0, -2.0]), Orientation::Horizontal);
    assert!(is(&c, 20, 23, green) && is(&c, 100, 23, red), "either way from zero");
    // A bar with no stretches (they add up past the largest number) is drawn whole.
    let mut whole = stacked(vec![6.0, 4.0]);
    whole.stretches = vec![Vec::new()];
    let c = draw(&whole, Orientation::Horizontal);
    assert!(is(&c, 50, 23, ChartLook::default().bar_color) && is(&c, 150, 23, ChartLook::default().bar_color), "whole");
    // A key of a row a value, taller than a short box leaves the bars, draws neither: the
    // placeholder instead, nothing past the box.
    let mut long = stacked(vec![6.0, 4.0]);
    long.key = (0..8).map(|k| (format!("{k} a value with a long name to fill a row"), red)).collect();
    long.stretches = vec![vec![1.0; 8]];
    for orientation in [Orientation::Horizontal, Orientation::Vertical] {
        let mut c = Canvas::new(200, 120).unwrap();
        let look = ChartLook { orientation, ..ChartLook::default() };
        chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 60.0).unwrap(), &long, &look, INK, 1.0, false);
        assert!(is(&c, 100, 50, Rgba::rgb(0xF0, 0xF1, 0xF3)) && !is(&c, 5, 50, red), "the placeholder");
        assert!(c.pixmap().pixel(5, 90).is_some_and(|p| p.alpha() == 0), "nothing under the box");
    }
    // As columns, the head leaves the bars a line past their labels and values: 40 pt high,
    // one key row, labelled and valued, is crowded so, not across; and the commands say so as
    // the page draws it.
    let mut short = stacked(vec![6.0, 4.0]);
    short.key.truncate(1);
    short.stretches = vec![vec![10.0]];
    short.labelled = true;
    let (w, h) = (200.0 / PT_PER_MM, 40.0 / PT_PER_MM);
    let valued = |orientation| ChartLook { orientation, values: true, ..ChartLook::default() };
    assert!(chart_crowded(&short, &valued(Orientation::Vertical), w, h));
    assert!(!chart_crowded(&short, &valued(Orientation::Horizontal), w, h));
    assert!(!chart_crowded(&stacked(vec![6.0, 4.0]), &ChartLook::default(), w, 120.0 / PT_PER_MM));
    // A box narrower than a swatch draws the placeholder too, nothing past its right.
    // Sample above the centered "Chart" label so system font glyph shapes at y=60 don't mask the fill.
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 5.0, 120.0).unwrap(), &stacked(vec![6.0, 4.0]), &ChartLook::default(), INK, 1.0, false);
    assert!(is(&c, 2, 10, Rgba::rgb(0xF0, 0xF1, 0xF3)) && (6..20).all(|x| c.pixmap().pixel(x, 7).is_some_and(|p| p.alpha() == 0)));
}

fn bar_blue(c: &Canvas, x: u32, y: u32) -> bool {
    c.pixmap().pixel(x, y).is_some_and(|p| p.blue() > 150 && p.red() < 100)
}

/// Ink (dark, opaque enough) in the pixels `xs` × `ys`, read unpremultiplied: a bar's
/// half-covered edge is its own colour, not a dark one.
fn chart_inked(c: &Canvas, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> bool {
    xs.flat_map(|x| ys.clone().map(move |y| (x, y)))
        .any(|(x, y)| c.pixmap().pixel(x, y).map(|p| p.demultiply()).is_some_and(|p| p.alpha() > 80 && p.blue() < 90 && p.red() < 90))
}

/// Columns stand on the bottom, a slot each, 70 % of it wide; with no title they take its room.
#[test]
fn a_vertical_chart_draws_columns() {
    let c = chart_drawn(&[(3.0, "a"), (1.0, "b")], serde_json::json!({"orientation": "Vertical", "title": ""}));
    // Slots of 100 px, columns from 15 to 85 and 115 to 185; 3 fills the 120 px, 1 a third.
    assert!(bar_blue(&c, 50, 10) && bar_blue(&c, 50, 110), "3 fills its slot's height");
    assert!(!bar_blue(&c, 150, 60) && bar_blue(&c, 150, 100), "1 stands a third as high");
    assert!(!bar_blue(&c, 100, 110) && !bar_blue(&c, 5, 110), "gaps between and beside the columns");
    // A negative one hangs from a zero line in the ink.
    let c = chart_drawn(&[(3.0, "a"), (-1.0, "b")], serde_json::json!({"orientation": "Vertical", "title": ""}));
    assert!(bar_blue(&c, 50, 60) && !bar_blue(&c, 50, 100), "3 stands above zero at 90 px");
    assert!(bar_blue(&c, 150, 105) && !bar_blue(&c, 150, 60), "-1 hangs below it");
    // Labelled, the labels sit under the columns, centred on them.
    let c = chart_drawn(&[(3.0, "a"), (1.0, "b")], serde_json::json!({"orientation": "Vertical", "label": "name", "title": ""}));
    assert!(chart_inked(&c, 40..60, 108..120) && chart_inked(&c, 140..160, 108..120) && !bar_blue(&c, 50, 112));
}

/// Values are written past each bar's end, the bars kept short enough to leave them room;
/// a negative one's past its left end.
#[test]
fn a_charts_values_are_written_past_its_bars() {
    let plain = chart_drawn(&[(3.0, "a")], serde_json::json!({}));
    let valued = chart_drawn(&[(3.0, "a")], serde_json::json!({"values": true}));
    // One row of 18 px under a 16 px title: the bar from 18.7 to 30.6 px down.
    assert!(bar_blue(&plain, 194, 25) && !chart_inked(&plain, 150..200, 18..32));
    assert!(!bar_blue(&valued, 194, 25) && bar_blue(&valued, 150, 25) && chart_inked(&valued, 185..200, 18..32), "3 at the bar's end");
    let negative = chart_drawn(&[(3.0, "a"), (-1.0, "b")], serde_json::json!({"values": true}));
    assert!(chart_inked(&negative, 0..40, 36..50), "-1 left of its bar");
}

/// Values too wide for the chart are written smaller, leaving the bars half its width.
#[test]
fn a_charts_wide_values_leave_its_bars_room() {
    let c = chart_drawn(&[(1e12, "a"), (-1e12, "b")], serde_json::json!({"values": true}));
    // The bars of 1e12 and -1e12 take the bar area between them: at least half of the 196 px.
    let blue = (0..200).filter(|x| bar_blue(&c, *x, 25) || bar_blue(&c, *x, 43)).count();
    assert!(blue >= 97, "{blue} px of bar");
    assert!(chart_inked(&c, 150..200, 18..32), "its value beside it");
}

/// The largest number and its opposite span past a number's range: each bar still takes its
/// half of the chart, across or as columns.
#[test]
fn a_charts_bars_past_a_numbers_range_take_their_half() {
    let across = chart_drawn(&[(f64::MAX, "a"), (-f64::MAX, "b")], serde_json::json!({}));
    let row = |y: u32| (0..200).filter(|x| bar_blue(&across, *x, y)).count();
    assert!(row(25) > 50 && row(43) > 50, "{} and {} px", row(25), row(43));
    let columns = chart_drawn(&[(f64::MAX, "a"), (-f64::MAX, "b")], serde_json::json!({"orientation": "Vertical"}));
    let up = (0..120).filter(|y| bar_blue(&columns, 50, *y)).count();
    let down = (0..120).filter(|y| bar_blue(&columns, 150, *y)).count();
    assert!(up > 30 && down > 30, "{up} and {down} px");
}

/// No title gives its room to the bars; a bar colour is drawn as given.
#[test]
fn a_chart_without_a_title_and_with_a_bar_colour() {
    let titled = chart_drawn(&[(3.0, "a")], serde_json::json!({}));
    let untitled = chart_drawn(&[(3.0, "a")], serde_json::json!({"title": ""}));
    assert!(!bar_blue(&titled, 150, 8) && bar_blue(&untitled, 150, 8));
    assert!(chart_inked(&titled, 0..20, 0..16) && !chart_inked(&untitled, 0..20, 0..16), "the title, or none");
    let rose = chart_drawn(&[(3.0, "a")], serde_json::json!({"bar_color": [194, 24, 91, 255]}));
    let p = rose.pixmap().pixel(150, 25).unwrap();
    assert!(p.red() > 180 && p.green() < 40 && p.blue() > 80 && p.blue() < 100, "{p:?}");
}

/// Labels at the right of their column end against the bars; at its left, from its edge.
#[test]
fn a_charts_labels_sit_left_or_against_the_bars() {
    let left = chart_drawn(&[(3.0, "Madrid")], serde_json::json!({"label": "name"}));
    let right = chart_drawn(&[(3.0, "Madrid")], serde_json::json!({"label": "name", "label_align": "Right"}));
    // The labels' column is 64 px wide.
    assert!(chart_inked(&left, 0..6, 18..32) && !chart_inked(&left, 56..63, 18..32));
    assert!(!chart_inked(&right, 0..6, 18..32) && chart_inked(&right, 50..63, 18..32));
}

/// Mirrored (#2487 part 4), a chart is drawn as its mirror image, its text read as ever: bars
/// run from the right, their labels' column at the right, values past the bars' left ends, a
/// negative bar to the right of zero; columns hang from the top, their labels over them.
#[test]
fn a_mirrored_chart_runs_the_other_way() {
    use serde_json::json;
    let plain = chart_drawn(&[(3.0, "a"), (1.0, "b")], json!({}));
    let mirrored = chart_drawn(&[(3.0, "a"), (1.0, "b")], json!({"mirror": true}));
    // Rows of 18 px under a 16 px title. The bar area is 196 px, kept 4 px from the left when
    // mirrored; 1 takes a third of it.
    assert!(bar_blue(&plain, 20, 43) && !bar_blue(&plain, 180, 43), "1 runs right from the left");
    assert!(bar_blue(&mirrored, 180, 43) && !bar_blue(&mirrored, 20, 43), "1 runs left from the right");
    assert!(bar_blue(&mirrored, 10, 25) && !bar_blue(&mirrored, 1, 25), "3 fills the bar area");
    // Labelled, the 64 px column is at the right; the label at its outer edge, the right.
    let labelled = chart_drawn(&[(3.0, "Madrid")], json!({"label": "name", "mirror": true}));
    assert!(bar_blue(&labelled, 10, 25) && !bar_blue(&labelled, 140, 25), "the bars left of the labels");
    assert!(chart_inked(&labelled, 190..200, 18..32) && !chart_inked(&labelled, 0..40, 18..32), "the label at the right");
    // Values past the bar's left end.
    let valued = chart_drawn(&[(3.0, "a")], json!({"values": true, "mirror": true}));
    assert!(chart_inked(&valued, 0..15, 18..32) && !chart_inked(&valued, 185..200, 18..32));
    // Zero 49 px from the right: 3 runs left of it, -1 right.
    let negative = chart_drawn(&[(3.0, "a"), (-1.0, "b")], json!({"mirror": true}));
    assert!(bar_blue(&negative, 50, 25) && !bar_blue(&negative, 170, 25), "3 left of zero");
    assert!(bar_blue(&negative, 170, 43) && !bar_blue(&negative, 50, 43), "-1 right of zero");
    // Columns hang from the top: 1 a third of the 120 px down.
    let columns = chart_drawn(&[(3.0, "a"), (1.0, "b")], json!({"orientation": "Vertical", "title": "", "mirror": true}));
    assert!(bar_blue(&columns, 150, 10) && !bar_blue(&columns, 150, 100), "1 hangs from the top");
    assert!(bar_blue(&columns, 50, 110), "3 fills its slot");
    let labelled = chart_drawn(&[(3.0, "a"), (1.0, "b")], json!({"orientation": "Vertical", "title": "", "label": "name", "mirror": true}));
    assert!(chart_inked(&labelled, 40..60, 0..12) && !chart_inked(&labelled, 40..60, 108..120), "labels over the columns");
}

/// A fixed maximum is the far end of the bar area, so charts given the same one share a
/// scale; a bar past it runs to that end. A maximum not above zero isn't read.
#[test]
fn a_charts_maximum_sets_its_scale() {
    use serde_json::json;
    let own = chart_drawn(&[(50.0, "a")], json!({}));
    let shared = chart_drawn(&[(50.0, "a")], json!({"max": 100.0}));
    assert!(bar_blue(&own, 150, 25), "50 fills the 196 px on its own");
    assert!(bar_blue(&shared, 90, 25) && !bar_blue(&shared, 110, 25), "50 of 100 takes half of them");
    let past = chart_drawn(&[(150.0, "a"), (50.0, "b")], json!({"max": 100.0}));
    assert!(bar_blue(&past, 194, 25) && !bar_blue(&past, 198, 25), "150 runs to the end, no further");
    assert!(bar_blue(&past, 90, 43) && !bar_blue(&past, 110, 43), "50 still half");
    // A column of one 140 px slot, 120 px high: 50 of 100 stands half as high.
    let columns = chart_drawn(&[(50.0, "a")], json!({"orientation": "Vertical", "title": "", "max": 100.0}));
    assert!(bar_blue(&columns, 100, 100) && !bar_blue(&columns, 100, 40));
    for bad in [json!(0.0), json!(-5.0)] {
        assert!(bar_blue(&chart_drawn(&[(50.0, "a")], json!({"max": bad})), 150, 25), "{bad}");
    }
    // Bars all at zero still draw the placeholder, as they do on their own scale.
    let zeros = |extra| chart_drawn(&[(0.0, "a"), (0.0, "b")], extra).pixmap().data().to_vec();
    assert_eq!(zeros(json!({"max": 100.0})), zeros(json!({})));
}

/// A bar outline (#2487 part 4b) runs round each bar, across, mirrored and as columns, and
/// round each stretch of a stacked bar; none where it isn't given.
#[test]
fn a_charts_bar_outline_runs_round_each_bar() {
    use serde_json::json;
    let red = |c: &Canvas, x: u32, y: u32| c.pixmap().pixel(x, y).is_some_and(|p| p.red() > 150 && p.blue() < 100);
    let outline = json!({"bar_outline": {"color": [208, 16, 16, 255], "width": 2.0}});
    let plain = chart_drawn(&[(50.0, "a")], json!({}));
    assert!(!red(&plain, 100, 19) && bar_blue(&plain, 100, 25), "no outline unless given");
    let c = chart_drawn(&[(50.0, "a")], outline.clone());
    assert!(red(&c, 100, 19) && red(&c, 100, 30) && bar_blue(&c, 100, 25), "inside its top and bottom, filled within");
    let mut mirrored = outline.clone();
    mirrored["mirror"] = json!(true);
    let c = chart_drawn(&[(50.0, "a"), (25.0, "b")], mirrored);
    assert!(red(&c, 199, 25) && red(&c, 150, 37) && bar_blue(&c, 150, 43), "round the bars from the right");
    assert!(red(&c, 102, 43) && !bar_blue(&c, 50, 43), "the shorter bar's line where it ends, on the left");
    // Within each bar: a wide line paints nothing the bars leave blank, so it never runs into
    // their values, labels or title, nor past the chart's room.
    let blank = |c: &Canvas, x: u32, y: u32| c.pixmap().pixel(x, y).is_some_and(|p| p.alpha() < 16);
    for look in [json!({"values": true}), json!({"mirror": true}), json!({"orientation": "Vertical", "values": true})] {
        let vals = [(50.0, "a"), (25.0, "b"), (0.5, "c")];
        let plain = chart_drawn(&vals, look.clone());
        let mut wide = look.clone();
        wide["bar_outline"] = json!({"color": [208, 16, 16, 255], "width": 5.0});
        let c = chart_drawn(&vals, wide);
        let spilt = (0..200).flat_map(|x| (0..120).map(move |y| (x, y))).filter(|(x, y)| blank(&plain, *x, *y) && red(&c, *x, *y)).count();
        assert_eq!(spilt, 0, "{look}");
    }
    let mut columns = outline.clone();
    (columns["orientation"], columns["title"]) = (json!("Vertical"), json!(""));
    let c = chart_drawn(&[(50.0, "a")], columns);
    assert!(red(&c, 100, 0) && red(&c, 30, 60) && red(&c, 169, 60) && bar_blue(&c, 100, 60), "round the column");
    // Round each stretch: a line between the two of a bar split 6 to 4.
    let stacked = rustgis_map::layout::ChartData {
        bars: vec![(10.0, String::new())],
        key: vec![("a".into(), Rgba::rgb(0x10, 0x10, 0xD0)), ("b".into(), Rgba::rgb(0x10, 0x10, 0xD0))],
        stretches: vec![vec![6.0, 4.0]],
        ..Default::default()
    };
    let look = ChartLook {
        bar_outline: Some(rustgis_map::layout::ChartOutline { color: Rgba::rgb(0xD0, 0x10, 0x10), width: 2.0 }),
        ..ChartLook::default()
    };
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &stacked, &look, INK, 1.0, false);
    // The divider's footprint: what isn't the bars' blue where the two stretches meet. One
    // line of the outline's width, 2 px off the pixel grid touching 3, not one from each
    // stretch (5).
    let divider = (100..140).filter(|x| !bar_blue(&c, *x, 23)).count();
    assert_eq!(divider, 3, "one line of the outline's width between the stretches");
    // A see-through line is laid once everywhere: as red between the stretches as along the
    // bar's edge, not twice over on one side of the divider.
    let half_red = ChartLook {
        bar_outline: Some(rustgis_map::layout::ChartOutline { color: Rgba([208, 16, 16, 128]), width: 3.0 }),
        ..ChartLook::default()
    };
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &stacked, &half_red, INK, 1.0, false);
    let reddest = |xs: std::ops::Range<u32>, y: u32| xs.filter_map(|x| c.pixmap().pixel(x, y)).map(|p| p.red()).max().unwrap_or(0);
    let (edge, between) = (reddest(0..8, 25), reddest(100..140, 25));
    assert!(edge > 60 && between.abs_diff(edge) <= 3, "edge {edge}, between {between}");
    // Bars thinner than the line, and a stretch thinner than it, keep their colour: the line
    // narrows to a third of them rather than covering them.
    let thin = json!({"bar_outline": {"color": [208, 16, 16, 255], "width": 5.0}});
    let c = chart_drawn(&[(50.0, "a"); 20], thin);
    // Down the bars' middle, blue outweighs red at least once a bar (none when the line
    // covered them).
    let blue = (0..120).filter(|y| c.pixmap().pixel(100, *y).is_some_and(|p| p.alpha() > 0 && p.blue() > p.red())).count();
    assert!(blue >= 20, "every bar keeps its blue, {blue} px");
    let green = Rgba::rgb(0x10, 0xC0, 0x10);
    let sliver = rustgis_map::layout::ChartData {
        bars: vec![(10.0, String::new())],
        key: vec![("a".into(), Rgba::rgb(0x10, 0x10, 0xD0)), ("b".into(), green)],
        stretches: vec![vec![9.85, 0.15]],
        ..Default::default()
    };
    let look = ChartLook {
        bar_outline: Some(rustgis_map::layout::ChartOutline { color: Rgba::rgb(0xD0, 0x10, 0x10), width: 5.0 }),
        ..ChartLook::default()
    };
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &sliver, &look, INK, 1.0, false);
    // In the bar's rows, those it paints down its middle, which the key's green swatch is not in.
    let rows: Vec<u32> = (0..120).filter(|y| c.pixmap().pixel(100, *y).is_some_and(|p| p.alpha() > 0)).collect();
    assert!(rows.len() > 5, "the bar's rows, {rows:?}");
    let greens = (0..200)
        .flat_map(|x| rows.iter().map(move |y| (x, *y)))
        .filter(|(x, y)| c.pixmap().pixel(*x, *y).is_some_and(|p| p.green() > 150 && p.red() < 100))
        .count();
    assert!(greens > 0, "the thin stretch keeps its green");
    // A first stretch drawn a pixel long though its share is far shorter: the next starts
    // inside it, and the line, a third of a pixel, still stays inside the bar, between the
    // stretches and round them (a stroke that thin is drawn a pixel wide), across and as
    // columns.
    let tiny = rustgis_map::layout::ChartData { stretches: vec![vec![0.001, 9.999]], ..sliver };
    for orientation in [rustgis_map::layout::Orientation::Horizontal, rustgis_map::layout::Orientation::Vertical] {
        let mut plain = Canvas::new(200, 120).unwrap();
        let bare = ChartLook { bar_outline: None, orientation, ..look };
        chart(&mut plain, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &tiny, &bare, INK, 1.0, false);
        let mut lined = Canvas::new(200, 120).unwrap();
        chart(&mut lined, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &tiny, &ChartLook { orientation, ..look }, INK, 1.0, false);
        // Nothing where the bar itself paints nothing.
        let spilt = (0..200)
            .flat_map(|x| (0..120).map(move |y| (x, y)))
            .filter(|(x, y)| plain.pixmap().pixel(*x, *y).is_some_and(|p| p.alpha() == 0))
            .filter(|(x, y)| lined.pixmap().pixel(*x, *y).is_some_and(|p| p.alpha() > 0 && p.red() > p.green()))
            .count();
        assert_eq!(spilt, 0, "{orientation:?}: the lines round and between the stretches stay inside the bar");
    }
    // Only the bar's end next to that stretch narrows: along the rest of the bar, the edge
    // keeps its width.
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &tiny, &look, INK, 1.0, false);
    let lined = (0..120).filter(|y| c.pixmap().pixel(100, *y).is_some_and(|p| p.red() > 150 && p.blue() < 100)).count();
    assert!(lined >= 6, "both edges of the bar a few pixels wide, {lined} rows");
    // The edge is one shape: where its sides meet off the pixel grid, the shared pixel is
    // covered once, not twice in part with the bar's blue showing through between them.
    let one = rustgis_map::layout::ChartData {
        bars: vec![(10.0, String::new())],
        key: vec![("a".into(), Rgba::rgb(0x10, 0x10, 0xD0))],
        stretches: vec![vec![10.0]],
        ..Default::default()
    };
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &one, &look, INK, 1.0, false);
    // The bar's rows, below the key's swatch: those its end's line reaches.
    let red = |y: &u32| c.pixmap().pixel(2, *y).is_some_and(|p| p.red() > 100);
    let (Some(top), Some(bottom)) = ((0..120).find(red), (0..120).rev().find(red)) else { panic!("no line") };
    let seams = (top + 1..bottom).filter(|y| c.pixmap().pixel(2, *y).is_some_and(|p| p.blue() > 20)).count();
    assert!(bottom - top > 5 && seams == 0, "inside the bar's end, all line: {seams} rows show the bar");
    // Lines between twenty stretches too thin to part them overlap: a see-through colour
    // covers the overlap once, never redder than along the edge.
    let mut thin = vec![0.0005; 20];
    thin.insert(0, 5.0);
    thin.push(4.99);
    let crowded = rustgis_map::layout::ChartData {
        key: (0..thin.len()).map(|i| (i.to_string(), if i % 2 == 0 { Rgba::rgb(0x10, 0x10, 0xD0) } else { green })).collect(),
        stretches: vec![thin],
        ..one
    };
    let mut c = Canvas::new(200, 120).unwrap();
    chart(&mut c, Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap(), &crowded, &half_red, INK, 1.0, false);
    let reddest = |xs: std::ops::Range<u32>, y: u32| xs.filter_map(|x| c.pixmap().pixel(x, y)).map(|p| p.red()).max().unwrap_or(0);
    // The middle of the bar, below the key.
    let lined: Vec<u32> = (0..120).filter(|y| reddest(0..3, *y) > 60).collect();
    let row = lined.get(lined.len() / 2).copied().unwrap_or(0);
    let (edge, between) = (reddest(0..8, row), reddest(8..190, row));
    assert!(edge > 60 && between <= edge + 3, "edge {edge}, between {between}");
}

/// Hidden labels leave their column, or their row under columns, to the bars.
#[test]
fn a_charts_hidden_labels_leave_their_room_to_the_bars() {
    use serde_json::json;
    let shown = chart_drawn(&[(3.0, "Madrid")], json!({"label": "name"}));
    let hidden = chart_drawn(&[(3.0, "Madrid")], json!({"label": "name", "hide_labels": true}));
    assert!(!bar_blue(&shown, 30, 25) && chart_inked(&shown, 0..6, 18..32));
    assert!(bar_blue(&hidden, 30, 25) && !chart_inked(&hidden, 0..60, 18..32));
    let columns = chart_drawn(&[(3.0, "Madrid")], json!({"orientation": "Vertical", "title": "", "label": "name", "hide_labels": true}));
    assert!(bar_blue(&columns, 100, 115), "the column down to the bottom");
}

/// Both map frames share the index feature, even though the locator draws a different map.
/// A page using expressions draws exactly like the same page with its values written inline.
#[test]
fn series_style_follows_the_index_feature_in_both_frames() {
    use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, Value};
    use rustgis_map::layout::MapSeries;

    let mut ds = Dataset::new("Tracts", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
    for (x, id, name) in [(-0.18, 71, "West"), (0.04, 295, "East")] {
        let ring = [(x, -0.06), (x + 0.14, -0.06), (x + 0.14, 0.06), (x, 0.06), (x, -0.06)];
        ds.features.push(Feature {
            id,
            geometry: Some(Geometry::Polygon(vec![vec![ring.map(|(x, y)| Coord::new(x, y)).to_vec()]])),
            attrs: vec![Value::Text(name.into())],
        });
    }
    let mut main = Map::new("Main");
    let mut locator = Map::new("Locator");
    main.add_dataset(ds.clone());
    locator.add_dataset(ds);
    let mut maps = [main, locator];
    let mut layout = Layout::new("Atlas", 140.0, 75.0);
    let [frame, _] = std::array::from_fn(|i| {
        let m = &maps[i];
        let frame = serde_json::from_value(serde_json::json!({"MapFrame": {
            "map": m.name, "center": {"x": 0, "y": 0}, "scale": 1_000_000
        }}))
        .unwrap();
        layout.add(&m.name, (5.0 + i as f64 * 70.0, 5.0, 60.0, 65.0), frame)
    });
    let base = render(&layout, &mut maps, 72.0, &PageInfo::default()).unwrap();
    layout.series = Some(MapSeries { frame, layer: "Tracts".into(), name_field: "name".into(), margin: 0.1, page: 0 });
    let info = PageInfo::default();
    let unchanged = render(&layout, &mut maps, 72.0, &info).unwrap();
    assert_eq!(base.data(), unchanged.data());

    let style = |maps: &mut [Map], id: &str, name: &str, number: &str| {
        for (i, map) in maps.iter_mut().enumerate() {
            let layer = map.layers.first_mut().unwrap();
            let page_feature = format!("$id = {id}");
            layer.definition_query = Some(format!("{number} > 0"));
            layer.symbol_by.stroke_width = Some(format!("if({page_feature}, 3, 0.6)"));
            if i == 0 {
                layer.symbol_by.fill = Some(format!("if({page_feature}, '#8bbce8', '#e5e7eb')"));
            } else {
                // The locator keeps the two classes by a rule expression and filters by page.
                let mut renderer = rustgis_map::Renderer::unique(
                    &layer.dataset,
                    &[],
                    "name",
                    rustgis_map::Ramp::Harbour,
                    rustgis_map::Symbol::default_for(GeometryType::Polygon, 0),
                )
                .unwrap();
                let rustgis_map::Renderer::Unique { expression, .. } = &mut renderer else { panic!() };
                *expression = Some(format!("CASE WHEN {page_feature} THEN 'West' ELSE 'East' END"));
                layer.renderer = renderer;
                layer.display_filters = vec![rustgis_map::effects::DisplayFilter {
                    name: "Current tract".into(),
                    min_scale: None,
                    max_scale: None,
                    where_: page_feature.clone(),
                }];
            }
            layer.labels.enabled = true;
            layer.labels.default.where_ = Some(page_feature);
            layer.labels.default.by.text = Some(format!("{name} || ' ' || {number}"));
        }
    };
    let mut previous = base;
    for (row, id, name) in [(0, "71", "'West'"), (1, "295", "'East'")] {
        layout.series.as_mut().unwrap().page = row;
        style(&mut maps, "@page_feature_id", "@page_name", "@page_number");
        let (page, _) = render_pending(&layout, &mut maps, 72.0, &info).unwrap();
        let svg = crate::svg::layout_svg(&layout, &mut maps, 72.0, &info).unwrap().0;
        style(&mut maps, id, name, &(row + 1).to_string());
        let mut ordinary = layout.clone();
        ordinary.series = None;
        let expected = render(&ordinary, &mut maps, 72.0, &info).unwrap();
        assert!(page.data() == expected.data(), "series page {} differs from the literal expressions", row + 1);
        assert_ne!(page.data(), previous.data());
        assert_eq!(svg, crate::svg::layout_svg(&ordinary, &mut maps, 72.0, &info).unwrap().0);
        previous = page;
    }
}

/// A legend's, colorbar's, table's or chart's ink colours its text and lines (#2257). Without
/// one, they draw in dark grey as before, a table on its white cells; with one, a table has no
/// white cells, only its header and stripes in a tint of the ink.
#[test]
fn an_ink_colours_legends_colorbars_tables_and_charts() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let red = Rgba::rgb(0xE0, 0x10, 0x10);
    let reddish = |c: &Canvas| c.pixmap().pixels().iter().any(|p| p.alpha() > 200 && p.red() > 150 && p.green() < 90 && p.blue() < 90);
    let draw = |f: &dyn Fn(&mut Canvas)| {
        let mut c = Canvas::new(200, 120).unwrap();
        f(&mut c);
        c
    };
    let r = Rect::from_xywh(0.0, 0.0, 200.0, 120.0).unwrap();
    let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Double)]);
    for (name, n) in [("a", 3.0), ("b", 1.0)] {
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text(name.into()), Value::Float(n)]);
    }
    let map = Map::new("m");
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    let grey = ColorScale::Ramp { lo: 0.0, hi: 10.0, colors: vec![Rgba::rgb(0, 0, 0), Rgba::rgb(255, 255, 255)] };
    let bar = |ink| Bar {
        title: "Height",
        orientation: Orientation::Horizontal,
        ticks: 3,
        side: TickSide::End,
        reverse: false,
        comma: false,
        ink,
    };
    for (ink, expected) in [(INK, false), (red, true)] {
        let legend = draw(&|c| {
            legend_missing(c, r, &map, ("Key", 1), None, opts, ink, 1.0);
        });
        assert_eq!(reddish(&legend), expected, "legend {ink:?}");
        assert_eq!(reddish(&draw(&|c| colorbar(c, r, &grey, &bar(ink), 1.0))), expected, "colorbar {ink:?}");
        // Its bars keep their blue.
        let data = chart_kind(serde_json::json!({"field": "n", "label": "name"})).chart_data(&ds).unwrap();
        assert_eq!(reddish(&draw(&|c| chart(c, r, &data, &ChartLook::default(), ink, 1.0, false))), expected, "chart {ink:?}");
    }
    // Two columns 90 px wide, a header and two rows 17 px high: (175, 25) is in the first
    // row's empty right end, (175, 8) in the header's.
    let plain = draw(&|c| table(c, r, &ds, 5, None, 1.0));
    let inked = draw(&|c| table(c, r, &ds, 5, Some(red), 1.0));
    assert!(!reddish(&plain) && reddish(&inked));
    let at = |c: &Canvas, x, y| c.pixmap().pixel(x, y).unwrap();
    assert_eq!((at(&plain, 175, 25).red(), at(&plain, 175, 25).alpha()), (255, 255), "white cells without an ink");
    assert_eq!(at(&inked, 175, 25).alpha(), 0, "none with one");
    let header = at(&inked, 175, 8);
    assert!(header.alpha() > 0 && header.alpha() < 100 && header.green() < header.red(), "a tint of the ink: {header:?}");
}

/// No-data rows reuse the map's fallback symbol, respect filters, and never appear by
/// default. All-classes mode can key the symbol even when the current frame doesn't use it.
#[test]
fn layout_no_data_legend_follows_the_frame() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let mut map = Map::new("m");
    let mut ds = Dataset::new("tracts", GeometryType::Point, vec![Field::new("percent", FieldType::Double)]);
    for v in [Value::Float(10.0), Value::Float(50.0), Value::Null] {
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![v]);
    }
    let id = map.add_dataset(ds);
    let l = map.layer_mut(id).unwrap();
    l.renderer = rustgis_map::Renderer::graduated(
        &l.dataset,
        &[],
        "percent",
        2,
        rustgis_map::ClassMethod::EqualInterval,
        rustgis_map::Ramp::Ember,
        rustgis_map::Symbol::default_for(GeometryType::Point, 0),
    )
    .unwrap();
    let fallback = l.renderer.symbol_for(&l.dataset, 2);
    let listed = |map: &mut Map, all: bool, show: bool| {
        let drawn = (!all).then(|| complete_shown_legends(map, 1e6, show, &mut None, &Env::default()).unwrap());
        let opts = LegendOptions { show_no_data: show, hide_other: true, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
        legend_rows(map, drawn.as_deref(), &opts)
            .into_iter()
            .filter_map(|r| match r {
                LegendRow::Class(_, e) if e.label == "No data" => Some(e.symbol),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(listed(&mut map, false, false).is_empty());
    assert_eq!(listed(&mut map, false, true), vec![fallback]);
    map.layer_mut(id).unwrap().definition_query = Some("percent IS NOT NULL".into());
    assert!(listed(&mut map, false, true).is_empty());
    assert_eq!(listed(&mut map, true, true), vec![fallback]);
    let sizes = |map: &mut Map, show| {
        legend_sizes_with_no_data_mm(map, 1e6, true, true, (show, rustgis_map::layout::LEGEND_TEXT_PT), (&[], &NO_LABELS)).unwrap()[0]
    };
    assert!(sizes(&mut map, true).1 > sizes(&mut map, false).1);
}

/// Page clipping is tested after alignment and rotation; box width alone is not clipping.
/// A legend's long title is text too, and invisible elements must not generate notices.
#[test]
fn layout_text_clipping_uses_ink_and_visibility() {
    let mut l = Layout::new("Page", 120.0, 90.0);
    let id = l.add("Title", (10.0, 10.0, 5.0, 12.0), ElementKind::text("Long line", 14.0, Rgba::rgb(0, 0, 0), false));
    let info = PageInfo::default();
    assert!(clipped_text(&l, &[], 96.0, &info).is_empty(), "outside the box but on the page");
    l.element_mut(id).unwrap().x = 117.0;
    assert_eq!(clipped_text(&l, &[], 96.0, &info), vec![(id, "Title".into())]);
    l.element_mut(id).unwrap().visible = false;
    assert!(clipped_text(&l, &[], 96.0, &info).is_empty());
    let e = l.element_mut(id).unwrap();
    e.visible = true;
    (e.x, e.y, e.w) = (0.0, 40.0, 60.0);
    if let ElementKind::Text { align, rotation, letter_spacing, .. } = &mut e.kind {
        *align = TextAlign::Right;
        *letter_spacing = 2.0;
        *rotation = 90.0;
    }
    assert!(clipped_text(&l, &[], 96.0, &info).is_empty(), "turned ink fits");
    l.element_mut(id).unwrap().y = 85.0;
    assert_eq!(clipped_text(&l, &[], 96.0, &info).len(), 1, "turned ink is off-page");
    l.element_mut(id).unwrap().visible = false;
    let frame = l.add(
        "Frame",
        (10.0, 10.0, 60.0, 70.0),
        serde_json::from_value(serde_json::json!({"MapFrame":{"map":"m", "center":{"x":0,"y":0}, "scale":1000000}})).unwrap(),
    );
    let legend = l.add(
        "Legend",
        (115.0, 10.0, 5.0, 40.0),
        ElementKind::Legend {
            frame,
            title: "Long legend title".into(),
            all_classes: false,
            hide_other: false,
            show_no_data: false,
            exclude: vec![],
            columns: 1,
            labels: BTreeMap::new(),
            color: None,
            text_size: rustgis_map::layout::LEGEND_TEXT_PT,
        },
    );
    assert_eq!(clipped_text(&l, &[Map::new("m")], 96.0, &info), vec![(legend, "Legend".into())]);
    // Its title is measured at the size it is drawn at (#2581): one that fits on the page at
    // 9 points runs past its edge at 36.
    l.element_mut(legend).unwrap().x = 60.0;
    assert!(clipped_text(&l, &[Map::new("m")], 96.0, &info).is_empty(), "fits at 9 points");
    if let ElementKind::Legend { text_size, .. } = &mut l.element_mut(legend).unwrap().kind {
        *text_size = 36.0;
    }
    assert_eq!(clipped_text(&l, &[Map::new("m")], 96.0, &info), vec![(legend, "Legend".into())]);
}

/// Shortens the legend wait on this thread until dropped, then puts back what was there (a test
/// run on one thread, `--test-threads=1`, shares it with the tests after).
struct ShortLegendWait(Option<web_time::Duration>);

fn short_legend_wait(wait: web_time::Duration) -> ShortLegendWait {
    ShortLegendWait(TEST_LEGEND_WAIT.replace(Some(wait)))
}

impl Drop for ShortLegendWait {
    fn drop(&mut self) {
        TEST_LEGEND_WAIT.set(self.0);
    }
}

/// No layer labelled: every layer under its name.
static NO_LABELS: BTreeMap<String, String> = BTreeMap::new();

fn legend_page(url: &str) -> (Layout, Vec<Map>) {
    let (mut page, mut maps) = vector_page();
    // Only the legend is painted; its hidden frame still provides the map and scale.
    page.elements[0].visible = false;
    page.add(
        "Legend",
        (10.0, 20.0, 100.0, 60.0),
        ElementKind::Legend {
            frame: page.elements[0].id,
            title: String::new(),
            all_classes: true,
            hide_other: false,
            show_no_data: false,
            exclude: Vec::new(),
            columns: 1,
            labels: BTreeMap::new(),
            color: None,
            text_size: rustgis_map::layout::LEGEND_TEXT_PT,
        },
    );
    let mut tile = rustgis_map::TileLayer::new("Walls", rustgis_map::TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png");
    tile.legends = vec![rustgis_data::ogc::Legend { url: url.into(), ..Default::default() }];
    maps[0].tiles.push(tile);
    (page, maps)
}

fn red_picture() -> Vec<u8> {
    let mut image = Pixmap::new(24, 24).unwrap();
    image.fill(tiny_skia::Color::from_rgba8(220, 20, 20, 255));
    image.encode_png().unwrap()
}

#[test]
fn an_interactive_page_does_not_wait_for_legend_pictures() {
    use std::sync::mpsc;
    let url = "https://tiles.test/waits/interactive-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (page, mut maps) = legend_page(url);
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = render_pending(&page, &mut maps, 72.0, &PageInfo::default());
        tx.send((page, maps, result)).unwrap();
    });
    let done = fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    // test-wait: an interactive render must finish while the legend fetch is deliberately held.
    let result = rx.recv_timeout(std::time::Duration::from_secs(2));
    // Always release the fetch, even if the responsiveness assertion below fails.
    done(Ok(red_picture()));
    let (page, mut maps, short) = result.expect("the UI must not wait for the legend fetch");
    assert!(short.unwrap().1.legends, "the incomplete page stays pending");
    worker.join().unwrap();
    wait_for_tile_legends(&maps[0], 1e6, &[], web_time::Instant::now() + LEGEND_WAIT).unwrap();
    let (whole, pending) = render_pending(&page, &mut maps, 72.0, &PageInfo::default()).unwrap();
    assert_eq!(pending, PagePending::default(), "ready content does not keep the page pending");
    assert!(whole.pixels().iter().any(|p| p.red() > 200 && p.green() < 60), "the completed picture is painted");
    let exported = render(&page, &mut maps, 72.0, &PageInfo::default()).unwrap();
    assert_eq!(whole.data(), exported.data(), "ready interactive and complete rendering agree");
}

#[test]
fn a_page_waits_for_its_legends_only_until_its_deadline() {
    let url = "https://tiles.test/waits/deadline-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (_, maps) = legend_page(url);
    // A deadline already passed ends the wait at once, with the picture still on its way.
    assert_eq!(wait_for_tile_legends(&maps[0], 1e6, &[], web_time::Instant::now()), Some(()));
    assert!(tile_legends_pending(&maps[0], 1e6, &[]));
    fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap()(Ok(red_picture()));
}

#[test]
fn an_export_shares_one_legend_deadline_across_its_calls() {
    assert!(LEGEND_BUDGET.get().is_none());
    {
        let _export = legend_budget();
        let first = legend_deadline();
        {
            let _inner = legend_budget();
            assert_eq!(legend_deadline(), first, "a call inside the export keeps its deadline");
        }
        assert_eq!(legend_deadline(), first, "the inner call didn't end the export's budget");
    }
    assert!(LEGEND_BUDGET.get().is_none(), "the budget ends with the export");
    let _next = legend_budget();
    assert!(LEGEND_BUDGET.get() == Some(None), "the next export starts afresh");
}

#[test]
fn a_legend_whose_picture_is_ready_does_not_start_the_exports_legend_clock() {
    let url = "https://tiles.test/waits/ready-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (page, mut maps) = legend_page(url);
    assert!(tile_legends_pending(&maps[0], 1e6, &[]), "the picture is asked for");
    fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap()(Ok(red_picture()));
    wait_for_tile_legends(&maps[0], 1e6, &[], web_time::Instant::now() + LEGEND_WAIT).unwrap();
    assert!(!tile_legends_pending(&maps[0], 1e6, &[]), "the picture has landed");
    let _export = legend_budget();
    render_drawing_inner(&page, &mut maps, 72.0, &PageInfo::default(), &mut None).unwrap();
    assert!(LEGEND_BUDGET.get() == Some(None), "a legend with nothing to wait for leaves the clock unstarted");
    assert!(finish_legends().is_ok());
    // A later legend whose picture is first asked for now still gets the whole wait.
    let before = web_time::Instant::now();
    assert!(legend_deadline() >= before + LEGEND_WAIT);
}

#[test]
fn an_svg_export_gives_up_on_held_legends_at_one_deadline() {
    let url = "https://tiles.test/waits/svg-deadline-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (mut page, mut maps) = legend_page(url);
    let frame = page.elements[0].id;
    page.add(
        "Second legend",
        (10.0, 5.0, 100.0, 10.0),
        ElementKind::Legend {
            frame,
            title: String::new(),
            all_classes: true,
            hide_other: false,
            show_no_data: false,
            exclude: Vec::new(),
            columns: 1,
            labels: BTreeMap::new(),
            color: None,
            text_size: rustgis_map::layout::LEGEND_TEXT_PT,
        },
    );
    // The thread-local budget and the shortened wait belong to this worker alone.
    let worker = std::thread::spawn(move || {
        TEST_LEGEND_WAIT.set(Some(web_time::Duration::from_millis(50)));
        crate::svg::layout_svg(&page, &mut maps, 72.0, &PageInfo::default()).map(|_| ())
    });
    let done = fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let result = worker.join().unwrap();
    done(Err("test fetch released".into()));
    assert!(result.unwrap_err().contains("still on their way"), "no successful SVG without its legend");
}

#[test]
fn an_export_drawn_without_a_legend_picture_stays_refused_when_the_picture_lands_later() {
    let url = "https://tiles.test/waits/late-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (page, mut maps) = legend_page(url);
    // A wait that has ended before it begins: the legend is drawn with its picture missing.
    let _short = short_legend_wait(web_time::Duration::ZERO);
    let _export = legend_budget();
    render_drawing_inner(&page, &mut maps, 72.0, &PageInfo::default(), &mut None).unwrap();
    fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap()(Ok(red_picture()));
    wait_for_tile_legends(&maps[0], 1e6, &[], web_time::Instant::now() + LEGEND_WAIT).unwrap();
    assert!(!tile_legends_pending(&maps[0], 1e6, &[]), "the picture has landed");
    // What was drawn still lacks it: landing later doesn't make the export complete.
    assert!(finish_legends().unwrap_err().contains("still on their way"));
}

#[test]
fn an_off_page_legend_does_not_hold_any_export_up() {
    let url = "https://tiles.test/waits/off-page-legend.png";
    let _held = crate::tiles::tests::hold_fetch(url);
    let (mut page, mut maps) = legend_page(url);
    let legend = page.elements.last().map(|e| e.id).unwrap();
    page.element_mut(legend).unwrap().x = 1000.0;
    // No wait at all: were the legend's picture waited for or counted missing, each fails.
    let _short = short_legend_wait(web_time::Duration::ZERO);
    let info = PageInfo::default();
    // Drawn nowhere, its picture is neither waited for nor missing from the file.
    render(&page, &mut maps, 72.0, &info).unwrap();
    render_drawing(&page, &mut maps, 72.0, &info).unwrap();
    crate::svg::layout_svg(&page, &mut maps, 72.0, &info).unwrap();
}

#[test]
fn a_legend_says_from_the_lookup_that_draws_it_whether_a_picture_is_missing() {
    let url = "https://tiles.test/waits/lookup-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (_, maps) = legend_page(url);
    let opts = || LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    let mut canvas = Canvas::new(200, 200).unwrap();
    let draw = |canvas: &mut Canvas| {
        legend_missing(canvas, Rect::from_xywh(0.0, 0.0, 200.0, 200.0).unwrap(), &maps[0], ("", 1), None, opts(), INK, 1.0)
    };
    assert!(draw(&mut canvas), "drawn while its picture is on its way");
    fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap()(Ok(red_picture()));
    wait_for_tile_legends(&maps[0], 1e6, &[], web_time::Instant::now() + LEGEND_WAIT).unwrap();
    assert!(!draw(&mut canvas), "drawn with it");
}

#[test]
fn only_an_on_page_legend_is_waited_for_by_the_redraw() {
    let url = "https://tiles.test/waits/redraw-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (mut page, maps) = legend_page(url);
    let legend = page.elements.last().map(|e| e.id).unwrap();
    page.element_mut(legend).unwrap().x = 1000.0;
    assert!(!legends_pending(&page, &maps), "off the page: not waited for");
    assert!(fetch.try_recv().is_err(), "nor is its picture asked for");
    page.element_mut(legend).unwrap().x = 10.0;
    assert!(legends_pending(&page, &maps), "on the page, its picture is on its way");
    fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap()(Ok(red_picture()));
}

#[test]
fn the_drawing_and_the_redraw_gate_agree_on_a_legend_at_the_pages_edge() {
    // The page is 120 mm wide: 0.01 mm of the legend on it counts; none does not, at whatever
    // resolution the page is drawn (a sub-pixel sliver must not make them disagree).
    for (x, on_page) in [(119.99, true), (120.0, false)] {
        let url = format!("https://tiles.test/waits/edge-legend-{x}.png");
        let fetch = crate::tiles::tests::hold_fetch(&url);
        let (mut page, mut maps) = legend_page(&url);
        let legend = page.elements.last().map(|e| e.id).unwrap();
        page.element_mut(legend).unwrap().x = x;
        assert_eq!(legends_pending(&page, &maps), on_page, "the redraw gate at x = {x}");
        let _short = short_legend_wait(web_time::Duration::ZERO);
        let _export = legend_budget();
        render_drawing_inner(&page, &mut maps, 72.0, &PageInfo::default(), &mut None).unwrap();
        assert_eq!(finish_legends().is_err(), on_page, "the page's drawing at x = {x}");
        if on_page {
            fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap()(Ok(red_picture()));
        }
    }
}

#[test]
fn an_export_includes_a_legend_that_arrives_while_it_waits() {
    let url = "https://tiles.test/waits/export-legend.png";
    let fetch = crate::tiles::tests::hold_fetch(url);
    let (page, mut maps) = legend_page(url);
    let worker = std::thread::spawn(move || render(&page, &mut maps, 72.0, &PageInfo::default()));
    let done = fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(!worker.is_finished(), "a complete export waits for the held picture");
    done(Ok(red_picture()));
    let image = worker.join().unwrap().unwrap();
    assert!(image.pixels().iter().any(|p| p.red() > 200 && p.green() < 60), "not a successful page without its picture");
}

#[test]
fn cancelled_legend_waits_return_no_export() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    struct Probe {
        flag: Arc<AtomicBool>,
        waiting: mpsc::SyncSender<()>,
    }
    impl rustgis_data::progress::Probe for Probe {
        fn cancelled(&self) -> bool {
            let _ = self.waiting.try_send(());
            self.flag.load(Ordering::Relaxed)
        }
        fn begin(&self, _: u64) {}
        fn advance(&self, _: u64) {}
    }
    for kind in 0..4 {
        let url = format!("https://tiles.test/waits/cancelled-legend-{kind}.png");
        let fetch = crate::tiles::tests::hold_fetch(&url);
        let (page, mut maps) = legend_page(&url);
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = flag.clone();
        let (waiting, observed) = mpsc::sync_channel(1);
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let probe_flag = if kind == 3 { Arc::new(AtomicBool::new(false)) } else { flag.clone() };
            let _probe = rustgis_data::progress::install(Some(Arc::new(Probe { flag: probe_flag, waiting })));
            let _frame = (kind == 3).then(|| crate::cancel::install(flag));
            let info = PageInfo::default();
            let result = match kind {
                0 | 3 => render(&page, &mut maps, 72.0, &info).map(|_| ()),
                1 => render_drawing(&page, &mut maps, 72.0, &info).map(|_| ()),
                _ => crate::svg::layout_svg(&page, &mut maps, 72.0, &info).map(|_| ()),
            };
            tx.send(result).unwrap();
        });
        let done = fetch.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        observed.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        cancel.store(true, Ordering::Relaxed);
        // test-wait: cancellation must finish before the deliberately pending fetch is released.
        let result = rx.recv_timeout(std::time::Duration::from_secs(2));
        done(Err("test fetch released".into()));
        assert_eq!(result.unwrap(), Err(rustgis_data::DataError::Cancelled.to_string()), "format {kind}");
        worker.join().unwrap();
    }
}

/// A frame drawn over another takes its rectangle out of the other's: the bands above and below
/// it and the strips beside it are left, a strip too narrow to point into is dropped, and one
/// that covers it all leaves nothing (#1191).
#[test]
fn a_frame_drawn_over_another_leaves_the_rest_of_it() {
    let r = [0.0, 0.0, 100.0, 50.0];
    assert_eq!(uncovered(r, [200.0, 0.0, 10.0, 10.0]), [r], "apart");
    assert_eq!(
        uncovered(r, [10.0, 10.0, 20.0, 20.0]),
        [[0.0, 0.0, 100.0, 10.0], [0.0, 30.0, 100.0, 20.0], [0.0, 10.0, 10.0, 20.0], [30.0, 10.0, 70.0, 20.0]]
    );
    // Over its top-left corner, and past it.
    assert_eq!(uncovered(r, [-5.0, -5.0, 15.0, 15.0]), [[0.0, 10.0, 100.0, 40.0], [10.0, 0.0, 90.0, 10.0]]);
    assert_eq!(uncovered(r, [0.0, 0.0, 100.0, 49.995]), Vec::<[f64; 4]>::new(), "a 0.005 mm strip is dropped");
    assert!(uncovered(r, [-1.0, -1.0, 102.0, 52.0]).is_empty());
}

/// Each element is a layer of the page's recording, named after it and identified by its id
/// (#1815): two elements named alike are told apart by id, not by which comes first, so an
/// element that draws nothing on one page of a series doesn't hand its layer to the other.
#[test]
fn each_element_is_a_layer_with_its_id() {
    use crate::vector::Item;
    let mut layout = Layout::new("Layers", 120.0, 90.0);
    let text = |t: &str| ElementKind::text(t, 12.0, Rgba::rgb(0, 0, 0), false);
    let (a, b) = (layout.add("Texto", (10.0, 10.0, 60.0, 10.0), text("Um")), layout.add("Texto", (10.0, 30.0, 60.0, 10.0), text("Dois")));
    let layers = |layout: &Layout| {
        let (drawing, _) = render_drawing(layout, &mut [], 96.0, &PageInfo::default()).unwrap();
        let (_, _, items) = drawing.finish();
        items.into_iter().filter_map(|i| if let Item::Group(g) = i { Some((g.layer?, g.layer_id?)) } else { None }).collect::<Vec<_>>()
    };
    assert_eq!(layers(&layout), [("Texto".to_string(), u64::from(a)), ("Texto".to_string(), u64::from(b))]);
    // The first draws nothing: the second keeps its own layer.
    if let Some(ElementKind::Text { text, .. }) = layout.elements.iter_mut().find(|e| e.id == a).map(|e| &mut e.kind) {
        text.clear();
    }
    assert_eq!(layers(&layout), [("Texto".to_string(), u64::from(b))]);
}

/// The page's colour (#2855) is the first thing the PDF's drawing fills, over the whole page;
/// a white page is filled white as before.
#[test]
fn a_pages_colour_is_filled_first_in_the_pdfs_drawing() {
    use crate::vector::{Brush, Item};
    let mut layout = Layout::new("Paper", 120.0, 90.0);
    let first = |layout: &Layout| {
        let (drawing, _) = render_drawing(layout, &mut [], 72.0, &PageInfo::default()).unwrap();
        let (w, h, items) = drawing.finish();
        match items.into_iter().next() {
            Some(Item::Fill { path, brush: Brush::Solid(c), .. }) => {
                let b = path.bounds();
                assert!(b.left() <= 0.0 && b.top() <= 0.0 && b.right() >= w - 0.5 && b.bottom() >= h - 0.5, "{b:?}");
                c.to_color_u8()
            }
            _ => panic!("the page is not filled first"),
        }
    };
    let c = first(&layout);
    assert_eq!((c.red(), c.green(), c.blue(), c.alpha()), (255, 255, 255, 255));
    layout.paper = Some(Rgba::rgb(0x0B, 0x1B, 0x2B));
    let c = first(&layout);
    assert_eq!((c.red(), c.green(), c.blue(), c.alpha()), (0x0B, 0x1B, 0x2B, 255));
}

/// A page with a frame of the built-in world and a title.
fn vector_page() -> (Layout, Vec<Map>) {
    let mut layout = Layout::new("Vectors", 120.0, 90.0);
    let kind = serde_json::from_value(serde_json::json!({"MapFrame": {
        "map": "Map", "center": {"x": 0, "y": 0}, "scale": 150000000
    }}))
    .unwrap();
    layout.add("World", (10.0, 20.0, 100.0, 60.0), kind);
    let title = ElementKind::text("Shelby County", 18.0, Rgba::rgb(20, 20, 20), true);
    layout.add("Title", (10.0, 4.0, 100.0, 12.0), title);
    (layout, vec![Map::new("Map")])
}

#[test]
fn a_recorded_page_keeps_its_text_and_frame_as_vectors() {
    use crate::vector::Item;
    let (layout, mut maps) = vector_page();
    let (drawing, pictured) = render_drawing(&layout, &mut maps, 150.0, &PageInfo::default()).unwrap();
    assert!(pictured.is_empty() && !drawing.overflowed());
    let (_, _, items) = drawing.finish();
    let items = crate::vector::without_layers(items);
    let text: String = items
        .iter()
        .filter_map(|i| if let Item::Glyphs(r) = i { Some(r) } else { None })
        .flat_map(|r| r.glyphs.iter().map(|g| g.ch))
        .collect();
    assert_eq!(text, "Shelby County", "the title, its space kept for copying");
    // The frame: a group cut to its box, its land as paths, not a picture of it.
    let frame = items.iter().find_map(|i| if let Item::Group(g) = i { Some(g) } else { None }).expect("the frame's group");
    assert!(frame.clip.is_some());
    assert!(frame.items.iter().filter(|i| matches!(i, Item::Fill { .. })).count() > 50);
    assert!(!frame.items.iter().any(|i| matches!(i, Item::Image(_))));
}

#[test]
fn a_recorded_page_draws_what_the_picture_of_it_shows() {
    let (layout, mut maps) = vector_page();
    let info = PageInfo::default();
    let png = render(&layout, &mut maps, 96.0, &info).unwrap();
    let (drawing, _) = render_drawing(&layout, &mut maps, 96.0, &info).unwrap();
    let (_, _, items) = drawing.finish();
    let mut replayed = Pixmap::new(png.width(), png.height()).unwrap();
    crate::vector::replay(&items, &mut replayed, Transform::identity());
    // The same paths through the same rasteriser: only glyph edges (coverage against filled
    // outlines) may differ.
    let differ = png.pixels().iter().zip(replayed.pixels()).filter(|(a, b)| {
        let (a, b) = (a.demultiply(), b.demultiply());
        a.red().abs_diff(b.red()).max(a.green().abs_diff(b.green())).max(a.blue().abs_diff(b.blue())) > 48
    });
    let share = differ.count() as f64 / png.pixels().len() as f64;
    assert!(share < 0.005, "{:.3}% of pixels differ", share * 100.0);
}

#[test]
fn a_huge_page_records_within_its_bounds() {
    let mut layout = Layout::new("Huge", 5000.0, 5000.0);
    layout.add("Title", (10.0, 10.0, 4000.0, 400.0), ElementKind::text("Shelby County", 300.0, Rgba::rgb(0, 0, 0), true));
    let (drawing, _) = render_drawing(&layout, &mut [], 600.0, &PageInfo::default()).unwrap();
    // Held to the page budget's size, as the picture of it is.
    assert!(drawing.width as f64 <= MAX_SIDE && drawing.height as f64 <= MAX_SIDE);
    assert!(!drawing.overflowed());
}

#[test]
fn a_turned_frame_stays_vectors() {
    use crate::vector::Item;
    let (mut layout, mut maps) = vector_page();
    if let Some(ElementKind::MapFrame { bearing, .. }) = layout.elements.first_mut().map(|e| &mut e.kind) {
        *bearing = 30.0;
    }
    let (drawing, _) = render_drawing(&layout, &mut maps, 96.0, &PageInfo::default()).unwrap();
    let (_, _, items) = drawing.finish();
    let items = crate::vector::without_layers(items);
    let frame = items.iter().find_map(|i| if let Item::Group(g) = i { Some(g) } else { None }).expect("the frame's group");
    // The turned view holds the north-up frame, turned.
    let Some(Item::Group(north)) = frame.items.first() else { panic!("the north-up frame inside") };
    assert!(north.transform.ky.abs() > 0.3, "turned: {:?}", north.transform);
    assert!(north.items.iter().filter(|i| matches!(i, Item::Fill { .. })).count() > 50);
}

#[test]
fn a_frame_too_large_for_vectors_is_drawn_as_a_picture_and_named() {
    use crate::vector::{Item, TEST_MAX_COST};
    let (layout, mut maps) = vector_page();
    TEST_MAX_COST.with(|l| l.set(Some(20_000)));
    let result = render_drawing(&layout, &mut maps, 72.0, &PageInfo::default());
    TEST_MAX_COST.with(|l| l.set(None));
    let (drawing, pictured) = result.unwrap();
    assert_eq!(pictured.iter().map(|p| p.1.as_str()).collect::<Vec<_>>(), ["World"]);
    let (_, _, items) = drawing.finish();
    let items = crate::vector::without_layers(items);
    assert!(items.iter().any(|i| matches!(i, Item::Image(_))), "the frame as a picture");
}

/// Every token the Format pane offers a layout's text is one the page expands (#516).
#[test]
fn the_tokens_offered_are_the_ones_expanded() {
    let layout = Layout::new("Atlas", 100.0, 100.0);
    let info = PageInfo { page: 2, pages: 5, page_name: "North".into() };
    let tokens = rustgis_map::layout::PAGE_TOKENS.iter().chain(rustgis_map::graphics::TOKENS);
    for (token, _) in tokens {
        assert!(!expand(token, &layout, &info, None).contains('{'), "{token}");
    }
    assert_eq!(expand("{page} of {pages}, {pageName} · {layout}", &layout, &info, None), "2 of 5, North · Atlas");
}

/// A WMS or WMTS layer's legend is its service's picture under its name, as the Layers pane
/// shows it, narrowed to fit across and sized for; left out by name, hidden, or with none at
/// the frame's scale, it isn't listed.
#[test]
fn a_web_layers_legend_is_its_services_picture() {
    crate::tiles::tests::serve();
    let mut map = Map::new("m");
    let mut t = rustgis_map::TileLayer::new("Walls", rustgis_map::TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png");
    t.legends = vec![rustgis_data::ogc::Legend { url: "https://tiles.test/red/legend.png".into(), ..Default::default() }];
    map.tiles.push(t);
    // The red drawn (the picture is red; the name is dark) and how far right, in pixels.
    let red = |map: &Map, width: f32, exclude: &[String], scale: f64| {
        let mut pm = Pixmap::new(300, 400).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { show_no_data: false, hide_other: false, exclude, labels: &NO_LABELS, scale, size: 1.0 };
        crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, width, 400.0).unwrap(), map, ("", 1), None, opts, 1.0));
        let hits: Vec<u32> = (0..pm.height())
            .flat_map(|y| (0..pm.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| pm.pixel(x, y).is_some_and(|p| p.red() > 200 && p.green() < 60))
            .map(|(x, _)| x)
            .collect();
        (hits.len(), hits.iter().max().copied().unwrap_or(0))
    };
    wait_for_tile_legends(&map, 1e6, &[], web_time::Instant::now() + LEGEND_WAIT).unwrap();
    let (ink, right) = red(&map, 300.0, &[], 1e6);
    assert!(ink > 200 * 200, "the picture is drawn: {ink} px");
    assert!(right < 6 + 256, "a pixel a point: {right}");
    // Its name's row and the picture, 256 points down and 6 + 256 across.
    let (w, h) = legend_sizes_mm(&mut map, 1e6, false, false, (&[], &NO_LABELS)).unwrap()[0];
    let mm = 25.4 / 96.0;
    assert!((w - 262.0 * mm).abs() < 1e-6 && (h - (f64::from(LEGEND_ROW) + 256.0) * mm).abs() < 1e-6, "{w} × {h} mm");
    let (_, right) = red(&map, 100.0, &[], 1e6);
    assert!(right < 100, "narrowed to fit: {right}");
    assert_eq!(red(&map, 300.0, &["WALLS".into()], 1e6).0, 0, "left out by name");
    map.tiles[0].legends[0].max_scale = Some(1e5);
    assert_eq!(red(&map, 300.0, &[], 1e6).0, 0, "none at this scale");
    map.tiles[0].legends[0].max_scale = None;
    // Narrowed, a picture takes only the height it's drawn at: a second layer's fits under it
    // (a 70-point legend: 64 points across, so 64 down), and the drawing order is the map's.
    let mut second = map.tiles[0].clone();
    second.name = "Floors".into();
    map.tiles.push(second);
    let mut pm = Pixmap::new(70, 200).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 70.0, 200.0).unwrap(), &map, ("", 1), None, opts, 1.0));
    let red_rows =
        (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(|p| p.red() > 200 && p.green() < 60))).count();
    assert!(red_rows >= 2 * 63, "both pictures, {red_rows} rows");
    let names: Vec<&str> = legend_rows(&map, None, &opts)
        .iter()
        .filter_map(|r| match r {
            LegendRow::Head(n) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["Walls", "Floors"]);
    // Among the map's layers where the map draws them: a web layer placed over a feature
    // layer is listed just above it, under the feature layers above.
    let lower = map.add_dataset(rustgis_data::Dataset::new("Lower", GeometryType::Point, vec![]));
    map.add_dataset(rustgis_data::Dataset::new("Upper", GeometryType::Point, vec![]));
    map.tiles[1].over = Some(lower);
    let listed: Vec<&str> = legend_rows(&map, None, &opts)
        .iter()
        .filter_map(|r| match r {
            LegendRow::Head(n) => Some(*n),
            LegendRow::Plain(_, name) => Some(*name),
            _ => None,
        })
        .collect();
    let feature_order: Vec<&str> = map.layers.iter().map(|l| l.name.as_str()).collect();
    let expected: Vec<&str> =
        feature_order.iter().flat_map(|n| if *n == "Lower" { vec!["Floors", "Lower"] } else { vec![*n] }).chain(["Walls"]).collect();
    assert_eq!(listed, expected, "layers top first: {feature_order:?}");
    // A heading with no room for any of its picture under it isn't drawn: a legend one row
    // tall stays blank (the map's layers set aside, whose rows would fit).
    map.layers.clear();
    let mut pm = Pixmap::new(300, 15).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    crate::canvas::with_canvas(&mut pm, |c| {
        legend(c, Rect::from_xywh(0.0, 0.0, 300.0, LEGEND_ROW).unwrap(), &map, ("", 1), None, opts, 1.0)
    });
    assert!(pm.pixels().iter().all(|p| p.red() > 200 && p.green() > 200), "no heading alone");
    map.tiles.truncate(1);
    map.tiles[0].visible = false;
    assert_eq!(red(&map, 300.0, &[], 1e6).0, 0, "hidden");
}

/// Plain under a rectangle is nothing drawn there but the background: the basemap's paper, not a
/// layer or its label, not an outline that coverage takes as context, and never with web tiles
/// (#1379).
#[test]
fn frame_plain_under_counts_everything_drawn_but_the_background() {
    use rustgis_data::{Dataset, Geometry};
    let square = |name: &str, half: f64| {
        let mut d = Dataset::new(name, GeometryType::Polygon, Vec::new());
        let ring = [(-half, -half), (half, -half), (half, half), (-half, half), (-half, -half)];
        d.push(Some(Geometry::Polygon(vec![vec![ring.map(|(x, y)| Coord::new(x, y)).to_vec()]])), Vec::new());
        d
    };
    let mut map = Map::new("m");
    (map.basemap, map.graticule) = (rustgis_map::Basemap::Blank, false);
    map.add_dataset(square("data", 0.1));
    // A 100 mm frame at 1:1,000,000 shows about 0.9° across: the square sits in its middle.
    let plain = |map: &mut Map, under| frame_plain_under(map, Coord::new(0.0, 0.0), 1_000_000.0, (100.0, 100.0), under, 128);
    assert!(plain(&mut map, (2.0, 70.0, 25.0, 25.0)), "a corner is the blank paper");
    // A paper colour (#2532) is the background too, and coverage still finds only the data.
    let white = frame_coverage(&mut map, Coord::new(0.0, 0.0), 1_000_000.0, 100.0, 100.0, 64);
    map.paper = Some(Rgba::rgb(0xF1, 0xEA, 0xDF));
    assert!(plain(&mut map, (2.0, 70.0, 25.0, 25.0)), "a corner is the coloured paper");
    assert_eq!(frame_coverage(&mut map, Coord::new(0.0, 0.0), 1_000_000.0, 100.0, 100.0, 64), white);
    assert_eq!(map.paper, Some(Rgba::rgb(0xF1, 0xEA, 0xDF)), "put back after coverage");
    map.paper = None;
    assert!(!plain(&mut map, (40.0, 40.0, 20.0, 20.0)), "the data in the middle");
    assert!(!plain(&mut map, (48.0, 48.0, 4.0, 4.0)), "one colour all over, but the data's, not the background");
    map.graticule = true;
    assert!(!plain(&mut map, (2.0, 2.0, 96.0, 96.0)), "the map's own graticule is drawn in the frame too");
    map.graticule = false;
    let border = map.add_dataset(square("border", 0.3));
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == border) {
        let outline = rustgis_map::Symbol::default_for(GeometryType::Polygon, 0);
        l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol { fill: Rgba::new(0, 0, 0, 0), ..outline });
    }
    assert!(plain(&mut map, (2.0, 86.0, 12.0, 12.0)), "the outline (17 to 83 mm) runs clear of that corner");
    assert!(!plain(&mut map, (2.0, 2.0, 96.0, 96.0)), "an outline coverage leaves out still counts here");
    // A point left of the rectangle, its label running into it: the label counts.
    let mut d = Dataset::new("named", GeometryType::Point, vec![rustgis_data::Field::new("name", rustgis_data::FieldType::Text)]);
    d.push(Some(Geometry::Point(Coord::new(-0.2, -0.25))), vec![rustgis_data::Value::Text("A long place name".into())]);
    let named = map.add_dataset(d);
    let right_of_it = (31.0, 63.0, 67.0, 33.0);
    map.layer_mut(border).unwrap().visible = false;
    assert!(plain(&mut map, right_of_it), "the point itself lies outside it");
    map.layer_mut(named).unwrap().labels.enabled = true;
    assert!(!plain(&mut map, right_of_it), "its label lies inside it");
    map.layer_mut(named).unwrap().labels.enabled = false;
    // On another body the background is drawn in the land's grey (Map::palette), not the
    // Earth basemap's water: that grey is the background there.
    (map.body, map.basemap) = ("mars".into(), rustgis_map::Basemap::Paper);
    assert!(plain(&mut map, (2.0, 86.0, 12.0, 12.0)), "Mars's plain grey is its background");
    (map.body, map.basemap) = ("earth".into(), rustgis_map::Basemap::Blank);
    map.tiles.push(rustgis_map::TileLayer::new("tiles", rustgis_map::TileKind::Raster, "https://example.invalid/{z}/{x}/{y}.png"));
    assert!(!plain(&mut map, (2.0, 86.0, 12.0, 12.0)), "web tiles aren't fetched for it, so nothing is plain");
}

#[test]
fn plain_dark_basemaps_keep_the_legend_paper_while_light_ones_clear_it() {
    for body in ["earth", "mars"] {
        for basemap in [rustgis_map::Basemap::Midnight, rustgis_map::Basemap::Paper, rustgis_map::Basemap::Ocean] {
            let mut map = Map::new("plain");
            (map.body, map.basemap, map.graticule) = (body.into(), basemap, false);
            // The Earth frame is open Gulf of Guinea water; on Mars it is the palette's grey.
            let plain = frame_plain_under(&mut map, Coord::new(0.0, 0.0), 1_000_000.0, (100.0, 100.0), (40.0, 40.0, 20.0, 20.0), 128);
            assert_eq!(plain, basemap != rustgis_map::Basemap::Midnight, "{body}: {basemap:?}");
        }
    }
}

/// A layer that only outlines its polygons isn't counted as drawn, unless nothing else is shown,
/// and it is shown again after; one with labels is more than an outline.
#[test]
fn frame_coverage_leaves_out_layers_that_only_outline() {
    use rustgis_data::{Dataset, Geometry};
    let count = |map: &mut Map| {
        let (.., cells) = frame_coverage(map, Coord::new(0.0, 0.0), 1_000_000.0, 100.0, 100.0, 64).unwrap();
        cells.iter().filter(|c| **c).count()
    };
    let point = || {
        let mut d = Dataset::new("far", GeometryType::Point, Vec::new());
        d.push(Some(Geometry::Point(Coord::new(-0.3, -0.3))), Vec::new());
        d
    };
    let mut map = Map::new("m");
    let mut square = Dataset::new("border", GeometryType::Polygon, Vec::new());
    square.push(
        Some(Geometry::Polygon(vec![vec![
            [(-0.2, -0.2), (0.2, -0.2), (0.2, 0.2), (-0.2, 0.2), (-0.2, -0.2)].map(|(x, y)| Coord::new(x, y)).to_vec(),
        ]])),
        Vec::new(),
    );
    let border = map.add_dataset(square);
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == border) {
        let outline = rustgis_map::Symbol::default_for(GeometryType::Polygon, 0);
        l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol { fill: Rgba::new(0, 0, 0, 0), ..outline });
    }
    assert!(count(&mut map) > 0, "alone, it is what the map shows");
    map.add_dataset(point());
    let beside = count(&mut map);
    assert!(map.layer(border).is_some_and(|l| l.visible), "shown again");
    let mut alone = Map::new("m");
    alone.add_dataset(point());
    assert_eq!(beside, count(&mut alone), "only the point counts");
    let l = map.layers.iter_mut().find(|l| l.id == border).unwrap();
    assert!(only_outlines(l));
    l.labels.enabled = true;
    assert!(!only_outlines(l), "labels make it more than an outline");
    l.labels.enabled = false;
    l.fill_layers = vec![rustgis_map::fills::FillLayer::default()];
    assert!(!only_outlines(l), "a stacked fill draws inside it");
    l.no_fill = Some(rustgis_map::NoFill::Symbols);
    assert!(!only_outlines(l), "No Fill's symbols leave a stacked fill drawn");
    l.no_fill = Some(rustgis_map::NoFill::Override);
    assert!(only_outlines(l), "No Fill's override is the fill that hides the rest");
    l.no_fill = None;
    l.fill_layers.clear();
    l.symbol_by.fill = Some("'#ff0000'".into());
    assert!(!only_outlines(l), "a fill by expression draws inside it");
    l.symbol_by.fill = None;
    l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(GeometryType::Polygon, 0));
    assert!(!only_outlines(l), "a filled symbol");
    l.no_fill = Some(rustgis_map::NoFill::Symbols);
    assert!(only_outlines(l), "No Fill counts as no fill");
    let symbol = rustgis_map::Symbol::default_for(GeometryType::Polygon, 0);
    l.renderer = rustgis_map::Renderer::Unique {
        field: "g".into(),
        classes: vec![("a".into(), symbol)],
        other: symbol,
        ramp: rustgis_map::Ramp::Ember,
        class_labels: Default::default(),
        other_label: None,
        class_strokes: Default::default(),
        other_strokes: Vec::new(),
        class_fills: [("a".to_string(), vec![rustgis_map::fills::FillLayer::default()])].into(),
        other_fills: Vec::new(),
        expression: None,
    };
    assert!(!only_outlines(l), "No Fill's symbols leave a class's stacked fill drawn");
    l.no_fill = Some(rustgis_map::NoFill::Override);
    assert!(only_outlines(l), "the override hides that too");
    l.no_fill = None;
    l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
        fill: Rgba::new(0, 0, 0, 0),
        ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0)
    });

    // A layer out of scale at the frame's scale draws nothing there: the outline is all it shows.
    let drawn_alone = {
        let far = map.layers.iter().find(|l| l.id != border).map(|l| l.id).unwrap();
        map.layers.iter_mut().find(|l| l.id == far).unwrap().min_scale = Some(1.0);
        let shown = count(&mut map);
        map.layers.iter_mut().find(|l| l.id == far).unwrap().min_scale = None;
        shown
    };
    assert!(drawn_alone > beside, "{drawn_alone} of {beside}");
    let l = map.layers.iter_mut().find(|l| l.id == border).unwrap();
    l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(GeometryType::Polygon, 0));
    assert!(plain_fill(l), "one colour");
    l.labels.enabled = true;
    assert!(!plain_fill(l), "labels make it more than context");
}

/// A polygon layer in one colour running on past the frame is context beside a layer drawn by
/// classes; one inside the frame, such as a study area, is what the map shows.
#[test]
fn frame_coverage_leaves_out_one_colour_fills_beside_classes() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let count = |map: &mut Map| {
        let (.., cells) = frame_coverage(map, Coord::new(0.0, 0.0), 1_000_000.0, 100.0, 100.0, 64).unwrap();
        cells.iter().filter(|c| **c).count()
    };
    let mut map = Map::new("m");
    let square = |name: &str, r: f64| {
        let mut d = Dataset::new(name, GeometryType::Polygon, Vec::new());
        d.push(
            Some(Geometry::Polygon(vec![vec![[(-r, -r), (r, -r), (r, r), (-r, r), (-r, -r)].map(|(x, y)| Coord::new(x, y)).to_vec()]])),
            Vec::new(),
        );
        d
    };
    // About 0.9° across the frame: the states run on past it.
    let states = map.add_dataset(square("states", 2.0));
    let mut d = Dataset::new("far", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
    d.push(Some(Geometry::Point(Coord::new(-0.3, -0.3))), vec![Value::Text("c".into())]);
    let far = map.add_dataset(d);
    let with_states = count(&mut map);
    let s = |c| rustgis_map::Symbol::default_for(GeometryType::Point, c);
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == far) {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("c".to_string(), s(0))],
            other: s(1),
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    }
    let beside_classes = count(&mut map);
    assert!(beside_classes > 0 && beside_classes < with_states, "{beside_classes} of {with_states}");
    assert!(map.layer(states).is_some_and(|l| l.visible), "shown again");
    map.add_dataset(square("study area", 0.1));
    let with_study_area = count(&mut map);
    assert!(with_study_area > beside_classes, "a one-colour layer inside the frame counts: {with_study_area} of {beside_classes}");

    // A drawing three times the frame's width, judged on the frame: a layer running past the
    // frame but not past the drawing is context in both, and one inside the frame is counted.
    let mut map = Map::new("m");
    let neighbour = map.add_dataset(square("neighbour", 0.6));
    let far = {
        let mut d = Dataset::new("far", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(-0.3, -0.3))), vec![Value::Text("c".into())]);
        map.add_dataset(d)
    };
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == far) {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("c".to_string(), s(0))],
            other: s(1),
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    }
    let drawn = |map: &mut Map, judged: (f64, f64)| {
        let (.., cells) = frame_coverage_judged_on(map, Coord::new(0.0, 0.0), 1_000_000.0, (300.0, 300.0), judged, 192).unwrap();
        cells.iter().filter(|c| **c).count()
    };
    let on_the_drawing = drawn(&mut map, (300.0, 300.0));
    let on_the_frame = drawn(&mut map, (100.0, 100.0));
    assert!(on_the_frame < on_the_drawing / 4, "the neighbour is context on the frame: {on_the_frame} of {on_the_drawing}");
    assert!(map.layer(neighbour).is_some_and(|l| l.visible), "shown again");
}

/// A legend's rows go into columns: as even as they go, a layer's name with the row under it,
/// and in a box too short each filled in turn; one column draws as it always has.
#[test]
fn a_legends_rows_go_into_columns() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let mut map = Map::new("m");
    let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
    d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("c0".into())]);
    let pts = map.add_dataset(d);
    let s = |c| rustgis_map::Symbol::default_for(GeometryType::Point, c);
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
        // The layer's name, 30 classes and the other values: 32 rows.
        let classes = (0u32..30).map(|i| (format!("c{i}"), s(i))).collect();
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes,
            other: s(31),
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    }
    let opts = || LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    let rows = legend_rows(&map, None, &opts());
    assert_eq!(rows.len(), 32);
    assert_eq!(balanced(&rows, 1, (1e6, 1.0)), (vec![0], 32.0 * LEGEND_ROW));
    assert_eq!(balanced(&rows, 2, (1e6, 1.0)), (vec![0, 16], 16.0 * LEGEND_ROW));
    assert_eq!(balanced(&rows, 3, (1e6, 1.0)).1, 11.0 * LEGEND_ROW);
    assert_eq!(balanced(&[], 3, (1e6, 1.0)), (vec![0], 0.0));
    // Too short for them all: each filled in turn, and the last holds what fits of the rest.
    let (starts, fits) = flow(&rows, 3, 5.0 * LEGEND_ROW, (1e6, 1.0));
    assert_eq!((starts, fits), (vec![0, 5, 10], false));
    // A layer's name isn't left at the foot of a column; the last, with none under it, can be.
    let heads = [LegendRow::Head("a"), LegendRow::Head("b"), LegendRow::Head("c"), LegendRow::Head("d"), LegendRow::Head("e")];
    assert_eq!(flow(&heads, 3, 3.0 * LEGEND_ROW, (1e6, 1.0)), (vec![0, 2], true));

    let sizes = legend_sizes_mm(&mut map, 1e6, true, false, (&[], &NO_LABELS)).unwrap();
    let row = f64::from(LEGEND_ROW) * 25.4 / 96.0;
    for (size, rows) in sizes.iter().zip([32.0, 16.0, 11.0]) {
        assert!((size.1 - rows * row).abs() < 1e-6, "{size:?} for {rows} rows");
    }
    assert!(sizes[0].0 < sizes[1].0 && sizes[1].0 < sizes[2].0, "{sizes:?}");

    // How far right and down the legend draws in a box, and how many pixels.
    let ink = |columns: usize, w: f32, h: f32| {
        let mut pm = Pixmap::new(500, 500).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &map, ("", columns), None, opts(), 1.0);
        });
        let dark = |x, y| pm.pixel(x, y).is_some_and(|p| p.red() < 200 || p.green() < 200 || p.blue() < 200);
        let right = (0..500).filter(|&x| (0..500).any(|y| dark(x, y))).max().unwrap_or(0);
        let down = (0..500).filter(|&y| (0..500).any(|x| dark(x, y))).max().unwrap_or(0);
        (right, down)
    };
    let mm_pt = 96.0 / 25.4;
    let (right1, down1) = ink(1, 500.0, 500.0);
    let (right2, down2) = ink(2, 500.0, 500.0);
    let (right3, down3) = ink(3, 500.0, 500.0);
    assert!(f64::from(down1) > 31.0 * sizes[0].1 / 32.0 * mm_pt, "{down1}");
    assert!(f64::from(down2) <= sizes[1].1 * mm_pt && down2 > down3, "{down2} then {down3}");
    assert!(right1 < right2 && right2 < right3, "{right1}, {right2}, {right3}");
    assert!(f64::from(right3) <= sizes[2].0 * mm_pt + 1.0, "{right3} across {:?}", sizes[2]);
    // A value out of range, from a file edited by hand, draws what the nearest would.
    assert_eq!(ink(0, 500.0, 500.0), (right1, down1));
    assert_eq!(ink(9, 500.0, 500.0), (right3, down3));
    // A box too narrow for the columns closes them up: the last still starts inside it, past
    // where the first two end in a box of their own.
    let narrow = f64::from(right3) as f32 * 0.8;
    let (right_narrow, down_narrow) = ink(3, narrow, 500.0);
    assert!(f64::from(right_narrow) < f64::from(right3) && right_narrow > right1, "{right_narrow} of {right3}");
    // Moved sideways, the letters' lowest edge can land a pixel higher; a lost row would be 15.
    assert!(down_narrow.abs_diff(down3) < LEGEND_ROW as u32 / 2, "no row left out: {down_narrow} of {down3}");
}

/// The widest class label of `rows`, in points.
fn widest_label(rows: &[LegendRow]) -> f32 {
    rows.iter().map(|row| row.width(0.0, 1.0) - 12.0).fold(0.0, f32::max)
}

/// A point layer in Unique Values, for a legend's rows.
fn classed_point_map(classes: Vec<(String, rustgis_map::Symbol)>, small: rustgis_map::Symbol) -> Map {
    point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes,
            other: small,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    })
}

/// A box one row tall: in one column a layer's name is drawn alone, as it always was; in more,
/// it waits for room for the class under it, and the first column stays blank.
#[test]
fn a_layers_name_stays_with_its_class_only_in_columns() {
    let small = rustgis_map::Symbol::default_for(GeometryType::Point, 0);
    let classes = (0u32..4).map(|i| (format!("c{i}"), small)).collect();
    let map = classed_point_map(classes, small);
    let opts = || LegendOptions { show_no_data: false, hide_other: true, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    let rows = legend_rows(&map, None, &opts());
    let (starts, _) = flow(&rows, 2, LEGEND_ROW, (1e6, 1.0));
    assert_eq!(starts, vec![0, 2]);
    let first = widest(column_parts(&rows, &starts).next().unwrap(), (1e6, 1.0));
    let drawn_in_first = |columns: usize| {
        let mut pm = Pixmap::new(300, 15).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 300.0, LEGEND_ROW).unwrap(), &map, ("", columns), None, opts(), 1.0);
        });
        (0..first as u32).any(|x| (0..15).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 200)))
    };
    assert!(drawn_in_first(1), "one column draws the name alone");
    assert!(!drawn_in_first(2), "two columns keep it with its class");
}

/// A large point symbol makes its row taller and widens its column's symbols only: the columns
/// balance what the rows take, and the other column stays as narrow as its labels let it.
#[test]
fn a_legends_columns_hold_large_point_symbols() {
    let small = rustgis_map::Symbol { size: 4.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    let classes = (0u32..10).map(|i| (format!("c{i}"), rustgis_map::Symbol { size: if i < 2 { 40.0 } else { 4.0 }, ..small })).collect();
    let map = classed_point_map(classes, small);
    let opts = LegendOptions { show_no_data: false, hide_other: true, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    let rows = legend_rows(&map, None, &opts);
    let tall = rows[1].height(1e6, 1.0);
    assert!(tall > 2.0 * LEGEND_ROW, "{tall}");
    // The name and the two large symbols, then the eight small ones: counted row by row, these
    // would be split at the middle.
    let (starts, height) = balanced(&rows, 2, (1e6, 1.0));
    assert_eq!(starts, vec![0, 3], "{height}");
    assert_eq!(height, 8.0 * LEGEND_ROW);
    let parts: Vec<_> = column_parts(&rows, &starts).collect();
    assert!(
        widest(parts[1], (1e6, 1.0)) < widest(parts[0], (1e6, 1.0)) - 20.0,
        "{} and {}",
        widest(parts[0], (1e6, 1.0)),
        widest(parts[1], (1e6, 1.0))
    );
    // In one column, the small rows' labels line up with the large symbols' as before.
    assert_eq!(
        widest(&rows, (1e6, 1.0)),
        widest(parts[0], (1e6, 1.0)).max(swatch_column(&rows, (1e6, 1.0)) + 12.0 + widest_label(parts[1]))
    );
}

/// A legend shows a layer under the label it gives it, by the layer's name in any case (#1654):
/// a plain layer's row and a classed layer's heading; it measures and draws the label, so its
/// size follows it. A layer it gives no label, or a blank one, shows its name.
#[test]
fn a_legend_shows_a_layers_label() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    let mut map = Map::new("m");
    let long = "BrasilData: Setores censitários 2022 (MG)";
    map.add_dataset(Dataset::new(long, GeometryType::Point, vec![]));
    let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
    d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("a".into())]);
    let pts = map.add_dataset(d);
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), rustgis_map::Symbol::default_for(GeometryType::Point, 0))],
            other: rustgis_map::Symbol::default_for(GeometryType::Point, 1),
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    }
    let labels =
        |pairs: &[(&str, &str)]| -> BTreeMap<String, String> { pairs.iter().map(|(n, l)| (n.to_string(), l.to_string())).collect() };
    let names = |labels: &BTreeMap<String, String>| -> Vec<String> {
        let opts = LegendOptions { show_no_data: false, hide_other: true, exclude: &[], labels, scale: 1e6, size: 1.0 };
        legend_rows(&map, None, &opts)
            .iter()
            .filter_map(|r| match r {
                LegendRow::Head(n) | LegendRow::Plain(_, n) => Some(n.to_string()),
                _ => None,
            })
            .collect()
    };
    // Top first: pts over the long-named layer.
    assert_eq!(names(&NO_LABELS), ["pts", long]);
    let short = labels(&[("brasildata: Setores censitários 2022 (mg)", "Setores"), ("PTS", "Pontos")]);
    assert_eq!(names(&short), ["Pontos", "Setores"], "by name, any case");
    assert_eq!(names(&labels(&[("pts", "  ")])), ["pts", long], "a blank label shows the name");
    assert_eq!(names(&labels(&[("Atlantis", "x")])), ["pts", long], "a label for no layer of the map changes nothing");
    // Its size: narrower with the short label, wider with a long one; as tall either way.
    let size = |labels: &BTreeMap<String, String>| legend_sizes_mm(&mut map.clone(), 1e6, true, true, (&[], labels)).unwrap()[0];
    let (named, labelled) = (size(&NO_LABELS), size(&short));
    let wide = size(&labels(&[(long, "Setores censitários do Censo Demográfico de 2022 em Minas Gerais")]));
    assert!(labelled.0 < named.0 - 20.0 && wide.0 > named.0 + 20.0, "{labelled:?} {named:?} {wide:?}");
    assert!((labelled.1 - named.1).abs() < 1e-9 && (wide.1 - named.1).abs() < 1e-9);
    // Drawn: the label, which ends within the width measured for it.
    let reach = |labels: &BTreeMap<String, String>| {
        let mut pm = Pixmap::new(400, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { show_no_data: false, hide_other: true, exclude: &[], labels, scale: 1e6, size: 1.0 };
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 400.0, 200.0).unwrap(), &map, ("", 1), None, opts, 1.0)
        });
        let right = (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 200))).max();
        f64::from(right.unwrap_or(0)) * 25.4 / 96.0
    };
    for (labels, (w, _)) in [(&NO_LABELS, named), (&short, labelled)] {
        let right = reach(labels);
        assert!(right <= w + 0.3 && right > w - 1.5, "{right} mm drawn across {w} mm");
    }
}

#[test]
fn legends_leave_out_what_the_frame_does_not_draw() {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
    // A layer with one feature, of class "a", classed a, b and c; and an empty layer.
    let mut map = Map::new("m");
    let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
    d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("a".into())]);
    let pts = map.add_dataset(d);
    map.add_dataset(Dataset::new("empty", GeometryType::Point, vec![]));
    let s = |c| rustgis_map::Symbol::default_for(GeometryType::Point, c);
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
        let classes = ["a", "b", "c"].iter().zip(0u32..).map(|(k, i)| (k.to_string(), s(i))).collect();
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes,
            other: s(3),
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    }
    // How far down the legend draws, in pixels.
    let depth_without = |map: &Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, hide_other: bool, exclude: &[String]| {
        let mut pm = Pixmap::new(300, 400).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { show_no_data: false, hide_other, exclude, labels: &NO_LABELS, scale: 1e6, size: 1.0 };
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 300.0, 400.0).unwrap(), map, ("", 1), drawn, opts, 1.0)
        });
        (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(|p| p.red() < 200))).max().unwrap_or(0)
    };
    let depth = |map: &Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, hide_other: bool| depth_without(map, drawn, hide_other, &[]);
    let drawn = shown_legends(&map, 1e6, false, &Env::default());
    assert_eq!(drawn.len(), 1, "the empty layer draws nothing: {drawn:?}");
    let (shown, all) = (depth(&map, Some(&drawn), false), depth(&map, None, false));
    // The drawn legend is two rows (the layer's heading and "a"); every class makes it six
    // (b, c, <all other values> and the empty layer's row too). Compared with each other, so
    // the font and antialiasing don't matter.
    assert!(shown > 0, "the drawn class is listed");
    assert!(all >= 2 * shown, "{shown} px of {all}");
    // The size a legend takes holds what it draws, to within a row: two rows drawn, six with
    // every class.
    let row = f64::from(LEGEND_ROW) * 25.4 / 96.0;
    // How far right the legend draws, in pixels (points at this scale), as `depth` goes down.
    let reach = |map: &Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>| {
        let mut pm = Pixmap::new(300, 400).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(
                c,
                Rect::from_xywh(0.0, 0.0, 300.0, 400.0).unwrap(),
                map,
                ("", 1),
                drawn,
                LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 },
                1.0,
            )
        });
        (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 200))).max().unwrap_or(0)
    };
    for (all_classes, ink, right) in [(false, shown, reach(&map, Some(&drawn))), (true, all, reach(&map, None))] {
        let (w, h) = legend_sizes_mm(&mut map, 1e6, all_classes, false, (&[], &NO_LABELS)).unwrap()[0];
        let (ink, right) = (f64::from(ink) * 25.4 / 96.0, f64::from(right) * 25.4 / 96.0);
        assert!(w > 0.0 && ink <= h && h - ink < row, "{ink} mm drawn in {h} mm");
        // The widths in the same units as the drawing: the ink ends within the width measured,
        // short of it by no more than a glyph's side bearing.
        assert!(right <= w + 0.3 && right > w - 1.5, "{right} mm drawn across {w} mm");
    }
    assert!((legend_sizes_mm(&mut map, 1e6, false, false, (&[], &NO_LABELS)).unwrap()[0].1 - 2.0 * row).abs() < 1e-9);
    // A layer the legend leaves out (by name, any case) isn't listed, drawn or not.
    let name = map.layer(pts).unwrap().name.to_uppercase();
    assert_eq!(depth_without(&map, Some(&drawn), false, std::slice::from_ref(&name)), 0, "its only drawn layer left out");
    let rest = depth_without(&map, None, false, &[name]);
    assert!(rest > 0 && rest < all, "the other layer is still listed: {rest} px of {all}");
    // Hiding <all other values> takes one row off the full legend, and nothing off the drawn
    // one, which has none.
    assert!(depth(&map, None, true) < all);
    assert_eq!(depth(&map, Some(&drawn), true), shown);
    // A layer whose drawn features all fall in <all other values>, hidden, is left out rather
    // than drawn as a plain swatch.
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts)
        && let rustgis_map::Renderer::Unique { classes, .. } = &mut l.renderer
    {
        classes.retain(|(k, _)| k != "a");
    }
    let drawn = shown_legends(&map, 1e6, false, &Env::default());
    assert!(depth(&map, Some(&drawn), false) > 0, "<all other values> alone is listed");
    assert_eq!(depth(&map, Some(&drawn), true), 0, "and hidden, the layer is left out");
    // So is one whose drawn features all fall in a fully transparent <all other values>,
    // rather than drawn as a plain swatch of its first feature.
    let set_other_alpha = |map: &mut Map, a: u8| {
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts)
            && let rustgis_map::Renderer::Unique { other, .. } = &mut l.renderer
        {
            other.fill = other.fill.with_alpha(a);
            other.stroke = other.stroke.with_alpha(a);
        }
    };
    set_other_alpha(&mut map, 0);
    let drawn = shown_legends(&map, 1e6, false, &Env::default());
    assert_eq!(depth(&map, Some(&drawn), false), 0, "the invisible fallback stands for nothing");
    set_other_alpha(&mut map, 255);
    // A class that bears the fallback's name is a class: hiding the fallback keeps it.
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts)
        && let rustgis_map::Renderer::Unique { classes, .. } = &mut l.renderer
    {
        classes.push(("a".into(), s(0)));
        classes.iter_mut().for_each(|(k, _)| {
            if k == "a" {
                *k = rustgis_map::symbology::OTHER_VALUES.into();
            }
        });
    }
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
        l.dataset.features.iter_mut().for_each(|f| f.attrs = vec![Value::Text(rustgis_map::symbology::OTHER_VALUES.into())]);
    }
    let drawn = shown_legends(&map, 1e6, false, &Env::default());
    assert!(depth(&map, Some(&drawn), true) > 0, "the class named like the fallback is still listed");
    // A Graduated layer with no classes still shows its plain swatch, as before #207.
    if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
        l.renderer = rustgis_map::Renderer::Graduated {
            field: "g".into(),
            breaks: Vec::new(),
            symbols: Vec::new(),
            ramp: rustgis_map::Ramp::Ember,
            method: rustgis_map::ClassMethod::NaturalBreaks,
            min: 0.0,
            classes: 0,
            class_labels: Default::default(),
            class_strokes: Default::default(),
            class_fills: Default::default(),
        };
    }
    map.layers.retain(|l| l.id == pts);
    assert!(depth(&map, None, false) > 0, "the empty Graduated layer is listed");
}

/// A layout with one map frame on `map` and a colorbar of its first raster, by name (one drawn by
/// classes isn't a colorbar's by default, #1679), at 10–90 × 10–30 mm (a vertical one at 10–30 ×
/// 10–90 mm), drawn at 96 dpi.
fn colorbar_page(map: &Map, orientation: Orientation, reverse: bool, title: &str) -> Pixmap {
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let frame = l.add(
        "Map",
        (95.0, 95.0, 4.0, 4.0),
        ElementKind::MapFrame {
            map: map.name.clone(),
            center: Coord::new(0.0, 0.0),
            scale: 1e8,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    let rect = if orientation == Orientation::Vertical { (10.0, 10.0, 20.0, 80.0) } else { (10.0, 10.0, 80.0, 20.0) };
    let layer = map.layers.iter().find(|l| l.is_raster()).map(|l| l.name.clone());
    let kind =
        ElementKind::Colorbar { frame, layer, title: title.into(), orientation, ticks: 5, tick_side: TickSide::End, reverse, color: None };
    l.add("Colorbar", rect, kind);
    render(&l, &mut [map.clone()], 96.0, &PageInfo::default()).unwrap()
}

/// A page whose frame draws a layer without its cast shadows says why, named by the layer; one
/// with them, or without any asked for, says nothing.
#[test]
fn a_page_says_why_a_frame_drew_no_cast_shadows() {
    let relief = |shadow| rustgis_map::raster::RasterRenderer::ShadedRelief {
        band: 0,
        ramp: None,
        azimuth: 315.0,
        altitude: 30.0,
        z_factor: 1.0,
        shadow,
        sky: 0.0,
        fill: 0.0,
        rock: None,
        ambient: None,
        balanced: false,
    };
    for shadow in [0.0, 1.0] {
        let map = ramp_map(relief(shadow), rustgis_map::raster::Stretch::MinMax);
        let (_, said) = crate::shadows_said(|| colorbar_page(&map, Orientation::Horizontal, false, ""));
        assert!(said.notices.is_empty() && !said.to_come, "shadow {shadow}: {said:?}");
    }
    // Read in place with only its overview held: no whole grid to trace.
    let mut map = ramp_map(relief(1.0), rustgis_map::raster::Stretch::MinMax);
    let rl = map.layers.iter_mut().find_map(|l| l.raster.as_mut()).unwrap();
    rl.in_place = true;
    let mut r = (**rl.data.as_ref().unwrap()).clone();
    r.overview = Some((1000, 10));
    rl.data = Some(std::sync::Arc::new(r));
    let (_, said) = crate::shadows_said(|| colorbar_page(&map, Orientation::Horizontal, false, ""));
    let said = said.notices;
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("dem: ") && said[0].contains("read in place"), "{said:?}");
    // What the map view's frame said is not a page's: a clean page drawn after it says nothing.
    let camera = rustgis_map::Camera { center: rustgis_geom::lonlat_to_merc(Coord::new(0.5, 0.5)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
    crate::render(&mut map, &crate::View { width: 64.0, height: 64.0, pixels_per_point: 1.0, camera, sky: false });
    let clean = ramp_map(relief(0.0), rustgis_map::raster::Stretch::MinMax);
    let (_, said) = crate::shadows_said(|| colorbar_page(&clean, Orientation::Horizontal, false, ""));
    assert!(said.notices.is_empty(), "{said:?}");
}

/// A page with maps showing copies of one relief (each layer the first of its map, so with the
/// same id), each under its own sun, drawn again until it has no shadows to come, as the
/// Layout view draws it while they land. Panics if it never settles.
fn page_settles_with_suns(suns: &[f64]) {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let n = 300usize;
    let heights: Vec<f32> = (0..n * n).map(|i| (((i % n) as f32 / 9.0).sin() + ((i / n) as f32 / 7.0).cos()) * 400.0).collect();
    let gt = GeoTransform { origin_x: -1.0, origin_y: 1.0, pixel_width: 2.0 / n as f64, pixel_height: -2.0 / n as f64 };
    let dem = Raster::new("dem", n as u32, n as u32, vec![heights], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let rl = rustgis_map::raster::RasterLayer::from_path(dem, "dem.tif");
    let mut maps = Vec::new();
    let mut l = Layout::new("Layout", 100.0, 100.0);
    for (i, azimuth) in suns.iter().enumerate() {
        let name = format!("m{i}");
        let mut copy = rl.clone();
        copy.style.renderer = rustgis_map::raster::RasterRenderer::ShadedRelief {
            band: 0,
            ramp: None,
            azimuth: *azimuth,
            altitude: 10.0,
            z_factor: 1.0,
            shadow: 1.0,
            sky: 0.0,
            fill: 0.0,
            rock: None,
            ambient: None,
            balanced: false,
        };
        let mut m = Map::new(name.clone());
        assert_eq!(m.add_raster("dem", copy), 1);
        maps.push(m);
        let frame = ElementKind::MapFrame {
            map: name.clone(),
            center: Coord::new(0.0, 0.0),
            scale: 1e7,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        };
        l.add(&name, (5.0 + 20.0 * i as f64, 5.0, 18.0, 18.0), frame);
    }
    let start = std::time::Instant::now();
    loop {
        let (drawn, said) = crate::shadows_said(|| render_pending(&l, &mut maps, 48.0, &PageInfo::default()));
        drawn.unwrap();
        assert!(said.notices.is_empty(), "{said:?}");
        if !said.to_come {
            break;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(60), "the page never got its shadows");
        std::thread::yield_now();
    }
}

/// Two maps lit otherwise on a page: neither map's grid stops the other's.
#[test]
fn two_maps_lit_otherwise_on_a_page_keep_their_own_shadows() {
    page_settles_with_suns(&[315.0, 135.0]);
}

/// More maps lit otherwise than the grids kept for going back: none is let go while it is drawn.
#[test]
fn many_maps_lit_otherwise_on_a_page_all_get_their_shadows() {
    page_settles_with_suns(&[315.0, 135.0, 45.0, 225.0, 0.0]);
}

/// A map with one 100 × 1 raster valued 0–99 by column, styled by `renderer`.
fn ramp_map(renderer: rustgis_map::raster::RasterRenderer, stretch: rustgis_map::raster::Stretch) -> Map {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
    let data: Vec<f32> = (0..100).map(|i| i as f32).collect();
    let r = Raster::new("dem", 100, 1, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let mut rl = rustgis_map::raster::RasterLayer::from_path(r, "dem.tif");
    rl.style.renderer = renderer;
    rl.style.stretch = stretch;
    let mut map = Map::new("m");
    map.add_raster("dem", rl);
    map
}

/// A map with a land cover raster of `n` classes (values 1 to `n`, one cell each) drawn by its
/// colour table, the first named "Forest".
fn classes_map(n: u8) -> Map {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
    let mut palette = vec![[0, 0, 0, 0]; 256];
    for v in 1..=n {
        palette[usize::from(v)] = [v, 255 - v, 128, 255];
    }
    let data: Vec<f32> = (1..=n).map(f32::from).collect();
    let mut r = Raster::new("lc", u32::from(n), 1, vec![data], gt, Crs::LonLat, None, ColorHint::Palette(palette), "UInt8").unwrap();
    r.bands[0].classes = vec![String::new(), "Forest".into()];
    let mut rl = rustgis_map::raster::RasterLayer::from_path(r, "lc.tif");
    rl.style.renderer = rustgis_map::raster::RasterRenderer::Colormap;
    let mut map = Map::new("m");
    map.add_raster("Land cover", rl);
    map
}

/// A classified raster's legend: its name over its classes, as the Layers pane lists them, by
/// what the frame shows and with every class alike (a colour table lists the values it holds);
/// left out by name; a raster on a ramp isn't listed (#1679).
#[test]
fn a_legend_lists_a_classified_rasters_classes() {
    let mut map = classes_map(5);
    let opts = LegendOptions { show_no_data: false, hide_other: true, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    let labels = |rows: &[LegendRow]| -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                LegendRow::Head(name) => format!("[{name}]"),
                LegendRow::Class(_, en) => en.label.clone(),
                LegendRow::Plain(_, name) => format!("plain {name}"),
                LegendRow::Picture(_) => "picture".into(),
            })
            .collect()
    };
    let want = ["[Land cover]", "Forest", "2", "3", "4", "5"];
    assert_eq!(labels(&legend_rows(&map, None, &opts)), want);
    let drawn = shown_legends(&map, 1e6, false, &Env::default());
    assert_eq!(labels(&legend_rows(&map, Some(&drawn), &opts)), want);
    // Its swatches are the pane's colours.
    let pane: Vec<Rgba> = map.layers[0]
        .raster
        .as_ref()
        .unwrap()
        .legend()
        .into_iter()
        .filter_map(|e| match e {
            rustgis_map::raster::RasterLegend::Class { color, .. } => Some(color),
            _ => None,
        })
        .collect();
    let swatches: Vec<Rgba> = legend_rows(&map, None, &opts)
        .iter()
        .filter_map(|r| if let LegendRow::Class(_, en) = r { Some(en.symbol.fill) } else { None })
        .collect();
    assert_eq!(swatches, pane);
    // Sized by its rows: its name and five classes.
    let mm = 25.4 / 96.0;
    let [(_, h), ..] = legend_sizes_mm(&mut map, 1e6, false, false, (&[], &NO_LABELS)).unwrap();
    assert!((h - 6.0 * f64::from(LEGEND_ROW) * mm).abs() < 1e-6, "{h} mm");
    let labelled = BTreeMap::from([("Land cover".to_string(), "Classes do MapBiomas".to_string())]);
    let opts_labelled = LegendOptions { labels: &labelled, ..opts };
    assert_eq!(labels(&legend_rows(&map, None, &opts_labelled))[0], "[Classes do MapBiomas]");
    // Left out by name, or when hidden.
    let exclude = ["land cover".to_string()];
    assert!(legend_rows(&map, None, &LegendOptions { exclude: &exclude, ..opts }).is_empty());
    map.layers[0].visible = false;
    assert!(shown_legends(&map, 1e6, false, &Env::default()).is_empty());
    assert!(legend_rows(&map, None, &opts).is_empty());
    // A raster on a ramp: a colorbar keys it, the legend doesn't list it.
    let ramp = ramp_map(
        rustgis_map::raster::RasterRenderer::Stretch { band: 0, ramp: rustgis_map::Ramp::Harbour, invert: false },
        rustgis_map::raster::Stretch::MinMax,
    );
    assert!(legend_rows(&ramp, None, &opts).is_empty() && shown_legends(&ramp, 1e6, false, &Env::default()).is_empty());
}

/// A drawn legend fills each class's swatch with its colour, under the raster's name.
#[test]
fn a_drawn_legend_fills_a_raster_class_with_its_colour() {
    let map = classes_map(5);
    let pm = point_legend(&map);
    for v in 1..=5u8 {
        let c = (v, 255 - v, 128);
        let found = pm.pixels().iter().any(|p| (p.red(), p.green(), p.blue()) == c);
        assert!(found, "class {v}'s colour {c:?}");
    }
}

/// A page with a main frame and an inset (5–55 mm) that outlines it in `color` at `width`.
fn indicator_page(color: Rgba, width: f64) -> Pixmap {
    let map = Map::new("m");
    let frame = |center, scale, extent_of| ElementKind::MapFrame {
        map: "m".into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: color,
        extent_width: width,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let main = l.add("Main", (60.0, 60.0, 30.0, 30.0), frame(Coord::new(20.0, 10.0), 5e7, None));
    l.add("Inset", (5.0, 5.0, 50.0, 50.0), frame(Coord::new(0.0, 0.0), 2e8, Some(main)));
    render(&l, &mut [map], 96.0, &PageInfo::default()).unwrap()
}

/// Pixels inside the inset (5–55 mm) close to `c`.
fn inset_pixels(pm: &Pixmap, c: Rgba) -> usize {
    let k = PT_PER_MM;
    let near = |p: (u8, u8, u8)| p.0.abs_diff(c.r()) < 40 && p.1.abs_diff(c.g()) < 40 && p.2.abs_diff(c.b()) < 40;
    ((5.0 * k) as u32..(55.0 * k) as u32)
        .flat_map(|y| ((5.0 * k) as u32..(55.0 * k) as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| near(rgb(pm, x, y)))
        .count()
}

/// One line of black text drawn in a white 400 × 60 px box with `style`.
fn styled_line(style: TextStyle) -> Pixmap {
    let mut pm = Pixmap::new(400, 60).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 0.0, 400.0, 60.0).unwrap();
    crate::canvas::with_canvas(&mut pm, |c| text_block(c, r, "Oceano Atlântico", 20.0, Rgba::rgb(0, 0, 0), style, 1.0));
    pm
}

/// The leftmost and rightmost inked columns, and the inked column nearest the left at the
/// top and at the bottom of the letters.
fn text_ink(pm: &Pixmap) -> (u32, u32, u32, u32) {
    let dark = |x: u32, y: u32| rgb(pm, x, y).0 < 128;
    let rows: Vec<u32> = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| dark(x, y))).collect();
    let (top, bottom) = (rows[0], rows[rows.len() - 1]);
    let cols: Vec<u32> = (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| dark(x, y))).collect();
    let first = |y: u32| (0..pm.width()).find(|&x| dark(x, y)).unwrap_or(0);
    (cols[0], cols[cols.len() - 1], first(top + 1), first(bottom - 1))
}

/// The height of the inked rows.
fn text_height(pm: &Pixmap) -> u32 {
    let rows: Vec<u32> = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| rgb(pm, x, y).0 < 128)).collect();
    rows[rows.len() - 1] - rows[0]
}

/// A page with `kind` in (10, 10, 80, 30) mm on white.
fn shape_page(kind: ElementKind) -> Layout {
    let mut l = Layout::new("L", 100.0, 50.0);
    l.add("Shape", (10.0, 10.0, 80.0, 30.0), kind);
    l
}

/// The runs of inked and blank pixels along a row (or a column) from `from` to `to`: how many
/// times it goes from ink to blank.
fn gaps(pm: &Pixmap, along: impl Iterator<Item = (u32, u32)>) -> usize {
    let ink: Vec<bool> = along.map(|(x, y)| rgb(pm, x, y).0 < 128).collect();
    ink.windows(2).filter(|w| w[0] && !w[1]).count()
}

/// A dashed line has gaps where its pattern says, its arrowhead solid; a dashed rectangle's
/// outline has gaps on all four sides; solid, both draw as before (#1763).
#[test]
fn dashed_lines_and_outlines_have_gaps() {
    use rustgis_map::layout::LineDir;
    let black = Rgba::rgb(0, 0, 0);
    let dash = rustgis_map::symbology::Dash::new(&[6.0, 6.0]);
    let png = |kind: ElementKind| render(&shape_page(kind), &mut [], 96.0, &PageInfo::default()).unwrap();
    // 96 dpi: 1 mm is 3.78 px. The line runs along y = 25 mm from x = 10 to 90 mm.
    let px = |mm: f64| (mm * 96.0 / 25.4).round() as u32;
    let line = |dash, head| ElementKind::Line { color: black, width: 2.0, dir: LineDir::Horizontal, head, dash };
    let along = |pm: &Pixmap| gaps(pm, (px(11.0)..px(80.0)).map(|x| (x, px(25.0))));
    assert_eq!(along(&png(line(None, LineHead::None))), 0, "solid");
    // 69 mm is about 261 points (PT_PER_MM): a 12-point round each, so some 22 gaps.
    let dashed = png(line(dash, LineHead::End));
    assert!((20..=24).contains(&along(&dashed)), "{} gaps", along(&dashed));
    // The head (the last 6 + 2.5 × 2 = 11 points, 2.9 mm) is filled: no gap across it.
    assert_eq!(gaps(&dashed, (px(87.5)..px(89.8)).map(|x| (x, px(25.0)))), 0, "the arrowhead stays solid");
    let rect = |dash| ElementKind::Rectangle { fill: Rgba::new(0, 0, 0, 0), outline: black, width: 2.0, radius: 0.0, dash };
    let sides = |pm: &Pixmap| {
        let (x0, x1, y0, y1) = (px(10.0), px(90.0), px(10.0), px(40.0));
        [
            gaps(pm, (x0 + 4..x1 - 4).map(|x| (x, y0))),
            gaps(pm, (x0 + 4..x1 - 4).map(|x| (x, y1))),
            gaps(pm, (y0 + 4..y1 - 4).map(|y| (x0, y))),
            gaps(pm, (y0 + 4..y1 - 4).map(|y| (x1, y))),
        ]
    };
    assert_eq!(sides(&png(rect(None))), [0; 4], "solid");
    let dashed = sides(&png(rect(dash)));
    assert!(dashed.iter().all(|g| *g >= 3), "gaps on every side: {dashed:?}");
    let oval = |dash| ElementKind::Ellipse { fill: Rgba::new(0, 0, 0, 0), outline: black, width: 2.0, dash };
    // Round its outline: the ellipse fills (10, 10, 80, 30) mm.
    let round = |pm: &Pixmap| {
        let px = |mm: f64| mm * 96.0 / 25.4;
        // Inked where any pixel within one of the curve's point is: the 2-point stroke is about
        // 2 pixels wide.
        let near = |(x, y): (f64, f64)| {
            (-1..=1).any(|dx| (-1..=1).any(|dy| rgb(pm, (x.round() as i32 + dx) as u32, (y.round() as i32 + dy) as u32).0 < 128))
        };
        let ink: Vec<bool> = (0..2000)
            .map(|i| f64::from(i) / 2000.0 * std::f64::consts::TAU)
            .map(|t| near((px(50.0 + 40.0 * t.cos()), px(25.0 + 15.0 * t.sin()))))
            .collect();
        ink.windows(2).filter(|w| w[0] && !w[1]).count()
    };
    assert_eq!(round(&png(oval(None))), 0, "solid");
    assert!(round(&png(oval(dash))) >= 20, "{}", round(&png(oval(dash))));
}

/// A dashed box round a turned text, on one page (#1763): the picture has the box's gaps and
/// the text running down the page inside it; the recorded page (the PDF's) keeps the dashes as
/// pieces and the text as glyphs; the SVG writes the dash array and the turn.
#[test]
fn a_dashed_box_and_a_turned_text_share_a_page() {
    use crate::vector::Item;
    // The text's box (10, 45, 80, 10) mm turned 90° about (50, 50): 45..55 across, 10..90 down.
    let mut layout = turned_text_page(90.0);
    let dash = rustgis_map::symbology::Dash::new(&[6.0, 6.0]);
    let outline = Rgba::rgb(0, 0, 0);
    layout.add(
        "Box",
        (5.0, 5.0, 90.0, 90.0),
        ElementKind::Rectangle { fill: Rgba::new(0, 0, 0, 0), outline, width: 2.0, radius: 0.0, dash },
    );
    let info = PageInfo::default();
    let png = render(&layout, &mut [], 96.0, &info).unwrap();
    let px = |mm: f64| (mm * 96.0 / 25.4).round() as u32;
    assert!(gaps(&png, (px(10.0)..px(90.0)).map(|x| (x, px(5.0)))) >= 3, "the box's top is dashed");
    // Inside the box: the text, tall and narrow about x = 50 mm.
    let inside: Vec<(u32, u32)> =
        (px(8.0)..px(92.0)).flat_map(|y| (px(8.0)..px(92.0)).map(move |x| (x, y))).filter(|&(x, y)| rgb(&png, x, y).0 < 128).collect();
    let (l, r) = (inside.iter().map(|p| p.0).min().unwrap(), inside.iter().map(|p| p.0).max().unwrap());
    let (t, b) = (inside.iter().map(|p| p.1).min().unwrap(), inside.iter().map(|p| p.1).max().unwrap());
    assert!(b - t > 4 * (r - l) && l > px(44.0) && r < px(56.0), "the text runs down the page: {l},{t} to {r},{b}");
    let (drawing, _) = render_drawing(&layout, &mut [], 96.0, &info).unwrap();
    let (_, _, items) = drawing.finish();
    let items = crate::vector::without_layers(items);
    let pieces: usize = items
        .iter()
        .filter_map(|i| if let Item::Stroke { path, .. } = i { Some(path) } else { None })
        .map(|p| p.segments().filter(|s| matches!(s, tiny_skia::PathSegment::MoveTo(_))).count())
        .sum();
    let text: String = items
        .iter()
        .filter_map(|i| if let Item::Glyphs(r) = i { Some(r) } else { None })
        .flat_map(|r| r.glyphs.iter().map(|g| g.ch))
        .collect();
    assert!(pieces > 20 && text == "Fonte: IBGE, 2022", "{pieces} dashes, text {text:?}");
    let svg = crate::svg::layout_svg(&layout, &mut [], 96.0, &info).unwrap().0;
    assert!(svg.contains("stroke-dasharray") && svg.contains(r#"transform="rotate(90 50 50)""#), "{svg}");
}

/// A dashed outline is kept as vectors, its dashes as the pieces of the path (as a dashed line
/// layer's are), which draw what its picture shows (#1763).
#[test]
fn a_dashed_outline_is_kept_as_its_dashes() {
    use crate::vector::Item;
    let dash = rustgis_map::symbology::Dash::new(&[6.0, 6.0]);
    let kind = ElementKind::Rectangle { fill: Rgba::new(0, 0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 2.0, radius: 0.0, dash };
    let layout = shape_page(kind);
    let info = PageInfo::default();
    let png = render(&layout, &mut [], 96.0, &info).unwrap();
    let (drawing, _) = render_drawing(&layout, &mut [], 96.0, &info).unwrap();
    let (_, _, items) = drawing.finish();
    let items = crate::vector::without_layers(items);
    let pieces: usize = items
        .iter()
        .filter_map(|i| if let Item::Stroke { path, .. } = i { Some(path) } else { None })
        .map(|p| p.segments().filter(|s| matches!(s, tiny_skia::PathSegment::MoveTo(_))).count())
        .sum();
    assert!(pieces > 20, "{pieces} dashes kept");
    let mut replayed = Pixmap::new(png.width(), png.height()).unwrap();
    replayed.fill(tiny_skia::Color::WHITE);
    crate::vector::replay(&items, &mut replayed, Transform::identity());
    let differ = png.pixels().iter().zip(replayed.pixels()).filter(|(a, b)| a.red().abs_diff(b.red()) > 48).count();
    assert!(differ < 50, "{differ} pixels differ");
}

/// A page with one text element in (10, 45, 80, 10) mm, turned `turn` degrees.
fn turned_text_page(turn: f64) -> Layout {
    let mut l = Layout::new("L", 100.0, 100.0);
    let mut kind = ElementKind::text("Fonte: IBGE, 2022", 14.0, Rgba::rgb(0, 0, 0), false);
    if let ElementKind::Text { rotation, .. } = &mut kind {
        *rotation = turn;
    }
    l.add("Source", (10.0, 45.0, 80.0, 10.0), kind);
    l
}

/// The dark pixels' bounds: left, top, right, bottom.
fn ink_bounds(pm: &Pixmap) -> (u32, u32, u32, u32) {
    let dark: Vec<(u32, u32)> =
        (0..pm.height()).flat_map(|y| (0..pm.width()).map(move |x| (x, y))).filter(|&(x, y)| rgb(pm, x, y).0 < 128).collect();
    let xs = dark.iter().map(|p| p.0);
    let ys = dark.iter().map(|p| p.1);
    (xs.clone().min().unwrap(), ys.clone().min().unwrap(), xs.max().unwrap(), ys.max().unwrap())
}

/// A page with `text` at 12 pt in (10, 10, 40, 60) mm, wrapped to its box or not (#2346),
/// aligned `align` and turned `turn` degrees.
fn note_page(text: &str, wrap: bool, align: TextAlign, turn: f64) -> Layout {
    let mut l = Layout::new("L", 100.0, 100.0);
    let mut kind = ElementKind::text(text, 12.0, Rgba::rgb(0, 0, 0), false);
    if let ElementKind::Text { wrap: w, align: a, rotation, .. } = &mut kind {
        (*w, *a, *rotation) = (wrap, align, turn);
    }
    l.add("Note", (10.0, 10.0, 40.0, 60.0), kind);
    l
}

const NOTE: &str = "Population counts from the 2020 census; boundaries simplified for print";

/// A wrapped text breaks at spaces into lines that stay within its box's width, whichever
/// its alignment; unwrapped, the same sentence runs past the box as it always has (#2346).
#[test]
fn a_wrapped_text_breaks_into_lines_that_fit_its_box() {
    let mm = 96.0 / 25.4;
    let (left, right) = (10.0 * mm, 50.0 * mm);
    let ink = |l: &Layout| ink_bounds(&render(l, &mut [], 96.0, &PageInfo::default()).unwrap());
    let (_, t, r, b) = ink(&note_page(NOTE, false, TextAlign::Left, 0.0));
    assert!(f64::from(r) > right + 40.0, "unwrapped, it runs past the box: {r}");
    let line = b - t;
    for align in TextAlign::ALL {
        let (l, t, r, b) = ink(&note_page(NOTE, true, align, 0.0));
        assert!(f64::from(l) >= left - 2.0 && f64::from(r) <= right + 2.0, "{align:?}: inked {l}..{r} in {left}..{right}");
        assert!(b - t > 3 * line, "{align:?}: in several lines, {t}..{b}");
    }
    // A word wider than the box (an address) is broken across lines, kept within it.
    let url = "https://example.org/data/population/census-2020/boundaries-simplified.zip";
    let (l, t, r, b) = ink(&note_page(url, true, TextAlign::Left, 0.0));
    assert!(f64::from(l) >= left - 2.0 && f64::from(r) <= right + 2.0 && b - t > 2 * line, "{l},{t} to {r},{b}");
    // Its own line breaks are kept: a blank line between two short ones stays.
    let (_, t, _, b) = ink(&note_page("Source\n\nIBGE", true, TextAlign::Left, 0.0));
    let (_, t2, _, b2) = ink(&note_page("Source\nIBGE", true, TextAlign::Left, 0.0));
    assert!(b - t > b2 - t2 + line / 2, "{t}..{b} against {t2}..{b2}");
}

/// A turned wrapped text wraps to its unturned box's width: turned 90°, its lines run down the
/// page within the box's 40 mm about its centre (#2346).
#[test]
fn a_turned_wrapped_text_wraps_to_its_unturned_width() {
    let mm = 96.0 / 25.4;
    let (l, t, r, b) = ink_bounds(&render(&note_page(NOTE, true, TextAlign::Left, 90.0), &mut [], 96.0, &PageInfo::default()).unwrap());
    // The box's centre is at (30, 40) mm: 40 mm wide, turned, it spans 20 to 60 mm down the page.
    assert!(f64::from(t) >= 20.0 * mm - 2.0 && f64::from(b) <= 60.0 * mm + 2.0, "{t}..{b}");
    assert!(r - l > 40, "its lines side by side: {l}..{r}");
}

/// A wrapped text whose lines reach past its box's bottom is reported as cut, as a text with
/// too many line breaks is; one whose lines fit isn't (#2346).
#[test]
fn a_wrapped_text_past_its_box_is_reported_cut() {
    let info = PageInfo::default();
    let mut l = note_page(NOTE, true, TextAlign::Left, 0.0);
    assert!(clipped_text(&l, &[], 96.0, &info).is_empty());
    let id = l.elements[0].id;
    l.element_mut(id).unwrap().h = 4.0;
    assert_eq!(clipped_text(&l, &[], 96.0, &info), vec![(id, "Note".into())]);
}

/// A wrapped text past the 200 lines a text draws is reported cut, even when its 201st line is
/// blank and the text goes on after it (#2346); one followed only by blank lines loses nothing
/// and isn't.
#[test]
fn a_wrapped_text_past_its_last_drawn_line_is_reported_cut() {
    let info = PageInfo::default();
    let page = |tail: &str| {
        let mut l = Layout::new("L", 100.0, 1500.0);
        let mut kind = ElementKind::text(format!("{}{tail}", vec!["a"; 200].join("\n")), 12.0, Rgba::rgb(0, 0, 0), false);
        if let ElementKind::Text { wrap, .. } = &mut kind {
            *wrap = true;
        }
        let id = l.add("Note", (10.0, 10.0, 40.0, 1400.0), kind);
        (l, id)
    };
    let (l, id) = page("\n\u{a0}\nZ");
    assert_eq!(clipped_text(&l, &[], 96.0, &info), vec![(id, "Note".into())]);
    let (l, _) = page("\n\n\n");
    assert!(clipped_text(&l, &[], 96.0, &info).is_empty(), "only blank lines past it");
    // Counted as more than the one line seen: the wrapped text was broken no further.
    let (l, id) = page("\nZ");
    let loss = text_losses(&l, &[], 96.0, &info);
    assert_eq!(
        loss,
        vec![TextLoss {
            id,
            name: "Note".into(),
            left_out: 1,
            at_least: true,
            below_box: false,
            past_limit: true,
            off_page: false,
            unit_label: None
        }]
    );
    let said = lines_left_out_notice(&loss[0]).unwrap();
    assert!(said.starts_with("1 or more lines of the text \"Note\""), "{said}");
    assert!(said.ends_with("aren't drawn: a text draws at most 200 lines; split it into more than one text"), "{said}");
}

/// The lines of a text left out below its box are counted, blank ones not, and said in the
/// singular or the plural; a text whose lines all fit says nothing (#2346).
#[test]
fn a_text_counts_the_lines_left_out_of_its_box() {
    let info = PageInfo::default();
    let mut l = note_page("one\ntwo\n\nfour\nfive", false, TextAlign::Left, 0.0);
    let id = l.elements[0].id;
    let losses = |l: &Layout| text_losses(l, &[], 96.0, &info);
    assert!(losses(&l).is_empty(), "all fit");
    l.element_mut(id).unwrap().h = 2.0;
    let loss = losses(&l);
    assert_eq!(
        loss,
        vec![TextLoss {
            id,
            name: "Note".into(),
            left_out: 3,
            at_least: false,
            below_box: true,
            past_limit: false,
            off_page: false,
            unit_label: None
        }]
    );
    // The element alone is measured the same, and another id finds nothing.
    assert_eq!(text_loss(&l, &[], 96.0, &info, id).as_ref(), loss.first());
    assert_eq!(text_loss(&l, &[], 96.0, &info, id + 1), None);
    let said = lines_left_out_notice(&loss[0]).unwrap();
    assert!(said.starts_with(&format!("3 lines of the text \"Note\" ({id}) don't fit its box and aren't drawn")), "{said}");
    if let ElementKind::Text { text, .. } = &mut l.element_mut(id).unwrap().kind {
        *text = "one\ntwo".into();
    }
    let said = lines_left_out_notice(&losses(&l)[0]).unwrap();
    assert!(said.starts_with(&format!("1 line of the text \"Note\" ({id}) doesn't fit its box and isn't drawn")), "{said}");
    // Off the page but all drawn: cut, with no line left out to count.
    let e = l.element_mut(id).unwrap();
    (e.h, e.x) = (60.0, 99.0);
    let loss = losses(&l);
    assert_eq!(loss.len(), 1);
    assert_eq!(lines_left_out_notice(&loss[0]), None);
}

/// A text whose ink crosses the page's edge is told apart from its lines left out, with the
/// advice that fixes an edge: moved past each side it is, back inside or turned within the
/// page it isn't, and one cut below its box and past the edge has both (#2703).
#[test]
fn a_text_past_the_pages_edge_is_told_with_its_advice() {
    let info = PageInfo::default();
    let mut l = note_page("Note", false, TextAlign::Left, 0.0);
    let id = l.elements[0].id;
    let put = |l: &mut Layout, (x, y): (f64, f64)| {
        if let Some(e) = l.element_mut(id) {
            (e.x, e.y) = (x, y);
        }
    };
    let turn = |l: &mut Layout, degrees: f64| {
        if let Some(ElementKind::Text { rotation, .. }) = l.element_mut(id).map(|e| &mut e.kind) {
            *rotation = degrees;
        }
    };
    let told = |l: &Layout| text_loss(l, &[], 96.0, &info, id).and_then(|loss| off_page_notice(&loss));
    let said = format!(
        "The text \"Note\" ({id}) runs off the page, so part of it isn't drawn; move it inside the page, resize it or reduce its size"
    );
    assert_eq!(told(&l), None, "inside the page");
    // Past each edge.
    for at in [(98.0, 10.0), (10.0, 99.0), (-30.0, 10.0), (10.0, -5.0)] {
        put(&mut l, at);
        let loss = text_loss(&l, &[], 96.0, &info, id).unwrap_or_else(|| panic!("{at:?} is past an edge"));
        assert!(loss.off_page && loss.left_out == 0 && !loss.below_box, "{at:?} {loss:?}");
        assert_eq!(off_page_notice(&loss), Some(said.clone()), "{at:?}");
        assert_eq!(lines_left_out_notice(&loss), None, "{at:?}: no line is left out");
        put(&mut l, (10.0, 10.0));
        assert_eq!(told(&l), None, "{at:?} moved back inside");
    }
    // A text that doesn't cross says nothing, whatever else is lost.
    let inside = TextLoss {
        id,
        name: "Note".into(),
        left_out: 1,
        at_least: false,
        below_box: true,
        past_limit: false,
        off_page: false,
        unit_label: None,
    };
    assert_eq!(off_page_notice(&inside), None);
    // Turned within the page: nothing. Turned about its box's centre past the edge: told.
    put(&mut l, (30.0, 10.0));
    turn(&mut l, 90.0);
    assert_eq!(told(&l), None, "turned within the page");
    turn(&mut l, 180.0);
    assert_eq!(told(&l), None, "turned within the page");
    put(&mut l, (80.0, 10.0));
    turn(&mut l, 0.0);
    assert_eq!(told(&l), None, "unturned, it is inside");
    turn(&mut l, 180.0);
    assert_eq!(told(&l), Some(said.clone()), "turned about its box's centre, past the edge");
    // Cut below its box and past the edge: each is told, apart.
    let mut l = note_page("one\ntwo\nthree", false, TextAlign::Left, 0.0);
    let id = l.elements[0].id;
    if let Some(e) = l.element_mut(id) {
        (e.x, e.h) = (98.0, 2.0);
    }
    let loss = text_loss(&l, &[], 96.0, &info, id).unwrap_or_else(|| panic!("cut"));
    assert!(loss.below_box && loss.off_page && loss.left_out == 2, "{loss:?}");
    let (lines, edge) = (lines_left_out_notice(&loss).unwrap_or_default(), off_page_notice(&loss).unwrap_or_default());
    assert!(lines.contains("don't fit its box") && !lines.contains("page"), "{lines}");
    assert!(edge.contains("runs off the page") && !edge.contains("lines"), "{edge}");
    // No ink, nothing to cut: an empty or blank text, past the edge.
    for blank in ["", "  \n "] {
        let mut l = note_page(blank, false, TextAlign::Left, 0.0);
        let id = l.elements[0].id;
        if let Some(e) = l.element_mut(id) {
            e.x = 98.0;
        }
        assert_eq!(text_loss(&l, &[], 96.0, &info, id), None, "{blank:?} has no ink");
    }
    // A file can hold any place and size: measured without a panic, and told at most once.
    for (x, y, size) in
        [(1e300, -1e300, 12.0), (f64::NAN, f64::INFINITY, 12.0), (10.0, 10.0, f64::NAN), (10.0, 10.0, 1e300), (98.0, 10.0, 1e300)]
    {
        let mut l = note_page("Note", false, TextAlign::Left, 0.0);
        let id = l.elements[0].id;
        if let Some(e) = l.element_mut(id) {
            (e.x, e.y) = (x, y);
            if let ElementKind::Text { size: s, .. } = &mut e.kind {
                *s = size;
            }
        }
        let losses = text_losses(&l, &[], 96.0, &info);
        assert!(losses.len() <= 1 && losses.iter().all(|loss| off_page_notice(loss).iter().count() <= 1), "{x} {y} {size}");
    }
}

/// A long text counts every line left out below its box, each only looked at for a letter
/// past the box: none is shaped there (#2346).
#[test]
fn a_long_text_counts_every_line_left_out() {
    let info = PageInfo::default();
    let mut l = note_page(&vec!["x"; 3000].join("\n"), false, TextAlign::Left, 0.0);
    let id = l.elements[0].id;
    l.element_mut(id).unwrap().h = 2.0;
    let loss = text_losses(&l, &[], 96.0, &info);
    assert_eq!(
        loss,
        vec![TextLoss {
            id,
            name: "Note".into(),
            left_out: 2999,
            at_least: false,
            below_box: true,
            past_limit: true,
            off_page: false,
            unit_label: None
        }]
    );
    let said = lines_left_out_notice(&loss[0]).unwrap();
    assert!(said.ends_with("make the box taller or the text smaller, and split it, as a text draws at most 200 lines"), "{said}");
    // A box tall enough for 200 lines: the rest are past the limit, which no box draws (#2346).
    let mut tall = note_page(&vec!["x"; 250].join("\n"), false, TextAlign::Left, 0.0);
    tall.height = 2000.0;
    tall.element_mut(id).unwrap().h = 1900.0;
    let loss = text_losses(&tall, &[], 96.0, &info);
    assert_eq!(
        loss,
        vec![TextLoss {
            id,
            name: "Note".into(),
            left_out: 50,
            at_least: false,
            below_box: false,
            past_limit: true,
            off_page: false,
            unit_label: None
        }]
    );
    let said = lines_left_out_notice(&loss[0]).unwrap();
    assert_eq!(
        said,
        format!("50 lines of the text \"Note\" ({id}) aren't drawn: a text draws at most 200 lines; split it into more than one text")
    );
    // Blank lines below the box and then one letter: that one line is left out.
    let mut l = note_page(&format!("x{}\nZ", "\n".repeat(2000)), false, TextAlign::Left, 0.0);
    l.element_mut(id).unwrap().h = 2.0;
    assert_eq!(
        text_losses(&l, &[], 96.0, &info),
        vec![TextLoss {
            id,
            name: "Note".into(),
            left_out: 1,
            at_least: false,
            below_box: false,
            past_limit: true,
            off_page: false,
            unit_label: None
        }]
    );
}

/// The cut-off notice suggests breaking lines or wrapping them for a text not wrapped, and
/// only the box or the size when every text it names is wrapped already (#2346).
#[test]
fn the_cut_off_notice_suggests_wrapping_a_text_not_wrapped() {
    let info = PageInfo::default();
    for wrap in [false, true] {
        let mut l = note_page(NOTE, wrap, TextAlign::Left, 0.0);
        let id = l.elements[0].id;
        l.element_mut(id).unwrap().h = 2.0;
        let note = clipped_text_notice(&l, &clipped_text(&l, &[], 96.0, &info)).unwrap();
        assert!(note.starts_with(&format!("Text is cut off in 1 layout element(s): Note ({id}); ")), "{note}");
        assert_eq!(note.contains("Wrap to the box"), !wrap, "{note}");
        assert_eq!(note.contains("add line breaks"), !wrap, "{note}");
        assert!(note.contains("reduce its size"), "{note}");
    }
    // A legend's title, or an element gone since, has no wrap to turn on: line breaks, as before.
    let l = note_page(NOTE, true, TextAlign::Left, 0.0);
    let note = clipped_text_notice(&l, &[(999, "Gone".into())]).unwrap();
    assert!(note.ends_with("resize or move the text, reduce its size, or add line breaks"), "{note}");
    let both = clipped_text_notice(&l, &[(l.elements[0].id, "Note".into()), (999, "Gone".into())]).unwrap();
    assert!(!both.contains("Wrap to the box") && both.contains("add line breaks"), "{both}");
    assert_eq!(clipped_text_notice(&Layout::new("L", 10.0, 10.0), &[]), None);
}

/// The same lines at every resolution: they are measured at one (#2346), so the layout view,
/// an image export and a print break a text alike.
#[test]
fn a_wrapped_text_breaks_alike_at_every_resolution() {
    let style = TextStyle { face: text::Face::default(), align: TextAlign::Left, spacing: 0.0, valign: TextVAlign::Top, line_spacing: 1.0 };
    let lines = wrapped(NOTE, 40.0, 12.0, style);
    assert!(lines.lines().count() > 3, "{lines}");
    assert!(lines.lines().all(|l| !l.starts_with(' ') && !l.ends_with(' ')), "{lines:?}");
    assert_eq!(lines.replace('\n', " "), NOTE, "broken only at its spaces");
    let tall = |dpi: f64| {
        let pm = render(&note_page(NOTE, true, TextAlign::Left, 0.0), &mut [], dpi, &PageInfo::default()).unwrap();
        let (_, t, _, b) = ink_bounds(&pm);
        f64::from(b - t) / dpi
    };
    assert!((tall(96.0) - tall(300.0)).abs() < 0.03, "{} against {} inches", tall(96.0), tall(300.0));
}

/// A text turned 90° runs down the page from the top of its turned box, -90° up it from the
/// bottom, about the centre of its box (#1763); upright, it runs across as before.
#[test]
fn a_turned_text_runs_down_or_up_the_page() {
    // 100 mm at 96 dpi; the box's centre at 50 mm.
    let centre = 50.0 * 96.0 / 25.4;
    let ink = |turn: f64| ink_bounds(&render(&turned_text_page(turn), &mut [], 96.0, &PageInfo::default()).unwrap());
    let (l, t, r, b) = ink(0.0);
    assert!(r - l > 4 * (b - t), "upright runs across: {l},{t} to {r},{b}");
    for turn in [90.0, -90.0] {
        let (l, t, r, b) = ink(turn);
        assert!(b - t > 4 * (r - l), "{turn}° runs along the page: {l},{t} to {r},{b}");
        // Across: within the turned box, 10 mm wide about the centre.
        assert!((f64::from(l + r) / 2.0 - centre).abs() < 10.0 * 96.0 / 25.4 / 2.0, "{turn}°: {l}..{r}");
        // Along: a left-aligned line starts at the box's left edge, which a turn of 90° takes to
        // the top and one of -90° to the bottom.
        let middle = f64::from(t + b) / 2.0;
        assert!(if turn > 0.0 { middle < centre } else { middle > centre }, "{turn}°: {t}..{b} about {centre}");
    }
}

/// A quarter-turned text keeps as sharp as an upright one: its picture is put on whole pixels,
/// not filtered between them, so its edges have no more part-covered pixels (#1763). At 300 dpi
/// the box's corner falls between pixels (10 mm is 118.1 of them).
#[test]
fn a_quarter_turned_text_is_as_sharp_as_upright() {
    // Solid pixels (darker than 80 %) and part-covered ones (20 to 80 %).
    let edges = |turn: f64| {
        let pm = render(&turned_text_page(turn), &mut [], 300.0, &PageInfo::default()).unwrap();
        let dark = (0..pm.height()).flat_map(|y| (0..pm.width()).map(move |x| (x, y))).map(|(x, y)| 255 - u32::from(rgb(&pm, x, y).0));
        dark.fold((0u32, 0u32), |(solid, part), d| (solid + u32::from(d > 204), part + u32::from(d > 51 && d <= 204)))
    };
    let (solid, part) = edges(0.0);
    let upright = f64::from(part) / f64::from(solid);
    for turn in [90.0, -90.0, 180.0] {
        let (s, p) = edges(turn);
        let turned = f64::from(p) / f64::from(s);
        assert!(turned < upright * 1.2, "{turn}°: {p} part-covered to {s} solid, upright {part} to {solid}");
        assert!(s.abs_diff(solid) * 10 < solid, "{turn}°: as much ink, {s} solid to {solid}");
    }
}

/// A turned text's picture covers the lines it draws, not its whole box: a short line in a
/// large box takes a small picture, where its alignment puts the line (#1763).
#[test]
fn a_turned_texts_picture_covers_its_lines() {
    let r = Rect::from_xywh(0.0, 0.0, 2000.0, 1000.0).unwrap();
    let plain = TextStyle { face: text::Face::default(), align: TextAlign::Left, spacing: 0.0, valign: TextVAlign::Top, line_spacing: 1.0 };
    let px = 14.0 * 1.33;
    let area = |align| text_area(r, "Fonte: IBGE", 14.0, TextStyle { align, ..plain }, 1.0).unwrap();
    let left = area(TextAlign::Left);
    assert!(left.width() < 200.0 && left.height() < 4.0 * px, "{left:?}");
    assert!(left.left() < 0.0 && left.top() < 0.0, "a line's height of room: {left:?}");
    let right = area(TextAlign::Right);
    assert!((right.right() - (2000.0 + px)).abs() < 1.0 && (right.width() - left.width()).abs() < 1.0, "{right:?}");
    let centre = area(TextAlign::Center);
    assert!(((centre.left() + centre.right()) / 2.0 - 1000.0).abs() < 1.0, "{centre:?}");
    // Two lines, twice the pitch; nothing when there is no line.
    let two = text_area(
        r,
        "Fonte: IBGE
Elaboração: RustGIS",
        14.0,
        plain,
        1.0,
    )
    .unwrap();
    assert!((two.height() - left.height() - px * 1.25).abs() < 0.01);
    assert!(text_area(r, "", 14.0, plain, 1.0).is_none());
}

/// A turned box past the page's budget is still drawn turned, at a lower resolution, not
/// left out or left upright (#1763).
#[test]
fn a_huge_turned_box_is_drawn_at_a_lower_resolution() {
    let mut pm = Canvas::new(200, 200).unwrap();
    // 20,000 px long (past MAX_SIDE), 20 high, about the canvas's centre.
    let r = Rect::from_xywh(-9_900.0, 90.0, 20_000.0, 20.0).unwrap();
    let mut scale = 0.0;
    let area = Rect::from_xywh(0.0, 0.0, r.width(), r.height()).unwrap();
    draw_turned_box(&mut pm, r, 90.0, area, |c, inner, k| {
        scale = k;
        c.fill_rect(inner, &paint(Rgba::rgb(0, 0, 0), 1.0), Transform::identity(), None);
    });
    assert!(scale < 1.0 && scale > 0.0, "{scale}");
    // Turned upright: a bar down the middle, top to bottom; nothing beside it.
    let dark = |x: u32, y: u32| pm.pixmap().pixel(x, y).is_some_and(|p| p.alpha() > 128);
    assert!(dark(100, 2) && dark(100, 197) && dark(95, 100), "the bar runs down the page");
    assert!(!dark(70, 100) && !dark(130, 100));
}

/// A turned text is kept as turned glyphs, which draw what its picture shows (#1763).
#[test]
fn a_turned_text_is_kept_turned_as_vectors() {
    use crate::vector::Item;
    let layout = turned_text_page(90.0);
    let info = PageInfo::default();
    let png = render(&layout, &mut [], 96.0, &info).unwrap();
    let (drawing, _) = render_drawing(&layout, &mut [], 96.0, &info).unwrap();
    let (_, _, items) = drawing.finish();
    let items = crate::vector::without_layers(items);
    let text: String = items
        .iter()
        .filter_map(|i| if let Item::Glyphs(r) = i { Some(r) } else { None })
        .flat_map(|r| r.glyphs.iter().map(|g| g.ch))
        .collect();
    assert_eq!(text, "Fonte: IBGE, 2022", "kept as text, not a picture");
    let mut replayed = Pixmap::new(png.width(), png.height()).unwrap();
    replayed.fill(tiny_skia::Color::WHITE);
    crate::vector::replay(&items, &mut replayed, Transform::identity());
    let (pl, pt, pr, pb) = ink_bounds(&png);
    let (vl, vt, vr, vb) = ink_bounds(&replayed);
    assert!(
        pl.abs_diff(vl) <= 2 && pt.abs_diff(vt) <= 2 && pr.abs_diff(vr) <= 2 && pb.abs_diff(vb) <= 2,
        "{pl},{pt},{pr},{pb} vs {vl},{vt},{vr},{vb}"
    );
}

/// Text elements take italic, alignment and letter spacing (#213, point c); plain, they draw
/// as before.
#[test]
fn text_takes_italic_alignment_and_letter_spacing() {
    let plain = TextStyle { face: text::Face::default(), align: TextAlign::Left, spacing: 0.0, valign: TextVAlign::Top, line_spacing: 1.0 };
    let base = styled_line(plain);
    let (l, r, ..) = text_ink(&base);
    assert!(l < 4, "left-aligned starts at the left: {l}");
    // Plain is what the old upright, left-aligned drawing gave.
    let mut old = Pixmap::new(400, 60).unwrap();
    old.fill(tiny_skia::Color::WHITE);
    let px = 20.0 * 1.33;
    crate::canvas::with_canvas(&mut old, |c| {
        text::text_at(c, "Oceano Atlântico", 0.0, px * 0.6, px, Rgba::rgb(0, 0, 0), false, text::Align::Left, false)
    });
    assert_eq!(text_ink(&old).0.abs_diff(l), 0);
    assert_eq!(text_ink(&old).1.abs_diff(r), 0);
    // Right and centre: the same width, moved.
    let (rl, rr, ..) = text_ink(&styled_line(TextStyle { align: TextAlign::Right, ..plain }));
    assert!((397..=399).contains(&rr) && (rr - rl).abs_diff(r - l) <= 1, "right-aligned ends at the right: {rl}..{rr}");
    let (cl, cr, ..) = text_ink(&styled_line(TextStyle { align: TextAlign::Center, ..plain }));
    assert!(cl.abs_diff(400 - cr) <= 2, "centred: {cl}..{cr}");
    // Spacing: 15 gaps of 3 pt widen the line by about 15 × 3 × 1.33 px.
    let (sl, sr, ..) = text_ink(&styled_line(TextStyle { spacing: 3.0, ..plain }));
    let grown = (sr - sl) as f32 - (r - l) as f32;
    assert!((grown - 15.0 * 3.0 * 1.33).abs() < 3.0, "{grown}");
    // Italic: a straight stem (an `l`) leans right, its top about tan 12° of its height
    // right of its foot; upright, it stands straight.
    let stem = |italic: bool| {
        let mut pm = Pixmap::new(120, 120).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(40.0, 0.0, 80.0, 120.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| {
            text_block(c, r, "l", 60.0, Rgba::rgb(0, 0, 0), TextStyle { face: text::Face { italic, ..plain.face }, ..plain }, 1.0)
        });
        let (.., top, bottom) = text_ink(&pm);
        (top as i32 - bottom as i32, text_height(&pm))
    };
    let ((upright, _), (lean, height)) = (stem(false), stem(true));
    assert!(upright.abs() <= 1, "upright stem: {upright}");
    assert!((lean as f32 - 0.2126 * height as f32).abs() <= 3.0, "lean {lean} over {height} px");
    // Right-aligned, slanted or tightened past the letters' width, the ink stays in the box.
    for style in [
        TextStyle { align: TextAlign::Right, face: text::Face { italic: true, ..plain.face }, ..plain },
        TextStyle { align: TextAlign::Right, spacing: -5.0, ..plain },
    ] {
        let mut pm = Pixmap::new(400, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| text_block(c, r, "Oceano Atlântico", 20.0, Rgba::rgb(0, 0, 0), style, 1.0));
        let (l, r, ..) = text_ink(&pm);
        assert!((296..=300).contains(&r) && l < r, "inked {l}..{r} in a 300 px box");
    }
    // 2 pt letters at -5 pt run backwards: still drawn, inside the box.
    let mut pm = Pixmap::new(400, 60).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
    crate::canvas::with_canvas(&mut pm, |c| {
        text_block(c, r, "MMMM", 2.0, Rgba::rgb(0, 0, 0), TextStyle { align: TextAlign::Right, spacing: -5.0, ..plain }, 4.0)
    });
    let (l, r, ..) = text_ink(&pm);
    assert!(r <= 300 && l < r, "inked {l}..{r}");
}

/// A 100 × 100 mm page with `text` at 12 pt in black in (10, 10, 40, `h`) mm, its kind changed
/// by `f`.
fn block_page(text: &str, h: f64, f: &dyn Fn(&mut ElementKind)) -> Layout {
    let mut l = Layout::new("L", 100.0, 100.0);
    let mut kind = ElementKind::text(text, 12.0, Rgba::rgb(0, 0, 0), false);
    f(&mut kind);
    l.add("Block", (10.0, 10.0, 40.0, h), kind);
    l
}

/// Sets a text's block alignment, line spacing and case.
fn set_block(k: &mut ElementKind, v: TextVAlign, ls: f64, c: TextCase) {
    if let ElementKind::Text { valign, line_spacing, case, .. } = k {
        (*valign, *line_spacing, *case) = (v, ls, c);
    }
}

/// The rows with a dark pixel, from the top.
fn ink_rows(pm: &Pixmap) -> Vec<u32> {
    (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| rgb(pm, x, y).0 < 128)).collect()
}

/// How many separate runs of columns with a dark pixel there are, from the left.
fn ink_column_runs(pm: &Pixmap) -> usize {
    let inked: Vec<bool> = (0..pm.width()).map(|x| (0..pm.height()).any(|y| rgb(pm, x, y).0 < 128)).collect();
    inked.windows(2).filter(|w| !w[0] && w[1]).count() + usize::from(inked.first() == Some(&true))
}

/// A text's block sits at the top of its box, as texts always have, in its middle, or with its
/// last line on its bottom edge; the block itself is the same, moved.
#[test]
fn a_text_block_sits_at_the_top_middle_or_bottom_of_its_box() {
    let draw = |v: TextVAlign| {
        render(&block_page("one\ntwo\nthree", 60.0, &|k| set_block(k, v, 1.0, TextCase::None)), &mut [], 96.0, &PageInfo::default())
            .unwrap()
    };
    let (top, mid, bottom) = (ink_rows(&draw(TextVAlign::Top)), ink_rows(&draw(TextVAlign::Middle)), ink_rows(&draw(TextVAlign::Bottom)));
    let plain = ink_rows(&render(&block_page("one\ntwo\nthree", 60.0, &|_| {}), &mut [], 96.0, &PageInfo::default()).unwrap());
    assert_eq!(top, plain, "top is today's drawing");
    let span = |r: &[u32]| r[r.len() - 1] - r[0];
    // Moved by part of a pixel, its edge rows may shade past the threshold one row more or less.
    for moved in [&mid, &bottom] {
        assert!(span(moved).abs_diff(span(&top)) <= 1, "the same block, moved: {} and {}", span(moved), span(&top));
    }
    let (box_top, box_bottom) = ((10.0 * 96.0 / 25.4) as u32, (70.0 * 96.0 / 25.4) as u32);
    assert!(box_bottom - bottom[bottom.len() - 1] <= 12, "bottom: last ink row {} of the box's {box_bottom}", bottom[bottom.len() - 1]);
    let (above, below) = (mid[0] - box_top, box_bottom - mid[mid.len() - 1]);
    assert!(above.abs_diff(below) <= 8, "middle: {above} above, {below} below");
}

/// Line spacing multiplies the distance between lines: twice as far apart at 2, half at 0.5.
#[test]
fn line_spacing_spreads_the_lines_by_its_multiple() {
    let span = |ls: f64| {
        let pm = render(
            &block_page("one\ntwo\nthree", 60.0, &|k| set_block(k, TextVAlign::Top, ls, TextCase::None)),
            &mut [],
            96.0,
            &PageInfo::default(),
        )
        .unwrap();
        let r = ink_rows(&pm);
        (r[r.len() - 1] - r[0]) as f64
    };
    // The usual pitch in pixels at 96 dpi, a layout point a pixel there.
    let pitch = 12.0 * 1.33 * 1.25;
    assert!(((span(2.0) - span(1.0)) - 2.0 * pitch).abs() <= 2.0, "{} {}", span(2.0), span(1.0));
    assert!(((span(1.0) - span(0.5)) - pitch).abs() <= 2.0, "{} {}", span(1.0), span(0.5));
}

/// A block taller than its box is drawn from the top whatever its alignment: its first lines
/// are kept, the same lines left out and said, and none is drawn above the box.
#[test]
fn a_block_taller_than_its_box_keeps_its_first_lines_whatever_its_valign() {
    let info = PageInfo::default();
    let page = |v| block_page("one\ntwo\nthree\nfour\nfive", 7.0, &|k| set_block(k, v, 1.0, TextCase::None));
    let top = page(TextVAlign::Top);
    let loss = text_losses(&top, &[], 96.0, &info);
    assert_eq!(loss.iter().map(|l| l.left_out).collect::<Vec<_>>(), [3]);
    for v in [TextVAlign::Middle, TextVAlign::Bottom] {
        let l = page(v);
        assert_eq!(render(&l, &mut [], 96.0, &info).unwrap().data(), render(&top, &mut [], 96.0, &info).unwrap().data(), "{v:?}");
        assert_eq!(text_losses(&l, &[], 96.0, &info), loss, "{v:?}");
        assert_eq!(lines_left_out_notice(&text_losses(&l, &[], 96.0, &info)[0]), lines_left_out_notice(&loss[0]));
    }
}

/// A text of more lines than a text draws is cut at them, so it is drawn from the top whatever
/// its alignment, though the lines kept would fit its box; one of exactly that many, all
/// drawn, is still aligned.
#[test]
fn a_text_cut_at_the_most_lines_it_draws_starts_at_the_top() {
    let info = PageInfo::default();
    // 2 pt lines are 0.88 mm apart: 200 of them fill 176 mm of a 200 mm box.
    let page = |lines: usize, v| {
        let text = (0..lines).map(|i| format!("line {i}")).collect::<Vec<_>>().join(
            "
",
        );
        let mut l = Layout::new("L", 100.0, 300.0);
        let mut kind = ElementKind::text(&text, 2.0, Rgba::rgb(0, 0, 0), false);
        set_block(&mut kind, v, 1.0, TextCase::None);
        l.add("Block", (10.0, 10.0, 40.0, 200.0), kind);
        l
    };
    let drawn = |l: &Layout| render(l, &mut [], 96.0, &info).unwrap();
    let top = page(300, TextVAlign::Top);
    for v in [TextVAlign::Middle, TextVAlign::Bottom] {
        let l = page(300, v);
        assert_eq!(drawn(&l).data(), drawn(&top).data(), "{v:?}");
        assert_eq!(text_losses(&l, &[], 96.0, &info), text_losses(&top, &[], 96.0, &info), "{v:?}");
        let svg = |l: &Layout| crate::svg::layout_svg(l, &mut [], 96.0, &info).unwrap().0;
        assert_eq!(svg(&l), svg(&top), "{v:?}");
        assert_ne!(drawn(&page(200, v)).data(), drawn(&page(200, TextVAlign::Top)).data(), "{v:?}: all drawn, aligned");
    }
    let text = |n: usize| {
        vec!["x"; n].join(
            "
",
        )
    };
    assert_eq!(text_shift(TextVAlign::Bottom, 5000.0, 10.0, &text(201), 1.0), 0.0);
    assert_eq!(text_shift(TextVAlign::Bottom, 5000.0, 10.0, &text(200), 1.0), block_shift(TextVAlign::Bottom, 5000.0, 10.0, 200, 1.0));
    assert!(text_shift(TextVAlign::Bottom, 5000.0, 10.0, &text(200), 1.0) > 0.0);
}

/// The cut-off check steps by the spaced pitch, as the page draws: a wider spacing leaves lines
/// out of a box that held them, a tighter one draws lines a box cut.
#[test]
fn line_spacing_changes_the_lines_left_out() {
    let info = PageInfo::default();
    let left_out = |h: f64, ls: f64| {
        let l = block_page("one\ntwo\nthree\nfour\nfive", h, &|k| set_block(k, TextVAlign::Top, ls, TextCase::None));
        text_losses(&l, &[], 96.0, &info).first().map_or(0, |l| l.left_out)
    };
    assert_eq!((left_out(25.0, 1.0), left_out(25.0, 2.0)), (0, 2));
    assert_eq!((left_out(12.0, 1.0), left_out(12.0, 0.5)), (2, 0));
}

/// Case applies once the tokens are filled in, and before the wrap, which breaks the wider
/// words.
#[test]
fn case_applies_after_the_tokens_and_before_the_wrap() {
    let info = PageInfo { page: 2, pages: 3, page_name: String::new() };
    let draw = |t: &str, c| render(&block_page(t, 20.0, &|k| set_block(k, TextVAlign::Top, 1.0, c)), &mut [], 96.0, &info).unwrap();
    assert_eq!(draw("Page {page} of {pages}", TextCase::Upper).data(), draw("PAGE 2 OF 3", TextCase::None).data());
    assert_eq!(draw("PAGE {page} OF {pages}", TextCase::Lower).data(), draw("page 2 of 3", TextCase::None).data());
    let style = text_style(false, false, rustgis_map::layout::FontFamily::Sans, None, TextAlign::Left, 0.0);
    let s = "boundaries simplified for print from the census".to_string();
    let lines = |c| drawn_text(s.clone(), c, true, 40.0, 12.0, style).lines().count();
    assert!(lines(TextCase::Upper) > lines(TextCase::Lower), "{} {}", lines(TextCase::Upper), lines(TextCase::Lower));
    assert_eq!(drawn_text(s.clone(), TextCase::None, false, 40.0, 12.0, style), s, "unwrapped and as typed: unchanged");
}

/// A turned text's picture covers its lines where its block alignment and spacing put them.
#[test]
fn a_turned_bottom_aligned_text_is_drawn_whole() {
    let info = PageInfo::default();
    let ink = |v, ls| {
        let pm = render(
            &block_page("one\ntwo\nthree", 60.0, &|k| {
                set_block(k, v, ls, TextCase::None);
                if let ElementKind::Text { rotation, .. } = k {
                    *rotation = 90.0;
                }
            }),
            &mut [],
            96.0,
            &info,
        )
        .unwrap();
        (ink_bounds(&pm), ink_column_runs(&pm))
    };
    // Spaced alike, so only the alignment differs. Turned a quarter clockwise, the box's
    // down runs leftwards across the page: its lines are runs of inked columns.
    let ((tl, tt, tr, tb), top_runs) = ink(TextVAlign::Top, 1.5);
    let ((bl, bt, br, bb), bottom_runs) = ink(TextVAlign::Bottom, 1.5);
    assert_eq!((top_runs, bottom_runs), (3, 3), "a run of columns a line, all three drawn");
    assert!((tr - tl).abs_diff(br - bl) <= 2, "the same block across: {tl}..{tr} and {bl}..{br}");
    assert!((tb - tt).abs_diff(bb - bt) <= 2, "the same lines along: {tt}..{tb} and {bt}..{bb}");
    // Moved left by the room the block leaves: the box's 60 mm less three lines at 1.5,
    // 12 pt × 1.33 × 1.25 × (1 + 2 × 1.5) = 79.8 px, of 226.8 px: 147.0 px.
    let free = 60.0 * 96.0 / 25.4 - 12.0 * 1.33 * 1.25 * (1.0 + 2.0 * 1.5);
    for (t, b) in [(tl, bl), (tr, br)] {
        assert!((f64::from(t) - f64::from(b) - free).abs() <= 2.0, "moved {} px of {free:.1}", f64::from(t) - f64::from(b));
    }
}

/// Where a text's block and case put its ink decides whether it runs off the page: a box
/// hanging past the page's bottom, its one line at the box's top, middle or bottom; and a line
/// near the page's right edge in upper case, its letters wider.
#[test]
fn a_text_block_and_case_are_said_off_the_page_where_they_put_the_ink() {
    let info = PageInfo::default();
    // The 100 mm page is 378.0 px tall at 96 dpi; the box (10, 80, 40, 30) mm runs from 302.4
    // to 415.7 px. Its one line of 12 pt (15.96 px a line, a pitch of 19.95) has its middle
    // 9.6 px under where the block starts, the block 19.95 px tall in a box of 113.4: at the
    // top at 312.0 px, in the middle 46.7 px lower at 358.7, at the bottom 93.4 px lower at
    // 405.4, below the page's 378.0 whatever the font's letters (some 8 px each way).
    let hanging = |v| {
        let mut l = Layout::new("L", 100.0, 100.0);
        let mut kind = ElementKind::text("one", 12.0, Rgba::rgb(0, 0, 0), false);
        set_block(&mut kind, v, 1.0, TextCase::None);
        l.add("Hanging", (10.0, 80.0, 40.0, 30.0), kind);
        text_losses(&l, &[], 96.0, &info)
    };
    assert_eq!(hanging(TextVAlign::Top), [], "at the top, on the page");
    assert_eq!(hanging(TextVAlign::Middle), [], "in the middle, still on the page");
    let bottom = hanging(TextVAlign::Bottom);
    assert!(bottom.len() == 1 && bottom[0].off_page && bottom[0].left_out == 0, "{bottom:?}");
    assert!(off_page_notice(&bottom[0]).is_some());
    // The page is 378.0 px wide; the box starts at 83 mm, 313.7 px, 64.3 px from its edge.
    // Ten letters `lflflflflf` of 12 pt (an em of 15.96 px) take some 0.25 em each as typed,
    // about 40 px, and some 0.58 em in upper case (`LFLFLFLFLF`), about 93 px: on the page as
    // typed with room for 1.6 times as wide letters, past its edge in upper case at 0.69 times
    // as wide ones.
    let near_edge = |c| {
        let mut l = Layout::new("L", 100.0, 100.0);
        let mut kind = ElementKind::text("lflflflflf", 12.0, Rgba::rgb(0, 0, 0), false);
        set_block(&mut kind, TextVAlign::Top, 1.0, c);
        l.add("Edge", (83.0, 50.0, 10.0, 10.0), kind);
        text_losses(&l, &[], 96.0, &info)
    };
    assert_eq!(near_edge(TextCase::None), [], "as typed, on the page");
    let upper = near_edge(TextCase::Upper);
    assert!(upper.len() == 1 && upper[0].off_page && upper[0].left_out == 0, "{upper:?}");
    assert!(off_page_notice(&upper[0]).is_some());
}

/// A block alignment or line spacing that reaches the page unrepaired is clamped where it is
/// used: an unknown alignment draws at the top, a spacing not a finite number (an infinity too)
/// at the usual one, one out of range at the nearer limit; none moves a line to a place not a
/// number.
#[test]
fn hostile_text_block_settings_draw_clamped() {
    let style = text_style(false, false, rustgis_map::layout::FontFamily::Sans, None, TextAlign::Left, 0.0);
    let kept = |v, ls: f64| {
        let s = style.with_block(v, ls);
        (s.valign, s.line_spacing)
    };
    assert_eq!(kept(TextVAlign::Invalid, f64::NAN), (TextVAlign::Top, 1.0));
    assert_eq!(kept(TextVAlign::Bottom, 9.0), (TextVAlign::Bottom, 3.0));
    assert_eq!(kept(TextVAlign::Middle, f64::INFINITY), (TextVAlign::Middle, 1.0));
    assert_eq!(kept(TextVAlign::Middle, f64::NEG_INFINITY), (TextVAlign::Middle, 1.0));
    assert_eq!(kept(TextVAlign::Top, 0.1), (TextVAlign::Top, 0.5));
    for (v, box_h, px, ls) in [
        (TextVAlign::Bottom, f64::NAN, 10.0, 1.0),
        (TextVAlign::Middle, 100.0, f64::NAN, 1.0),
        (TextVAlign::Bottom, 100.0, 10.0, f64::NAN),
        (TextVAlign::Bottom, f64::INFINITY, 10.0, 1.0),
        (TextVAlign::Invalid, 100.0, 10.0, 1.0),
    ] {
        assert_eq!(block_shift(v, box_h, px, 3, ls), 0.0, "{v:?} {box_h} {px} {ls}");
    }
    assert_eq!(block_shift(TextVAlign::Bottom, 100.0, 10.0, 0, 1.0), 0.0, "no line");
    let info = PageInfo::default();
    let draw = |v, ls| render(&block_page("one\ntwo", 60.0, &|k| set_block(k, v, ls, TextCase::Invalid)), &mut [], 96.0, &info).unwrap();
    assert_eq!(draw(TextVAlign::Invalid, f64::NAN).data(), draw(TextVAlign::Top, 1.0).data());
    assert_eq!(draw(TextVAlign::Bottom, 1e300).data(), draw(TextVAlign::Bottom, 3.0).data());
    let l = block_page("one\ntwo", 60.0, &|k| set_block(k, TextVAlign::Bottom, f64::NAN, TextCase::Invalid));
    assert!(text_losses(&l, &[], 96.0, &info).is_empty());
}

/// The extent indicator is drawn in its own colour and width (#213): red and 2 pt until set.
#[test]
fn extent_indicators_take_their_colour_and_width() {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH};
    let blue = Rgba::rgb(0x1F, 0x4E, 0x9A);
    let default = indicator_page(INDICATOR_COLOR, INDICATOR_WIDTH);
    assert!(inset_pixels(&default, INDICATOR_COLOR) > 0, "red by default");
    let thin = indicator_page(blue, 1.0);
    let thick = indicator_page(blue, 6.0);
    assert_eq!(inset_pixels(&thin, INDICATOR_COLOR), 0, "no red once restyled");
    let (t, w) = (inset_pixels(&thin, blue), inset_pixels(&thick, blue));
    assert!(t > 0 && w > 3 * t, "a wider outline covers more: {t} at 1 pt, {w} at 6 pt");
}

/// A map frame's border (#2557): dark grey and 0.8 pt by default, in its own colour and width
/// once set, none at a width of 0, and along the frame's shape, a round frame's leaving the
/// corners of its box clear.
#[test]
fn a_map_frame_border_takes_its_colour_and_width_or_none() {
    use rustgis_map::layout::{FRAME_BORDER_COLOR, FRAME_BORDER_WIDTH, INDICATOR_COLOR, INDICATOR_WIDTH};
    let page = |shape: FrameShape, border_color: Rgba, border_width: f64| {
        let mut map = Map::new("m");
        (map.basemap, map.graticule) = (rustgis_map::Basemap::Blank, false);
        let mut l = Layout::new("Layout", 100.0, 100.0);
        l.add(
            "Frame",
            (20.0, 20.0, 60.0, 60.0),
            ElementKind::MapFrame {
                map: "m".into(),
                center: Coord::new(0.0, 0.0),
                scale: 1e7,
                shape,
                grid: None,
                extent_of: None,
                extent_color: INDICATOR_COLOR,
                extent_width: INDICATOR_WIDTH,
                border_color,
                border_width,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        render(&l, &mut [map], 96.0, &PageInfo::default()).unwrap()
    };
    // Pixels of a tone (a line this thin is blended with the white page), over the whole page
    // or in the frame box's top-left corner.
    let count = |pm: &Pixmap, tone: fn(u8, u8, u8) -> bool, corner: bool| {
        let edge = (20.0 * PT_PER_MM + 6.0) as u32;
        let (w, h) = if corner { (edge, edge) } else { (pm.width(), pm.height()) };
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let (r, g, b) = rgb(pm, x, y);
                tone(r, g, b)
            })
            .count()
    };
    let grey: fn(u8, u8, u8) -> bool = |r, g, b| r.max(g).max(b) - r.min(g).min(b) < 12 && r < 240;
    let bluish: fn(u8, u8, u8) -> bool = |r, _, b| i32::from(b) - i32::from(r) > 30;
    let inked: fn(u8, u8, u8) -> bool = |r, g, b| r.min(g).min(b) < 240;
    let blue = Rgba::rgb(0x1F, 0x4E, 0x9A);
    let default = page(FrameShape::Rectangle, FRAME_BORDER_COLOR, FRAME_BORDER_WIDTH);
    assert!(count(&default, grey, false) > 0, "dark grey by default");
    assert_eq!(count(&default, bluish, false), 0);
    let thin = page(FrameShape::Rectangle, blue, 1.0);
    let thick = page(FrameShape::Rectangle, blue, 6.0);
    assert_eq!(count(&thin, grey, false), 0, "no grey once recoloured");
    let (t, w) = (count(&thin, bluish, false), count(&thick, bluish, false));
    assert!(t > 0 && w > 3 * t, "a wider border covers more: {t} at 1 pt, {w} at 6 pt");
    let none = page(FrameShape::Rectangle, blue, 0.0);
    assert_eq!(count(&none, inked, false), 0, "no border at 0");
    let clear = page(FrameShape::Rectangle, Rgba::new(0x1F, 0x4E, 0x9A, 0), 3.0);
    assert_eq!(count(&clear, inked, false), 0, "none in a clear colour");
    let round = page(FrameShape::Circle, blue, 3.0);
    assert!(count(&round, bluish, false) > 0, "a round frame has its border too");
    assert_eq!(count(&round, bluish, true), 0, "along the circle, not the box's corner");
    assert!(count(&thick, bluish, true) > 0, "a rectangle's border reaches its corner");
}

/// On a globe, the extent indicator is drawn where the globe shows the area, also away from
/// the disc's centre (#2348): through the flat camera, Svalbard 18° north of the centre came out
/// half as far again up, nearly 5 mm off on a 50 mm locator.
#[test]
fn an_extent_indicator_on_a_globe_lands_on_the_area() {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH, frame_camera};
    let (area, at, scale) = (Coord::new(20.0, 78.0), Coord::new(20.0, 60.0), 2e8);
    let mut globe = Map::new("g");
    globe.set_projection(rustgis_map::Projection::Globe);
    let maps = || [Map::new("m"), globe.clone()];
    let frame = |map: &str, center, scale, extent_of| ElementKind::MapFrame {
        map: map.into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: INDICATOR_COLOR,
        extent_width: INDICATOR_WIDTH,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let main = l.add("Main", (60.0, 60.0, 30.0, 30.0), frame("m", area, 2e7, None));
    l.add("Locator", (5.0, 5.0, 50.0, 50.0), frame("g", at, scale, Some(main)));
    let pm = render(&l, &mut maps(), 96.0, &PageInfo::default()).unwrap();
    // Where the globe draws the area, in pixels on the page.
    let side = 50.0 * PT_PER_MM;
    let g = rustgis_map::globe::Globe::new(&frame_camera(&globe, at, scale, 0.0, 0.0), side, side);
    let (x, y) = g.forward(area).unwrap();
    // At 96 dpi a layout point is a pixel.
    let expected = (5.0 * PT_PER_MM + x, 5.0 * PT_PER_MM + y);
    // The red pixels' middle, inside the locator.
    let px_mm = PT_PER_MM;
    let red: Vec<(f64, f64)> = ((5.0 * px_mm) as u32..(55.0 * px_mm) as u32)
        .flat_map(|y| ((5.0 * px_mm) as u32..(55.0 * px_mm) as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let (r, g, b) = rgb(&pm, x, y);
            r > 180 && g < 90 && b < 90
        })
        .map(|(x, y)| (f64::from(x), f64::from(y)))
        .collect();
    assert!(!red.is_empty(), "no extent indicator on the globe");
    let n = red.len() as f64;
    let mid = (red.iter().map(|p| p.0).sum::<f64>() / n, red.iter().map(|p| p.1).sum::<f64>() / n);
    let off = ((mid.0 - expected.0).powi(2) + (mid.1 - expected.1).powi(2)).sqrt() / px_mm;
    assert!(off < 1.5, "the indicator's middle {mid:?} is {off:.1} mm from the area at {expected:?}");
}

/// A frame on the globe is outlined by what it shows of the globe (#2348): zoomed in on Iceland,
/// around Iceland on a globe locator; seen whole, with no edge on the disc, not at all (through
/// the flat camera its edges were read as a flat world, a line drawn across the locator).
#[test]
fn a_frame_on_the_globe_is_outlined_by_what_it_shows() {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH, frame_camera};
    let mut globe = Map::new("g");
    globe.set_projection(rustgis_map::Projection::Globe);
    let frame = |center, scale, extent_of| ElementKind::MapFrame {
        map: "g".into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: INDICATOR_COLOR,
        extent_width: INDICATOR_WIDTH,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    let (at, scale) = (Coord::new(-20.0, 55.0), 2e8);
    // The red pixels inside the locator (5–55 mm), with the source frame on `center` at 1:`main`.
    let red = |center: Coord, main: f64| {
        let mut l = Layout::new("Layout", 100.0, 100.0);
        // 40 mm across: near the equator, wider than the smallest globe the camera draws there.
        let source = l.add("Main", (58.0, 58.0, 40.0, 40.0), frame(center, main, None));
        l.add("Locator", (5.0, 5.0, 50.0, 50.0), frame(at, scale, Some(source)));
        let pm = render(&l, &mut [globe.clone()], 96.0, &PageInfo::default()).unwrap();
        let k = PT_PER_MM;
        ((5.0 * k) as u32..(55.0 * k) as u32)
            .flat_map(|y| ((5.0 * k) as u32..(55.0 * k) as u32).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let (r, g, b) = rgb(&pm, x, y);
                r > 180 && g < 90 && b < 90
            })
            .map(|(x, y)| (f64::from(x), f64::from(y)))
            .collect::<Vec<_>>()
    };
    let iceland = red(Coord::new(-19.0, 65.0), 2e7);
    assert!(!iceland.is_empty(), "no outline of the zoomed-in frame");
    let n = iceland.len() as f64;
    let mid = (iceland.iter().map(|p| p.0).sum::<f64>() / n, iceland.iter().map(|p| p.1).sum::<f64>() / n);
    let side = 50.0 * PT_PER_MM;
    let g = rustgis_map::globe::Globe::new(&frame_camera(&globe, at, scale, 0.0, 0.0), side, side);
    let (x, y) = g.forward(Coord::new(-19.0, 65.0)).unwrap();
    let expected = (5.0 * PT_PER_MM + x, 5.0 * PT_PER_MM + y);
    let off = ((mid.0 - expected.0).powi(2) + (mid.1 - expected.1).powi(2)).sqrt() / PT_PER_MM;
    assert!(off < 1.5, "the outline's middle {mid:?} is {off:.1} mm from Iceland at {expected:?}");
    let whole = red(Coord::new(-20.0, 10.0), 4e8);
    assert!(whole.is_empty(), "the globe seen whole outlined by {} red pixels", whole.len());
}

/// A frame on the globe whose sides run off the disc is outlined by its top and bottom edges
/// only, broken where they leave the disc, not joined by a line across the globe (#2348), and
/// running up to the disc's rim (#2369).
#[test]
fn a_frame_partly_off_the_globe_is_outlined_without_a_chord() {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH, frame_camera};
    let mut globe = Map::new("g");
    globe.set_projection(rustgis_map::Projection::Globe);
    let frame = |center, scale, extent_of| ElementKind::MapFrame {
        map: "g".into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: INDICATOR_COLOR,
        extent_width: INDICATOR_WIDTH,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    let center = Coord::new(-20.0, 0.0);
    // The source frame, 40 × 20 mm: a scale whose disc is taller than the frame, its sides in
    // the sky beside the disc, and its corners too.
    let (sw, sh) = (40.0, 20.0);
    let disc = |scale: f64| {
        let cam = frame_camera(&globe, center, scale, 0.0, 0.0);
        rustgis_map::globe::Globe::new(&cam, sw * PT_PER_MM, sh * PT_PER_MM).disc_radius() / PT_PER_MM
    };
    let source_scale = (1..400).map(|i| f64::from(i) * 1e6).find(|&s| disc(s) < 0.5 * sw - 3.0).unwrap();
    assert!(disc(source_scale) > 0.5 * sh + 2.0, "a disc {:.1} mm across", 2.0 * disc(source_scale));
    let (at, scale) = (center, 2.0 * source_scale);
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let source = l.add("Main", (55.0, 70.0, sw, sh), frame(center, source_scale, None));
    l.add("Locator", (5.0, 5.0, 50.0, 50.0), frame(at, scale, Some(source)));
    let pm = render(&l, &mut [globe.clone()], 96.0, &PageInfo::default()).unwrap();
    let red = |x: u32, y: u32| {
        let (r, g, b) = rgb(&pm, x, y);
        r > 180 && g < 90 && b < 90
    };
    let k = PT_PER_MM;
    let (x0, x1) = ((5.0 * k) as u32, (55.0 * k) as u32);
    let inked = (x0..x1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| red(x, y)).count();
    assert!(inked > 0, "no outline of the frame");
    // The row through the source frame's centre, between its top and bottom edges: crossed only
    // by a chord.
    let side = 50.0 * k;
    let g = rustgis_map::globe::Globe::new(&frame_camera(&globe, at, scale, 0.0, 0.0), side, side);
    let (_, cy) = g.forward(center).unwrap();
    let row = (5.0 * k + cy).round() as u32;
    let crossed: Vec<u32> = (row - 2..=row + 2).flat_map(|y| (x0..x1).filter(move |&x| red(x, y))).collect();
    assert!(crossed.is_empty(), "the outline crosses the middle of the globe at {crossed:?}");
    // The top and bottom edges are outlined up to the rim, where they leave the disc, not up
    // to the last of their samples on it (#2369).
    let s = rustgis_map::globe::Globe::new(&frame_camera(&globe, center, source_scale, 0.0, 0.0), sw * k, sh * k);
    let (r, (sx0, sy0)) = (s.disc_radius(), s.forward(center).unwrap());
    let half = (r * r - sy0 * sy0).sqrt() - 1e-3;
    for (sx, sy) in [(sx0 - half, 0.0), (sx0 + half, 0.0), (sx0 - half, sh * k), (sx0 + half, sh * k)] {
        let on_rim = s.inverse(sx, sy).expect("a point just inside the rim is on the globe");
        let (x, y) = g.forward(on_rim).expect("the rim seen on the locator");
        let (x, y) = (5.0 * k + x, 5.0 * k + y);
        let gap = (x0..x1)
            .flat_map(|py| (x0..x1).map(move |px| (px, py)))
            .filter(|&(px, py)| red(px, py))
            .map(|(px, py)| (f64::from(px) - x).hypot(f64::from(py) - y))
            .fold(f64::INFINITY, f64::min)
            / k;
        assert!(gap < 0.3, "the outline ends {gap:.2} mm short of the rim at ({sx:.0}, {sy:.0}) on the frame");
    }
}

/// A frame on the globe across the antimeridian, outlined on a flat frame, stays one small box
/// near the flat frame's centre (#2348): its edge's longitudes kept continuous, not jumping from
/// 179.5° to -179.5° and drawing a line across the flat map, and brought to the flat frame's side
/// of ±180° when they start on the other.
#[test]
fn a_globe_frame_across_the_antimeridian_is_outlined_in_one_piece() {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH};
    let mut globe = Map::new("g");
    globe.set_projection(rustgis_map::Projection::Globe);
    let frame = |map: &str, center, scale, extent_of| ElementKind::MapFrame {
        map: map.into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: INDICATOR_COLOR,
        extent_width: INDICATOR_WIDTH,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    // The globe frame's centre and the flat locator's: on 180°, and a frame on -179° whose
    // first point (its top-left corner) is on the +180° side, on a locator on -175°.
    for (source_at, locator_at) in [(180.0, 180.0), (-179.0, -175.0)] {
        let mut l = Layout::new("Layout", 100.0, 100.0);
        // 40 mm at 1:20M, about 800 km across: 4 mm on the locator at 1:200M.
        let source = l.add("Main", (58.0, 58.0, 40.0, 40.0), frame("g", Coord::new(source_at, 0.0), 2e7, None));
        l.add("Locator", (5.0, 5.0, 50.0, 50.0), frame("m", Coord::new(locator_at, 0.0), 2e8, Some(source)));
        let pm = render(&l, &mut [globe.clone(), Map::new("m")], 96.0, &PageInfo::default()).unwrap();
        let k = PT_PER_MM;
        let (x0, x1) = ((5.0 * k) as u32, (55.0 * k) as u32);
        let red: Vec<(u32, u32)> = (x0..x1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let (r, g, b) = rgb(&pm, x, y);
                r > 180 && g < 90 && b < 90
            })
            .collect();
        assert!(!red.is_empty(), "{source_at}° on {locator_at}°: no outline of the frame");
        let middle = 30.0 * k;
        let far = red.iter().filter(|&&(x, y)| (f64::from(x) - middle).abs() > 6.0 * k || (f64::from(y) - middle).abs() > 6.0 * k).count();
        assert_eq!(far, 0, "{source_at}° on {locator_at}°: {far} of {} red pixels more than 6 mm from the middle", red.len());
    }
}

/// A frame of the map `map`, named "s", at `source` (centre, scale, width and height in mm)
/// outlined on a locator 90 × 80 mm of the map `locator`, named "m", at `view` (centre and
/// scale), on a page 200 × 120 mm: the outline's red pixels, in millimetres from the locator's
/// centre (x east, y south).
fn outline_on(map: Map, source: (Coord, f64, f64, f64), locator: Map, view: (Coord, f64)) -> Vec<(f64, f64)> {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH};
    let frame = |map: &str, center, scale, extent_of| ElementKind::MapFrame {
        map: map.into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: INDICATOR_COLOR,
        extent_width: INDICATOR_WIDTH,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    let (at, scale, w, h) = source;
    let mut l = Layout::new("Layout", 200.0, 120.0);
    let main = l.add("Main", (5.0, 5.0, w, h), frame("s", at, scale, None));
    l.add("Locator", (100.0, 5.0, 90.0, 80.0), frame("m", view.0, view.1, Some(main)));
    let pm = render(&l, &mut [map, locator], 96.0, &PageInfo::default()).unwrap();
    let k = PT_PER_MM;
    let (x0, x1, y0, y1) = ((100.0 * k) as u32, (190.0 * k) as u32, (5.0 * k) as u32, (85.0 * k) as u32);
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let (r, g, b) = rgb(&pm, x, y);
            r > 180 && g < 90 && b < 90
        })
        .map(|(x, y)| (f64::from(x) / k - 145.0, f64::from(y) / k - 45.0))
        .collect()
}

/// [`outline_on`] a flat world map at 1:600M centred on (0°, 0°).
fn outline_on_flat_world(map: Map, source: (Coord, f64, f64, f64)) -> Vec<(f64, f64)> {
    outline_on(map, source, Map::new("m"), (Coord::new(0.0, 0.0), 6e8))
}

/// A flat frame wider than what its locator's projection can show (UTM zone 31, 30° either
/// side of 3°E) is outlined there in pieces, broken where its edges leave what the locator
/// shows, not joined by a line across the locator (#2369).
#[test]
fn a_frame_wider_than_its_locators_projection_is_outlined_without_a_chord() {
    let mut utm = Map::new("m");
    utm.set_projection(rustgis_map::Projection::Custom("EPSG:32631".into()));
    // 80 × 20 mm at 1:175M on the equator, centred on 3°E: about 60°W to 66°E and 16°S to 16°N,
    // its sides outside the zone, its top and bottom edges crossing it.
    let red = outline_on(Map::new("s"), (Coord::new(3.0, 0.0), 1.75e8, 80.0, 20.0), utm, (Coord::new(3.0, 0.0), 1.5e8));
    assert!(!red.is_empty(), "no outline of the frame");
    // The equator's row is crossed only by a line joining the top edge to the bottom one.
    let across: Vec<_> = red.iter().filter(|p| p.1.abs() < 1.0).collect();
    assert!(across.is_empty(), "the outline crosses the equator at {across:?}");
}

#[test]
fn an_extent_indicator_reaches_its_locators_projection_boundary() {
    let source = Map::new("s");
    let mut locator = Map::new("m");
    locator.set_projection(rustgis_map::Projection::Custom("EPSG:32631".into()));
    let center = Coord::new(3.0, 0.0);
    let camera = rustgis_map::layout::frame_camera(&source, center, 1.75e8, 0.0, 0.0);
    let top = source.to_lonlat(camera.to_map(40.0 * PT_PER_MM, 0.0, 80.0 * PT_PER_MM, 20.0 * PT_PER_MM)).unwrap().y;
    let red = outline_on(source, (center, 1.75e8, 80.0, 20.0), locator.clone(), (center, 1.5e8));
    // UTM's forward transform accepts 30 degrees on either side of its central meridian.
    // All four ends reach that boundary, whether entering or leaving the visible area.
    for lon in [-27.0 + 1e-7, 33.0 - 1e-7] {
        for lat in [-top, top] {
            let edge = locator.to_map(Coord::new(lon, lat)).unwrap();
            let middle = locator.to_map(center).unwrap();
            let expected = ((edge.x - middle.x) / 150_000.0, (middle.y - edge.y) / 150_000.0);
            let distance = red.iter().map(|p| (p.0 - expected.0).hypot(p.1 - expected.1)).fold(f64::INFINITY, f64::min);
            assert!(distance < 0.6, "the outline stops {distance:.2} mm short of ({lon}, {lat})");
        }
    }
}

/// A boundary search on either side of the date line follows the short arc, in either
/// direction, rather than drawing a chord across the UTM overview (#2433).
#[test]
fn an_overviews_boundary_search_crosses_the_date_line_on_the_short_arc() {
    for (code, center, inside, outside, edge) in
        [("EPSG:32655", 147.0, 176.0, -179.0, 177.0 - 1e-7), ("EPSG:32606", -147.0, -176.0, 179.0, -177.0 + 1e-7)]
    {
        let mut map = Map::new("overview");
        map.set_projection(rustgis_map::Projection::Custom(code.into()));
        let camera = rustgis_map::layout::frame_camera(&map, Coord::new(center, 0.0), 1e8, 0.0, 0.0);
        let view = View { width: 600.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
        let screen = |lon| {
            let p = map.to_map(Coord::new(lon, 10.0)).unwrap();
            camera.try_to_screen(p, view.width, view.height).unwrap()
        };
        let start = screen(inside);
        let end = screen(edge);
        for reverse in [false, true] {
            let mut ring = [Some(Coord::new(inside, 10.0)), Some(Coord::new(outside, 10.0))];
            if reverse {
                ring.reverse();
            }
            let mut canvas = Canvas::new(600, 400).unwrap();
            outline_lonlat(&mut canvas, &map, &view, &ring, false, Rgba::rgb(255, 0, 0), 2.0);
            let pm = canvas.into_pixmap();
            let red: Vec<_> = (0..400)
                .flat_map(|y| (0..600).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let (r, g, b) = rgb(&pm, x, y);
                    r > 180 && g < 90 && b < 90
                })
                .map(|(x, y)| (f64::from(x), f64::from(y)))
                .collect();
            assert!(!red.is_empty(), "{code}, reverse {reverse}: no outline");
            assert!(
                red.iter().all(|p| p.0 >= start.0.min(end.0) - 2.0 && p.0 <= start.0.max(end.0) + 2.0),
                "{code}, reverse {reverse}: a chord across the overview"
            );
            assert!(
                red.iter().any(|p| (p.0 - end.0).hypot(p.1 - end.1) < 2.0),
                "{code}, reverse {reverse}: the outline does not reach the boundary"
            );
        }
    }
}

/// A flat frame on a custom projection whose edges run off its world (Mollweide, zoomed out
/// past its ellipse) is outlined by its top and bottom edges, broken where they leave the
/// world and reaching it there, not joined by lines across the map (#2369).
#[test]
fn a_flat_frame_off_its_projections_world_is_outlined_without_a_chord() {
    let mut moll = Map::new("s");
    moll.set_projection(rustgis_map::Projection::from_code("mollweide").unwrap());
    // 80 × 20 mm at 1:600M: Mollweide's world is 60 × 30 mm, so the frame's sides are off it
    // and its top and bottom edges leave it about 22 mm either side of the centre.
    let red = outline_on_flat_world(moll, (Coord::new(0.0, 0.0), 6e8, 80.0, 20.0));
    assert!(!red.is_empty(), "no outline of the frame");
    // The equator's row is crossed only by a line joining the top edge to the bottom one.
    let across: Vec<_> = red.iter().filter(|p| p.1.abs() < 1.0).collect();
    assert!(across.is_empty(), "the outline crosses the equator at {across:?}");
    // The edges run to the world's sides, ±180° (33.4 mm from the centre at 1:600M), and no
    // further: without bisecting, they stopped at the last sample on the world, at about 160°.
    let reach = red.iter().fold(0.0_f64, |m, p| m.max(p.0.abs()));
    assert!((reach - 33.4).abs() < 0.6, "the outline reaches {reach:.1} mm from the centre, not ±180°");
    for side in [-1.0, 1.0] {
        assert!(red.iter().any(|p| (p.0 * side - 33.4).abs() < 0.6), "no outline at the world's {side} side");
    }
}

/// A frame on the globe around the North Pole, outlined on a flat world map, is split at ±180°
/// and drawn across the whole world, not as one piece a stretch of which is off the world, past
/// ±180° (#2369).
#[test]
fn a_globe_frame_around_a_pole_is_outlined_across_a_flat_world() {
    let mut globe = Map::new("s");
    globe.set_projection(rustgis_map::Projection::Globe);
    // 40 mm at 1:40M, about 1600 km across, centred 5° (550 km) from the pole: the pole inside
    // it. (A frame's centre is kept in Web Mercator, up to 85.05°.)
    let (at, scale) = (Coord::new(30.0, 85.0), 4e7);
    let cam = rustgis_map::layout::frame_camera(&globe, at, scale, 0.0, 0.0);
    let side = 40.0 * PT_PER_MM;
    let (px, py) = rustgis_map::globe::Globe::new(&cam, side, side).forward(Coord::new(0.0, 90.0)).expect("the pole on the disc");
    assert!((0.0..side).contains(&px) && (0.0..side).contains(&py), "the pole at ({px:.0}, {py:.0}), off the frame");
    let red = outline_on_flat_world(globe, (at, scale, 40.0, 40.0));
    assert!(!red.is_empty(), "no outline of the frame");
    // On both of the world's sides, ±180° (33.4 mm from the centre), and not past them.
    for dir in [-1.0, 1.0] {
        assert!(red.iter().any(|p| (p.0 * dir - 33.4).abs() < 1.0), "no outline at the world's {dir} side");
    }
    let past: Vec<_> = red.iter().filter(|p| p.0.abs() > 34.2).collect();
    assert!(past.is_empty(), "{} red pixels past ±180°, as far as {:?}", past.len(), past.iter().map(|p| p.0).fold(0.0_f64, f64::max));
}

/// A ring placed on Robinson centred on 150°E, whose edge is at -30°, is cut there, each piece
/// on its side of the map, not joined across it (#2369): a flat frame's ring across the edge,
/// and a globe frame's around a pole, a whole turn of longitude.
#[test]
fn a_ring_is_cut_at_its_maps_own_edge() {
    let mut robinson = Map::new("m");
    robinson.set_projection(rustgis_map::Projection::Custom("robinson +lon_0=150".into()));
    let mid = Some(Coord::new(150.0, 0.0));
    let flat = [(-40.0, 10.0), (-20.0, 10.0), (-20.0, 20.0), (-40.0, 20.0), (-40.0, 10.0)].map(|(x, y)| Some(Coord::new(x, y)));
    // From 20°E all the way round, 85°N, as a pole's ring runs, its longitudes continuous.
    let pole: Vec<Option<Coord>> = (0..=36).map(|i| Some(Coord::new(20.0 + 10.0 * f64::from(i), 85.0))).collect();
    for (name, ring, from_globe) in [("flat", &flat[..], false), ("pole", &pole[..], true)] {
        let placed = ring_on_flat(&robinson, mid, ring, from_globe);
        let points: Vec<Coord> = placed.iter().flatten().copied().collect();
        assert!(!points.is_empty(), "{name}: nothing placed");
        // Every point in the map's world, -30° to 330°, so it projects onto its own side.
        let out: Vec<_> = points.iter().filter(|c| !(-30.0..=330.0).contains(&c.x)).collect();
        assert!(out.is_empty(), "{name}: points outside the map's world: {out:?}");
        // On both sides of the edge, the pieces broken apart.
        assert!(points.iter().any(|c| c.x < -29.0) && points.iter().any(|c| c.x > 329.0), "{name}: not on both sides: {points:?}");
        let jumps: Vec<_> = placed.windows(2).filter_map(|w| Some((w[0]?, w[1]?))).filter(|(a, b)| (a.x - b.x).abs() > 180.0).collect();
        assert!(jumps.is_empty(), "{name}: joined across the edge: {jumps:?}");
    }
    // On Web Mercator, which doesn't wrap, a flat frame's ring is placed where it is.
    assert_eq!(ring_on_flat(&Map::new("m"), Some(Coord::new(0.0, 0.0)), &flat, false), flat.to_vec());
}

/// A flat frame wider than the world, outlined on another flat frame, is drawn where it is
/// (#2348): its first point past -180° doesn't move it a turn east, so both its sides show.
#[test]
fn a_flat_frame_wider_than_the_world_is_outlined_where_it_is() {
    use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH};
    let frame = |center, scale, extent_of| ElementKind::MapFrame {
        map: "m".into(),
        center,
        scale,
        shape: FrameShape::Rectangle,
        grid: None,
        extent_of,
        extent_color: INDICATOR_COLOR,
        extent_width: INDICATOR_WIDTH,
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing: 0.0,
        pitch: 0.0,
    };
    let at = Coord::new(0.0, 0.0);
    let mut l = Layout::new("Layout", 200.0, 120.0);
    // 80 mm at 1:529M on the equator: about 380° of longitude, -190° to 190°.
    let source = l.add("Main", (5.0, 5.0, 80.0, 20.0), frame(at, 5.29e8, None));
    // 90 mm at 1:593M: about 480°, so both of the source's sides are in it.
    l.add("Locator", (100.0, 5.0, 90.0, 60.0), frame(at, 5.93e8, Some(source)));
    let pm = render(&l, &mut [Map::new("m")], 96.0, &PageInfo::default()).unwrap();
    let k = PT_PER_MM;
    let red = |x: u32, y: u32| {
        let (r, g, b) = rgb(&pm, x, y);
        r > 180 && g < 90 && b < 90
    };
    let (top, bottom) = ((5.0 * k) as u32, (65.0 * k) as u32);
    let middle = 145.0 * k;
    let side = |xs: std::ops::Range<u32>| xs.flat_map(|x| (top..bottom).map(move |y| (x, y))).filter(|&(x, y)| red(x, y)).count();
    let (west, east) = (side((100.0 * k) as u32..(middle - 10.0 * k) as u32), side((middle + 10.0 * k) as u32..(190.0 * k) as u32));
    assert!(west > 0 && east > 0, "the outline's sides: {west} red pixels west of the middle, {east} east");
}

fn rgb(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
    pm.pixel(x, y).map(|p| (p.red(), p.green(), p.blue())).unwrap_or((255, 255, 255))
}

/// A colorbar's title longer than the element is wide wraps inside it (#2493), every word kept;
/// the bar goes under its lines.
#[test]
fn a_colorbars_long_title_wraps_inside_it() {
    let scale = ColorScale::Ramp { lo: 0.0, hi: 10.0, colors: vec![Rgba::rgb(200, 200, 200), Rgba::rgb(230, 230, 230)] };
    let title = "Precipitação média mensal JAXA GSMaP (mm/mês), jan–mar 2025";
    let bar = Bar { title, orientation: Orientation::Horizontal, ticks: 3, side: TickSide::End, reverse: false, comma: false, ink: INK };
    // A canvas wider than the element, so ink past its right edge shows.
    let mut c = Canvas::new(600, 200).unwrap();
    let r = Rect::from_xywh(10.0, 10.0, 200.0, 150.0).unwrap();
    colorbar(&mut c, r, &scale, &bar, 1.0);
    let dark = |x: u32, y: u32| c.pixmap().pixel(x, y).is_some_and(|p| p.alpha() > 128 && p.red() < 120);
    let past = (r.right().ceil() as u32 + 1..600).find(|x| (0..200).any(|y| dark(*x, y)));
    assert_eq!(past, None, "the title's ink past the element's right edge");
    let lines = colorbar_title_lines(title, 200.0 / PT_PER_MM);
    assert!(lines.len() >= 2, "{lines:?}");
    assert_eq!(lines.concat().replace(' ', ""), title.replace(' ', ""), "every word is kept");
    // The second line is inked, under the first.
    let pitch = (COLORBAR_TITLE_PT * 1.33 * 1.25) as u32;
    let line = |y0: u32| (y0..y0 + 8).any(|y| (10..210).any(|x| dark(x, y)));
    assert!(line(10 + pitch + 3), "a second line of title");
}

/// A colorbar's title stops short of the bar's own room, so a long title never leaves the bar
/// without its tick labels: the bar ends above them, its labels under it (#2638). Before, the
/// title's second line took the room, and the bar filled the element's bottom with no labels.
#[test]
fn a_colorbars_long_title_leaves_its_bar_room_for_labels() {
    // A red-to-blue bar, so its pixels are told apart from the grey text.
    let scale = ColorScale::Ramp { lo: 0.0, hi: 10.0, colors: vec![Rgba::rgb(220, 30, 30), Rgba::rgb(30, 30, 220)] };
    let title = "Precipitação média mensal JAXA GSMaP (mm/mês), jan–mar 2025";
    let bar = Bar { title, orientation: Orientation::Horizontal, ticks: 3, side: TickSide::End, reverse: false, comma: false, ink: INK };
    // 100 × 40 px: room for two lines of title, but then only 8 px of bar, with no labels.
    let mut c = Canvas::new(300, 200).unwrap();
    colorbar(&mut c, Rect::from_xywh(10.0, 10.0, 100.0, 40.0).unwrap(), &scale, &bar, 1.0);
    let px = |x: u32, y: u32| c.pixmap().pixel(x, y);
    let coloured =
        |y: u32| (10..110).any(|x| px(x, y).is_some_and(|p| p.alpha() > 200 && p.green() < 90 && p.red().abs_diff(p.blue()) > 60));
    let dark = |y: u32| (10..110).any(|x| px(x, y).is_some_and(|p| p.alpha() > 128 && p.red() < 120 && p.blue() < 120));
    assert!((26..36).any(coloured), "the bar under the title");
    assert!(!(44..50).any(coloured), "the bar ends above its labels, not at the element's bottom");
    assert!((40..50).any(dark), "its labels under it");
}

/// A vertical colorbar's end labels are centred on its bar's ends, so its title leaves the bar
/// two labels' height (#2638): a vertical colorbar 25 px high (6.6 mm at 96 dpi) holds its
/// one-line title back and says so, its bar keeping both end labels, and one 12 mm high draws
/// the title and says nothing. A vertical colorbar too short for its end labels, or too narrow
/// for its labels beside the bar, says so even with no title, and a horizontal one too short
/// for its labels under its bar, too.
#[test]
fn a_colorbars_labels_left_out_are_said() {
    let map = ramp_map(
        rustgis_map::raster::RasterRenderer::Stretch { band: 0, ramp: rustgis_map::Ramp::Thermal, invert: false },
        rustgis_map::raster::Stretch::MinMax,
    );
    let info = PageInfo::default();
    let mm = 25.4 / 96.0;
    // (orientation, title, width and height in mm, a loss said, title lines left out)
    for (orientation, title, w, h, said, left_out) in [
        (Orientation::Vertical, "Elevation (m)", 25.0, 25.0 * mm, true, 1),
        (Orientation::Vertical, "Elevation (m)", 25.0, 12.0, false, 0),
        (Orientation::Vertical, "", 25.0, 12.0 * mm, true, 0),
        (Orientation::Vertical, "", 25.0, 25.0 * mm, false, 0),
        // Too narrow for its labels beside the bar: they're left out, and said.
        (Orientation::Vertical, "", 2.0, 25.0, true, 0),
        (Orientation::Horizontal, "", 25.0, 15.0 * mm, true, 0),
        (Orientation::Horizontal, "", 25.0, 25.0 * mm, false, 0),
    ] {
        let mut l = Layout::new("Page", 120.0, 90.0);
        let frame = l.add(
            "Frame",
            (60.0, 60.0, 50.0, 25.0),
            serde_json::from_value(serde_json::json!({"MapFrame":{"map":"m", "center":{"x":0,"y":0}, "scale":1000000}})).unwrap(),
        );
        let kind = ElementKind::Colorbar {
            frame,
            layer: None,
            title: title.into(),
            orientation,
            ticks: 5,
            tick_side: TickSide::End,
            reverse: false,
            color: None,
        };
        l.add("Colorbar", (5.0, 5.0, w, h), kind);
        let losses = text_losses(&l, std::slice::from_ref(&map), 96.0, &info);
        let case = format!("{orientation:?} {title:?} {w:.1} × {h:.1} mm: {losses:?}");
        assert_eq!(losses.len() == 1, said, "{case}");
        if let Some(loss) = losses.first() {
            assert!(loss.below_box && loss.left_out == left_out, "{case}");
            // Its own advice, not a text's line breaks.
            let note = clipped_text_notice(&l, &[(loss.id, loss.name.clone())]).unwrap_or_default();
            assert!(note.contains("make the colorbar larger"), "{case}: {note}");
        }
    }
    // As drawn: 25 px high, the title held back, both end labels inside the element.
    let scale = ColorScale::Ramp { lo: 0.0, hi: 10.0, colors: vec![Rgba::rgb(220, 30, 30), Rgba::rgb(30, 30, 220)] };
    let bar = Bar {
        title: "Elevation (m)",
        orientation: Orientation::Vertical,
        ticks: 2,
        side: TickSide::End,
        reverse: false,
        comma: false,
        ink: INK,
    };
    let mut c = Canvas::new(200, 100).unwrap();
    colorbar(&mut c, Rect::from_xywh(10.0, 10.0, 100.0, 25.0).unwrap(), &scale, &bar, 1.0);
    let dark = |x: u32, y: u32| c.pixmap().pixel(x, y).is_some_and(|p| p.alpha() > 128 && p.red() < 120 && p.blue() < 120);
    // The labels stand to the bar's right, past its ticks: ink near its top end and its bottom.
    let label_ink = |ys: std::ops::Range<u32>| ys.into_iter().any(|y| (60..110).any(|x| dark(x, y)));
    assert!(label_ink(10..20), "the top end's label");
    assert!(label_ink(26..36), "the bottom end's label");
}

/// A label left out for spilling out of a colorbar takes no room, so it doesn't thin the next
/// one away unsaid (#2638): on a bar 40 px wide, the end label 1000 spills past its left
/// side and is counted, and the middle label 1500, inside, is still drawn.
#[test]
fn a_colorbars_spilled_label_doesnt_thin_the_next() {
    let scale = ColorScale::Ramp { lo: 1000.0, hi: 2000.0, colors: vec![Rgba::rgb(220, 30, 30), Rgba::rgb(30, 30, 220)] };
    let bar =
        Bar { title: "", orientation: Orientation::Horizontal, ticks: 3, side: TickSide::End, reverse: false, comma: false, ink: INK };
    let plan = colorbar_plan(Rect::from_xywh(0.0, 0.0, 40.0, 40.0).unwrap(), &scale, &bar, 1.0);
    let drawn: Vec<&str> = plan.labels.iter().map(|l| l.0.as_str()).collect();
    assert!(plan.spilled >= 1, "the end label spills: {drawn:?}");
    assert!(drawn.contains(&"1500"), "the middle label is drawn: {drawn:?}");
}

/// A colorbar too short for a line of title over its bar holds the title back, and the bar
/// then starts at the element's top, keeping its labels: a title not drawn takes no room
/// (#2638). Before, the bar was pushed down by the room of a line never drawn, and lost them.
#[test]
fn a_colorbar_whose_title_is_held_back_keeps_its_labels() {
    let scale = ColorScale::Ramp { lo: 0.0, hi: 10.0, colors: vec![Rgba::rgb(220, 30, 30), Rgba::rgb(30, 30, 220)] };
    let title = "Precipitação média mensal JAXA GSMaP (mm/mês), jan–mar 2025";
    let bar = Bar { title, orientation: Orientation::Horizontal, ticks: 3, side: TickSide::End, reverse: false, comma: false, ink: INK };
    // 100 × 30 px, from y = 10 to 40: a line of title (16.8 px) and the bar's own room
    // (19.8) don't both fit, so no line is drawn.
    assert_eq!(colorbar_title_drawn(4, 10.0, 40.0 - colorbar_bar_room(1.0, Orientation::Horizontal), COLORBAR_TITLE_PT as f32 * 1.33), 0);
    let mut c = Canvas::new(300, 200).unwrap();
    colorbar(&mut c, Rect::from_xywh(10.0, 10.0, 100.0, 30.0).unwrap(), &scale, &bar, 1.0);
    let px = |x: u32, y: u32| c.pixmap().pixel(x, y);
    let coloured =
        |y: u32| (10..110).any(|x| px(x, y).is_some_and(|p| p.alpha() > 200 && p.green() < 90 && p.red().abs_diff(p.blue()) > 60));
    let dark = |y: u32| (10..110).any(|x| px(x, y).is_some_and(|p| p.alpha() > 128 && p.red() < 120 && p.blue() < 120));
    assert!((11..15).any(coloured), "the bar at the element's top");
    assert!(!(35..40).any(coloured), "the bar ends above its labels");
    assert!((30..40).any(dark), "its labels under it");
}

/// A colorbar's title lines are drawn only where the bar keeps its own room under them, the
/// first too, with no slack: the bar never loses its labels to them (#2638). At 12 px a line,
/// the first needs 16.8 px and each further one 15 more, over the bar's 19.8.
#[test]
fn a_colorbars_title_lines_leave_the_bar_its_room() {
    let (px, room) = (COLORBAR_TITLE_PT as f32 * 1.33, colorbar_bar_room(1.0, Orientation::Horizontal));
    for (height, lines) in [(20.0, 0), (36.0, 0), (37.0, 1), (51.2, 1), (52.0, 2), (200.0, 5)] {
        assert_eq!(colorbar_title_drawn(5, 0.0, height - room, px), lines, "{height} px");
    }
    // Whatever the height, the lines drawn leave the bar its room.
    for h in 0..300 {
        let h = h as f32 * 0.5;
        let n = colorbar_title_drawn(5, 0.0, h - room, px);
        assert!(n == 0 || px * (1.4 + 1.25 * (n - 1) as f32) + room <= h, "{h} px: {n} lines");
    }
}

/// A colorbar too short for its title's lines draws those that fit inside it, not lines past
/// its bottom, and the page's cut-off check says the rest are left out; one tall enough says
/// nothing (#2493).
#[test]
fn a_colorbars_title_lines_past_its_bottom_are_left_out_and_said() {
    let scale = ColorScale::Ramp { lo: 0.0, hi: 10.0, colors: vec![Rgba::rgb(200, 200, 200), Rgba::rgb(230, 230, 230)] };
    let title = "Precipitação média mensal JAXA GSMaP (mm/mês), jan–mar 2025";
    let bar = Bar { title, orientation: Orientation::Horizontal, ticks: 3, side: TickSide::End, reverse: false, comma: false, ink: INK };
    // 100 px wide, four lines or more; 20 px high, no line: the first's room would end at
    // 10 + 12 × 1.4 = 26.8, leaving the bar less than its own 19.8 (a second would be centred
    // at 32, within a line of the bottom at 30, as a text element's line may be).
    assert!(colorbar_title_lines(title, 100.0 / PT_PER_MM).len() >= 4);
    let mut c = Canvas::new(300, 200).unwrap();
    colorbar(&mut c, Rect::from_xywh(10.0, 10.0, 100.0, 20.0).unwrap(), &scale, &bar, 1.0);
    let dark = |x: u32, y: u32| c.pixmap().pixel(x, y).is_some_and(|p| p.alpha() > 128 && p.red() < 120);
    let lowest = (0..200).rev().find(|y| (0..300).any(|x| dark(x, *y)));
    assert!(lowest.is_some_and(|y| y <= 30), "ink down to {lowest:?}, past the colorbar's bottom at 30");
    // On a page, in a frame whose map has a raster on a ramp.
    let map = ramp_map(
        rustgis_map::raster::RasterRenderer::Stretch { band: 0, ramp: rustgis_map::Ramp::Thermal, invert: false },
        rustgis_map::raster::Stretch::MinMax,
    );
    let mut l = Layout::new("Page", 120.0, 90.0);
    let frame = l.add(
        "Frame",
        (60.0, 60.0, 50.0, 25.0),
        serde_json::from_value(serde_json::json!({"MapFrame":{"map":"m", "center":{"x":0,"y":0}, "scale":1000000}})).unwrap(),
    );
    let kind = ElementKind::Colorbar {
        frame,
        layer: None,
        title: title.into(),
        orientation: Orientation::Horizontal,
        ticks: 5,
        tick_side: TickSide::End,
        reverse: false,
        color: None,
    };
    let id = l.add("Colorbar", (5.0, 5.0, 25.0, 6.0), kind);
    let info = PageInfo::default();
    let losses = text_losses(&l, std::slice::from_ref(&map), 96.0, &info);
    assert_eq!(losses.len(), 1, "the short colorbar's title is cut");
    // 6 mm high: no room for a line of title over the bar's own: every line said.
    let lines = colorbar_title_lines(title, 25.0).len();
    assert!(lines >= 3 && losses[0].id == id && losses[0].below_box, "{lines} lines");
    assert_eq!(losses[0].left_out, lines);
    l.element_mut(id).unwrap().h = 40.0;
    assert!(text_losses(&l, std::slice::from_ref(&map), 96.0, &info).is_empty(), "tall enough for every line");
}

#[test]
fn colorbars_draw_the_raster_colour_scale() {
    use rustgis_map::raster::{RasterRenderer, Stretch};
    let ramp = rustgis_map::Ramp::Thermal;
    let map = ramp_map(RasterRenderer::Stretch { band: 0, ramp: ramp.clone(), invert: false }, Stretch::MinMax);
    let k = PT_PER_MM;
    // A row through the bar, near its top edge (the labels are below it).
    let y = (12.0 * k) as u32;
    let pm = colorbar_page(&map, Orientation::Horizontal, false, "");
    let (left, right) = (rgb(&pm, (16.0 * k) as u32, y), rgb(&pm, (84.0 * k) as u32, y));
    let close = |a: (u8, u8, u8), b: Rgba| a.0.abs_diff(b.r()) < 40 && a.1.abs_diff(b.g()) < 40 && a.2.abs_diff(b.b()) < 40;
    assert!(close(left, ramp.at(0.0)), "the low end is the ramp's start: {left:?} vs {:?}", ramp.at(0.0));
    assert!(close(right, ramp.at(1.0)), "the high end is the ramp's end: {right:?} vs {:?}", ramp.at(1.0));
    // Labelled below the bar: dark text under it, between the bar and the element's bottom.
    let inked = |pm: &Pixmap, y0: f64, y1: f64| {
        ((y0 * k) as u32..(y1 * k) as u32).any(|y| (0..pm.width()).any(|x| rgb(pm, x, y).0 < 80 && rgb(pm, x, y).2 < 80))
    };
    assert!(inked(&pm, 26.0, 30.0), "tick labels below the bar");
    // Reversed, the ends swap.
    let rev = colorbar_page(&map, Orientation::Horizontal, true, "");
    assert!(close(rgb(&rev, (16.0 * k) as u32, y), ramp.at(1.0)));
    assert!(close(rgb(&rev, (84.0 * k) as u32, y), ramp.at(0.0)));
    // Vertical, low values sit at the bottom.
    let v = colorbar_page(&map, Orientation::Vertical, false, "");
    let x = (11.0 * k) as u32;
    assert!(close(rgb(&v, x, (85.0 * k) as u32), ramp.at(0.0)), "{:?}", rgb(&v, x, (85.0 * k) as u32));
    assert!(close(rgb(&v, x, (15.0 * k) as u32), ramp.at(1.0)), "{:?}", rgb(&v, x, (15.0 * k) as u32));
    // A title pushes the bar down under it.
    let t = colorbar_page(&map, Orientation::Horizontal, false, "Elevation (m)");
    assert!(inked(&t, 10.0, 13.0), "the title is drawn at the top");

    // Classes draw as steps, one colour each.
    let breaks = vec![24.0, 49.0, 74.0, 99.0];
    let classify =
        RasterRenderer::Classify { band: 0, method: rustgis_map::ClassMethod::EqualInterval, ramp: ramp.clone(), breaks, classes: 4 };
    let classes = ramp_map(classify, Stretch::MinMax);
    // Named: a colorbar with no layer leaves a raster drawn by classes to the legend (#1679).
    assert!(rustgis_map::layout::colorbar_layer(&classes, None).is_none());
    assert!(rustgis_map::layout::colorbar_layer(&map, None).is_some(), "a stretch is the default's");
    // A stretched raster in a hidden group isn't the default's: the one the map shows is.
    let mut hidden = map.clone();
    let top = hidden.layers[0].clone();
    let id = hidden.add_raster("hidden", top.raster.clone().unwrap());
    let mut group = rustgis_map::LayerGroup::new("off");
    group.visible = false;
    group.children.push(id);
    hidden.add_group(group);
    assert_eq!(rustgis_map::layout::colorbar_layer(&hidden, None).map(|l| l.name.as_str()), Some("dem"));
    let pm = colorbar_page(&classes, Orientation::Horizontal, false, "");
    let mut colours: Vec<(u8, u8, u8)> = ((16.0 * k) as u32..(84.0 * k) as u32).map(|x| rgb(&pm, x, y)).collect();
    colours.dedup();
    assert!((4..=7).contains(&colours.len()), "four steps (and their seams): {colours:?}");

    // In an element too short for its labels, they are left out rather than drawn under it.
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let frame = l.add(
        "Map",
        (95.0, 95.0, 4.0, 4.0),
        ElementKind::MapFrame {
            map: "m".into(),
            center: Coord::new(0.0, 0.0),
            scale: 1e8,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    let kind = ElementKind::Colorbar {
        frame,
        layer: None,
        title: String::new(),
        orientation: Orientation::Horizontal,
        ticks: 5,
        tick_side: TickSide::End,
        reverse: false,
        color: None,
    };
    l.add("Colorbar", (10.0, 10.0, 80.0, 3.0), kind);
    let short = render(&l, &mut [map.clone()], 96.0, &PageInfo::default()).unwrap();
    assert!(!inked(&short, 13.5, 20.0), "nothing spills below the element");
    assert!(close(rgb(&short, (16.0 * k) as u32, (11.0 * k) as u32), ramp.at(0.0)), "the bar is still drawn");

    // RGB has no colour scale: a placeholder, not a bar.
    let pm =
        colorbar_page(&ramp_map(RasterRenderer::Rgb { red: 0, green: 0, blue: 0 }, Stretch::MinMax), Orientation::Horizontal, false, "");
    assert_eq!(rgb(&pm, (16.0 * k) as u32, y), (0xF0, 0xF1, 0xF3));
}

#[test]
fn colorbar_tick_labels_are_even_and_tidy() {
    let labels = |lo, hi, n| tick_labels(lo, hi, n).into_iter().map(|(_, s)| s).collect::<Vec<_>>();
    assert_eq!(labels(0.0, 100.0, 5), ["0", "25", "50", "75", "100"]);
    assert_eq!(labels(0.0, 1.0, 5), ["0.00", "0.25", "0.50", "0.75", "1.00"]);
    assert_eq!(labels(0.0, 1.0, 3), ["0.0", "0.5", "1.0"]);
    assert_eq!(labels(-1.0, 1.0, 2), ["-1", "1"]);
    // Odd bounds keep only the decimals the step needs.
    assert_eq!(labels(12.345, 87.9, 2), ["12", "88"]);
    assert_eq!(labels(-0.004, 1.0, 2), ["0", "1"], "no -0");
    // One value: one label. Out of range counts are clamped.
    assert_eq!(labels(5.0, 5.0, 5), ["5"]);
    assert_eq!(tick_labels(0.0, 1.0, 0).len(), 2);
    assert_eq!(tick_labels(0.0, 1.0, 1000).len(), rustgis_map::layout::MAX_COLORBAR_TICKS);
    assert!(tick_labels(f64::NAN, 1.0, 5).is_empty());
    assert_eq!(labels(0.0, 4e6, 2), ["0", "4.0M"]);
}

#[test]
fn hairline_rectangles_draw_thin() {
    // A filled 0.15 mm rule at 300 dpi is under two pixels tall, not a 1 mm bar (#211).
    let mut l = Layout::new("Layout", 50.0, 20.0);
    l.add(
        "Rule",
        (5.0, 10.0, 40.0, 0.15),
        ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0, dash: None },
    );
    let pm = render(&l, &mut [], 300.0, &PageInfo::default()).unwrap();
    let x = pm.width() / 2;
    let inked = (0..pm.height()).filter(|&y| pm.pixel(x, y).is_some_and(|p| p.red() < 250)).count();
    assert!((1..=3).contains(&inked), "{inked} rows inked");
}

#[test]
fn arrowheads_are_drawn_at_the_ends_they_point_to() {
    use rustgis_map::layout::LineDir;
    // 100 × 20 mm at 254 dpi (ten pixels a millimetre); a level arrow from 10 to 90 mm.
    let mut l = Layout::new("Layout", 100.0, 20.0);
    let blue = Rgba::rgb(0, 0, 255);
    l.add(
        "Arrow",
        (10.0, 7.0, 80.0, 6.0),
        ElementKind::Line { color: blue, width: 1.0, dir: LineDir::Horizontal, head: LineHead::End, dash: None },
    );
    let pm = render(&l, &mut [], 254.0, &PageInfo::default()).unwrap();
    let inked = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|p| p.red() < 128 && p.blue() > 128);
    // At 254 dpi (2.65 pixels a point) the shaft is about 2.6 px wide and the head about 22 px
    // long and 10 px either side of the line: 5 px off the line, 15 px back from the tip, is
    // inside the head, but clear of the shaft at the tail.
    assert!(inked(500, 100), "the shaft");
    assert!(inked(885, 105) && inked(885, 95), "the head, either side of the line");
    assert!(!inked(115, 105) && !inked(115, 95), "no head at the tail");
    assert!(!inked(905, 100), "nothing past the tip");
}

#[test]
fn ellipses_and_lines_draw_inside_their_boxes() {
    use rustgis_map::layout::LineDir;
    // 100 × 50 mm at 254 dpi: ten pixels a millimetre.
    let mut l = Layout::new("Layout", 100.0, 50.0);
    let red = Rgba::rgb(255, 0, 0);
    l.add("Circle", (10.0, 10.0, 30.0, 30.0), ElementKind::Ellipse { fill: red, outline: red, width: 0.0, dash: None });
    l.add(
        "Line",
        (50.0, 10.0, 40.0, 30.0),
        ElementKind::Line { color: Rgba::rgb(0, 0, 255), width: 2.0, dir: LineDir::Rising, head: LineHead::None, dash: None },
    );
    let pm = render(&l, &mut [], 254.0, &PageInfo::default()).unwrap();
    let is = |x: u32, y: u32, want: (u8, u8, u8)| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == want);
    let (white, red, blue) = ((255, 255, 255), (255, 0, 0), (0, 0, 255));
    // The circle's middle is filled; its box's corner, outside the circle, isn't.
    assert!(is(250, 250, red), "circle centre");
    assert!(is(110, 110, white), "outside the circle, in its box's corner");
    // The rising line runs from the box's bottom-left to its top-right, through the middle.
    assert!(is(700, 250, blue), "the line's middle");
    assert!(is(550, 150, white) && is(850, 350, white), "the other diagonal stays blank");
}

/// A 100 × 100 mm page with one map frame filling 10–90 mm, showing the world at 1:200M with
/// `grid` over it, drawn at 96 dpi.
fn grid_page(grid: rustgis_map::layout::Grid) -> Pixmap {
    let mut maps = vec![Map::new("m")];
    let mut l = Layout::new("Layout", 100.0, 100.0);
    l.add(
        "Map",
        (10.0, 10.0, 80.0, 80.0),
        ElementKind::MapFrame {
            map: "m".into(),
            center: Coord::new(0.0, 0.0),
            scale: 2e8,
            shape: FrameShape::Rectangle,
            grid: Some(grid),
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap()
}

/// Pixels in the box (page millimetres) that `hit` picks.
fn count(pm: &Pixmap, (x0, y0, x1, y1): (f64, f64, f64, f64), hit: impl Fn(tiny_skia::PremultipliedColorU8) -> bool) -> usize {
    let px = |mm: f64| (mm * PT_PER_MM) as u32;
    (px(y0)..px(y1)).flat_map(|y| (px(x0)..px(x1)).map(move |x| (x, y))).filter(|&(x, y)| pm.pixel(x, y).is_some_and(&hit)).count()
}

fn red(p: tiny_skia::PremultipliedColorU8) -> bool {
    p.red() > 180 && p.green() < 90 && p.blue() < 90
}

fn ink(p: tiny_skia::PremultipliedColorU8) -> bool {
    p.red() < 90 && p.green() < 90 && p.blue() < 90
}

/// A page whose 80 mm frame shows Winterthur at 1:25,000 (2 km across) on a map in `projection`.
fn winterthur_page(grid: rustgis_map::layout::Grid, projection: rustgis_map::Projection) -> Pixmap {
    let mut m = Map::new("m");
    m.projection = projection;
    let mut maps = vec![m];
    let mut l = Layout::new("Layout", 100.0, 100.0);
    l.add(
        "Map",
        (10.0, 10.0, 80.0, 80.0),
        ElementKind::MapFrame {
            map: "m".into(),
            center: Coord::new(8.73, 47.5),
            scale: 25_000.0,
            shape: FrameShape::Rectangle,
            grid: Some(grid),
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap()
}

/// Where red runs cross a row of the page (page millimetres): the middle of each run.
fn red_runs(pm: &Pixmap, y_mm: f64) -> Vec<f64> {
    let y = (y_mm * PT_PER_MM) as u32;
    let mut out = Vec::new();
    let mut start = None;
    for x in 0..pm.width() {
        match (pm.pixel(x, y).is_some_and(red), start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                out.push(f64::from(s + x - 1) / 2.0 / PT_PER_MM);
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A measured grid: eastings and northings its step apart in its coordinate system (1 km is
/// 40 mm at 1:25,000), bent onto the map's projection, straight on a map in that system; the
/// map's own system, else the frame's UTM zone, when it names none; nothing for a system it
/// can't draw.
#[test]
fn measured_grids_draw_eastings_and_northings() {
    use rustgis_map::layout::{Grid, GridKind};
    let utm = Grid {
        kind: GridKind::Measured,
        crs: Some("EPSG:25832".into()),
        step: 1000.0,
        color: Rgba::rgb(0xFF, 0, 0),
        labels: false,
        width: 1.5,
        ..Grid::default()
    };
    let gaps = |xs: &[f64]| xs.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>();
    // On Web Mercator: 1 km is 40 mm on paper, give or take Mercator's scale at 47.5°N.
    let pm = winterthur_page(utm.clone(), rustgis_map::Projection::WebMercator);
    let xs = red_runs(&pm, 50.0);
    assert!(xs.len() >= 2, "{xs:?}");
    assert!(gaps(&xs).iter().all(|g| (g - 40.0).abs() < 1.5), "{xs:?}");
    // On a map in EPSG:25832 itself: exactly 40 mm, and straight (the same at the top and bottom).
    let own = rustgis_map::Projection::Custom("EPSG:25832".into());
    let pm = winterthur_page(utm.clone(), own.clone());
    let (top, bottom) = (red_runs(&pm, 20.0), red_runs(&pm, 80.0));
    assert!(gaps(&top).iter().all(|g| (g - 40.0).abs() < 0.3), "{top:?}");
    assert_eq!(top.len(), bottom.len());
    assert!(top.iter().zip(&bottom).all(|(a, b)| (a - b).abs() < 0.3), "{top:?} {bottom:?}");
    // Naming none: the map's own system draws the same grid; on Web Mercator, UTM 32N's
    // (EPSG:32632, the same eastings as 25832 to a metre) does.
    let unnamed = Grid { crs: None, ..utm.clone() };
    assert!(red_runs(&winterthur_page(unnamed.clone(), own), 20.0).iter().zip(&top).all(|(a, b)| (a - b).abs() < 0.3));
    let mercator = red_runs(&winterthur_page(utm.clone(), rustgis_map::Projection::WebMercator), 20.0);
    let zone = red_runs(&winterthur_page(unnamed, rustgis_map::Projection::WebMercator), 20.0);
    assert_eq!(mercator.len(), zone.len());
    assert!(mercator.iter().zip(&zone).all(|(a, b)| (a - b).abs() < 0.3), "{mercator:?} {zone:?}");
    // A map in an ESRI system (or a preset): its own, by its PROJ definition, straight on it.
    let esri = rustgis_map::Projection::Custom("ESRI:102031".into());
    let pm = winterthur_page(Grid { crs: None, ..utm.clone() }, esri);
    let (top, bottom) = (red_runs(&pm, 20.0), red_runs(&pm, 80.0));
    assert!(top.len() >= 2 && top.len() == bottom.len(), "{top:?} {bottom:?}");
    assert!(top.iter().zip(&bottom).all(|(a, b)| (a - b).abs() < 0.3), "the map's own system: straight {top:?} {bottom:?}");
    // A system it can't draw (geographic, unknown): nothing.
    for crs in ["EPSG:4326", "EPSG:1", "not a system"] {
        let pm = winterthur_page(Grid { crs: Some(crs.into()), ..utm.clone() }, rustgis_map::Projection::WebMercator);
        assert_eq!(count(&pm, (10.0, 10.0, 90.0, 90.0), red), 0, "{crs}");
    }
    // A step too fine for the frame is widened rather than drawn as a blur.
    let fine = winterthur_page(Grid { step: 1.0, ..utm.clone() }, rustgis_map::Projection::WebMercator);
    // (About 2 km across: 50 m apart, some 40 lines across the frame.)
    let across = red_runs(&fine, 50.0).len();
    assert!((20..=55).contains(&across), "{across} lines across: not a red blur");
    // Its labels: eastings and northings, whole or short, a step apart.
    let tr = rustgis_data::raster::Crs::parse("EPSG:25832").unwrap().transformer().unwrap();
    let frame: Vec<Coord> = [(8.71, 47.49), (8.75, 47.49), (8.75, 47.51), (8.71, 47.51)].iter().map(|&(x, y)| Coord::new(x, y)).collect();
    let mut lines = Vec::new();
    measured_lines(&utm, &tr, &frame, false, &mut lines);
    let eastings: Vec<&str> = lines.iter().filter(|l| l.2).map(|l| l.1.as_str()).collect();
    assert!(eastings.contains(&"479000") && eastings.contains(&"480000"), "{eastings:?}");
    assert!(lines.iter().filter(|l| !l.2).any(|l| l.1 == "5261000"), "{lines:?}");
    let mut short = Vec::new();
    measured_lines(&Grid { short: true, ..utm.clone() }, &tr, &frame, false, &mut short);
    assert!(short.iter().any(|l| l.2 && l.1 == "79") && short.iter().any(|l| !l.2 && l.1 == "61"));
    // MGRS labels: an interval picked for the frame is at most 100 km, a square's side.
    let country: Vec<Coord> = [(3.0, 43.0), (15.0, 43.0), (15.0, 52.0), (3.0, 52.0)].iter().map(|&(x, y)| Coord::new(x, y)).collect();
    let tr32 = rustgis_data::raster::Crs::from_epsg(32632).unwrap().transformer().unwrap();
    let step_of = |g: &Grid| {
        let mut l = Vec::new();
        measured_lines(g, &tr32, &country, false, &mut l);
        let mut xs: Vec<f64> = l.iter().filter(|l| l.2).filter_map(|l| l.1.parse::<f64>().ok()).collect();
        xs.sort_by(f64::total_cmp);
        xs.windows(2).map(|w| w[1] - w[0]).fold(f64::INFINITY, f64::min)
    };
    let plain = Grid { step: 0.0, ..utm.clone() };
    assert_eq!(step_of(&plain), 200_000.0);
    assert_eq!(step_of(&Grid { mgrs: true, ..plain }), 100_000.0);
    // A frame of no extent draws none.
    let mut none = Vec::new();
    measured_lines(&utm, &tr, &[], false, &mut none);
    assert!(none.is_empty());
    assert_eq!((nice_step(2000.0, 1.0), nice_step(7000.0, 1.0), nice_step(1.0, 1.0)), (500.0, 1000.0, 1.0));
}

/// A measured grid labelled as MGRS names each 100 km square that shows, by its grid zone
/// designation and letters, where the label fits in it: one square over Winterthur at
/// 1:25,000, several at 1:2,000,000, each label in the square it names; in zone 32 only on a
/// frame across its edge with zone 33; both bands across the Equator; none on a frame too small
/// for a label. The grid is in UTM 32N for MGRS on a map in it, and in the frame's MGRS zone
/// (Norway's and Svalbard's exceptions kept) over LV95 or Web Mercator.
#[test]
fn mgrs_labels_name_the_100_km_squares() {
    use rustgis_map::layout::{Grid, GridKind};
    let map = Map::new("m");
    let tr = rustgis_data::raster::Crs::from_epsg(32632).unwrap().transformer().unwrap();
    // (A label's size fixed, not measured: the fonts installed differ between machines.)
    let size = |_: &str| (40.0_f32, 16.0_f32);
    let view = |center: Coord, scale: f64, mm: f64| View {
        width: mm * PT_PER_MM,
        height: mm * PT_PER_MM,
        pixels_per_point: 1.0,
        camera: frame_camera(&map, center, scale, 0.0, 0.0),
        sky: false,
    };
    let at = |v: &View, x: f32, y: f32| map.to_lonlat(v.camera.to_map(f64::from(x), f64::from(y), v.width, v.height)).unwrap();
    let near = view(Coord::new(8.73, 47.5), 25_000.0, 80.0);
    let one = mgrs_squares(&map, &near, None, 32, &tr, size);
    assert_eq!(one.len(), 1, "{one:?}");
    assert_eq!(one[0].0, "32T MT");
    assert!((f64::from(one[0].1) - near.width / 2.0).abs() < 2.0 && (f64::from(one[0].2) - near.height / 2.0).abs() < 2.0);
    let wide = view(Coord::new(8.73, 47.5), 2_000_000.0, 80.0);
    let many = mgrs_squares(&map, &wide, None, 32, &tr, size);
    assert!(many.len() >= 2, "{many:?}");
    for (text, x, y) in &many {
        let named = rustgis_geom::to_mgrs(at(&wide, *x, *y), 0).unwrap().to_string();
        assert_eq!(&named, text);
    }
    // Across the edge with zone 33 at 12° E: only zone 32's squares, west of it.
    let edge = view(Coord::new(11.0, 47.5), 2_000_000.0, 80.0);
    let west = mgrs_squares(&map, &edge, None, 32, &tr, size);
    assert!(!west.is_empty() && west.iter().all(|(t, x, y)| t.starts_with("32") && at(&edge, *x, *y).x < 12.0), "{west:?}");
    assert!(mgrs_squares(&map, &view(Coord::new(8.73, 47.5), 25_000.0, 15.0), None, 32, &tr, size).is_empty());
    // Across the Equator, a northern zone's grid names the southern squares too, as MGRS does.
    let equator = view(Coord::new(9.0, 0.0), 2_000_000.0, 80.0);
    let both = mgrs_squares(&map, &equator, None, 32, &tr, size);
    for band in ["32N ", "32M "] {
        assert!(both.iter().any(|(t, ..)| t.starts_with(band)), "{band}: {both:?}");
    }
    for (text, x, y) in &both {
        assert_eq!(&rustgis_geom::to_mgrs(at(&equator, *x, *y), 0).unwrap().to_string(), text);
    }
    // Its system: UTM 32N on a map in it or over LV95 (the frame's zone), the one named.
    let g = Grid { kind: GridKind::Measured, mgrs: true, ..Grid::default() };
    let c = Coord::new(8.73, 47.5);
    let on = |code: &str| {
        let mut m = Map::new("m");
        m.projection = rustgis_map::Projection::Custom(code.into());
        m
    };
    assert_eq!(measured_crs(&g, &on("EPSG:32632"), Coord::new(14.0, 47.5)).map(|c| c.epsg()), Some(32632));
    assert_eq!(measured_crs(&g, &on("EPSG:2056"), c).map(|c| c.epsg()), Some(32632));
    assert_eq!(measured_crs(&g, &map, c).map(|c| c.epsg()), Some(32632));
    // MGRS's zone at the centre: Bergen's 32V, Svalbard's 33X; a plain measured grid's 6° one.
    assert_eq!(measured_crs(&g, &map, Coord::new(5.3, 60.4)).map(|c| c.epsg()), Some(32632));
    assert_eq!(measured_crs(&g, &map, Coord::new(10.0, 78.0)).map(|c| c.epsg()), Some(32633));
    assert_eq!(measured_crs(&g, &map, Coord::new(9.0, -1.0)).map(|c| c.epsg()), Some(32732));
    let plain = Grid { mgrs: false, ..g.clone() };
    assert_eq!(measured_crs(&plain, &map, Coord::new(5.3, 60.4)).map(|c| c.epsg()), Some(32631));
    let named = Grid { crs: Some("EPSG:32733".into()), ..g.clone() };
    assert_eq!(measured_crs(&named, &map, c).map(|c| c.epsg()), Some(32733));
    assert_eq!(measured_crs(&Grid { crs: Some("EPSG:2056".into()), ..g }, &map, c), None);
}

#[test]
fn mgrs_labels_on_the_globe_name_the_squares_it_draws() {
    // #2337: on a globe, a label names the square the globe draws under it, not the one the
    // flat map would show at that place in the frame.
    let mut map = Map::new("m");
    map.set_projection(rustgis_map::Projection::Globe);
    let tr = rustgis_data::raster::Crs::from_epsg(32632).unwrap().transformer().unwrap();
    // Small labels at 1:20,000,000, where the globe's curve parts it from the flat map: the
    // two agree at the middle of the frame and drift apart away from it.
    let size = |_: &str| (6.0_f32, 4.0_f32);
    let view = View {
        width: 120.0 * PT_PER_MM,
        height: 120.0 * PT_PER_MM,
        pixels_per_point: 1.0,
        camera: frame_camera(&map, Coord::new(9.0, 47.5), 20_000_000.0, 0.0, 0.0),
        sky: false,
    };
    let globe = rustgis_map::globe::Globe::new(&view.camera, view.width, view.height);
    // The square the globe draws at a label's place, if any.
    let under = |x: f32, y: f32| globe.inverse(f64::from(x), f64::from(y)).and_then(|c| rustgis_geom::to_mgrs(c, 0)).map(|m| m.to_string());
    let squares = mgrs_squares(&map, &view, Some(&globe), 32, &tr, size);
    assert!(squares.len() >= 2, "{squares:?}");
    for (text, x, y) in &squares {
        assert_eq!(under(*x, *y).as_ref(), Some(text));
    }
    // Through the flat camera, as before, some labels sit on another square of the globe.
    let flat = mgrs_squares(&map, &view, None, 32, &tr, size);
    assert!(flat.iter().any(|(text, x, y)| under(*x, *y).as_ref() != Some(text)), "{flat:?}");
}

#[test]
fn the_space_around_a_globe_is_free_in_its_coverage() {
    // #2337: the light space a globe is drawn on leaves room for a layout's surrounds, as the
    // blank basemap's white paper does (its ocean too, as open sea on a flat map); the
    // atmosphere's glow at the globe's rim doesn't. Before, every cell was taken.
    let mut map = Map::new("m");
    map.set_projection(rustgis_map::Projection::Globe);
    // A globe about 64 mm across in a frame 100 mm square.
    let (gw, gh, cells) = frame_coverage(&mut map, Coord::new(0.0, 0.0), 2e8, 100.0, 100.0, 50).unwrap();
    let cell = |x: u32, y: u32| cells[(y * gw + x) as usize];
    assert!(cells.iter().any(|c| *c), "the glow at the rim is drawn");
    for (x, y) in [(1, 1), (gw - 2, 1), (1, gh - 2), (gw - 2, gh - 2)] {
        assert!(!cell(x, y), "the space at the corner ({x}, {y}) is free");
    }
}

#[test]
fn a_measured_grid_on_the_globe_seen_whole_is_drawn() {
    // #2337: with the whole globe inside the frame, no edge of it lands on the globe; the
    // measured grid takes its bounds from the globe in the frame instead of drawing nothing.
    use rustgis_map::layout::{Grid, GridKind};
    let mut m = Map::new("m");
    m.set_projection(rustgis_map::Projection::Globe);
    let mut maps = vec![m];
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let grid = Grid {
        kind: GridKind::Measured,
        crs: Some("EPSG:32632".into()),
        step: 0.0,
        color: Rgba::rgb(0xFF, 0, 0),
        labels: false,
        width: 1.0,
        ..Grid::default()
    };
    l.add(
        "Map",
        (10.0, 10.0, 80.0, 80.0),
        ElementKind::MapFrame {
            map: "m".into(),
            center: Coord::new(9.0, 45.0),
            scale: 2e8,
            shape: FrameShape::Rectangle,
            grid: Some(grid),
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    let pm = render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap();
    assert!(count(&pm, (30.0, 30.0, 70.0, 70.0), red) > 20, "the measured grid's lines on the globe");
    assert_eq!(count(&pm, (11.0, 11.0, 20.0, 20.0), red), 0, "none past the rim");
}

#[test]
fn a_grid_on_the_globe_is_drawn_on_it() {
    // #2337: a frame of a map on the globe draws its graticule through the globe, not as a
    // flat world around it: none of it beyond the globe's rim, in the frame's corners.
    use rustgis_map::layout::Grid;
    let mut m = Map::new("m");
    m.set_projection(rustgis_map::Projection::Globe);
    let mut maps = vec![m];
    let mut l = Layout::new("Layout", 100.0, 100.0);
    let grid = Grid { step: 30.0, color: Rgba::rgb(0xFF, 0, 0), labels: true, width: 1.0, ..Grid::default() };
    l.add(
        "Map",
        (10.0, 10.0, 80.0, 80.0),
        ElementKind::MapFrame {
            map: "m".into(),
            center: Coord::new(0.0, 0.0),
            // A globe about 64 mm across, in the middle of the 80 mm frame.
            scale: 2e8,
            shape: FrameShape::Rectangle,
            grid: Some(grid),
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    let pm = render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap();
    assert!(count(&pm, (40.0, 40.0, 60.0, 60.0), red) > 50, "lines on the globe");
    // Neither lines nor labels (dark) past the rim: in the corners and along the edges.
    let ink = |p: tiny_skia::PremultipliedColorU8| red(p) || (p.alpha() > 0 && p.red() < 120 && p.green() < 120 && p.blue() < 120);
    let corners = [(11.0, 11.0, 20.0, 20.0), (80.0, 11.0, 89.0, 20.0), (11.0, 80.0, 20.0, 89.0), (80.0, 80.0, 89.0, 89.0)];
    let edges = [(25.0, 11.0, 75.0, 15.0), (25.0, 85.0, 75.0, 89.0), (11.0, 25.0, 15.0, 75.0), (85.0, 25.0, 89.0, 75.0)];
    for r in corners.into_iter().chain(edges) {
        assert_eq!(count(&pm, r, ink), 0, "nothing past the rim at {r:?}");
    }
}

#[test]
fn grids_draw_lines_ticks_or_both() {
    use rustgis_map::layout::{Grid, GridStyle};
    let g =
        |style| Grid { step: 30.0, color: Rgba::rgb(0xFF, 0, 0), labels: false, style, tick_length: 4.0, width: 1.0, ..Grid::default() };
    let inner = (25.0, 25.0, 75.0, 75.0);
    let rim = (10.0, 10.0, 90.0, 13.0);
    let lines = grid_page(g(GridStyle::Lines));
    let ticks = grid_page(g(GridStyle::Ticks));
    let both = grid_page(g(GridStyle::LinesAndTicks));
    // Ticks only: nothing across the middle of the frame, but marks along its edge.
    assert!(count(&lines, inner, red) > 100, "lines cross the frame");
    assert_eq!(count(&ticks, inner, red), 0, "ticks stay at the edges");
    assert!(count(&ticks, rim, red) > 10, "ticks along the top edge");
    // Lines and ticks: the lines, with the ticks' extra width along the edge.
    assert!(count(&both, inner, red) > 100);
    assert!(count(&both, rim, red) > count(&lines, rim, red), "ticks thicken the lines at the edge");
    // The line width is the grid's.
    let thick = grid_page(Grid { width: 3.0, ..g(GridStyle::Lines) });
    assert!(count(&thick, inner, red) > 2 * count(&lines, inner, red));
}

#[test]
fn grid_labels_go_on_the_edges_picked() {
    use rustgis_map::layout::{Grid, GridEdges};
    let g = |edges, label_orientation| Grid { step: 30.0, color: Rgba::new(0, 0, 0, 0), edges, label_orientation, ..Grid::default() };
    let (top, bottom) = ((20.0, 10.0, 80.0, 16.0), (20.0, 84.0, 80.0, 90.0));
    let (left, right) = ((10.0, 20.0, 18.0, 80.0), (82.0, 20.0, 90.0, 80.0));
    let default = grid_page(g(GridEdges::default(), Orientation::Horizontal));
    assert!(count(&default, bottom, ink) > 0 && count(&default, left, ink) > 0, "bottom and left by default");
    assert_eq!(count(&default, top, ink) + count(&default, right, ink), 0);
    let flipped = grid_page(g(GridEdges { top: true, bottom: false, left: false, right: true }, Orientation::Horizontal));
    assert!(count(&flipped, top, ink) > 0 && count(&flipped, right, ink) > 0, "top and right when picked");
    assert_eq!(count(&flipped, bottom, ink) + count(&flipped, left, ink), 0);
    // Vertical side labels run along the edge: narrower across it than horizontal ones.
    // Columns with any text in them, inside the frame's left edge; antialiased text counts
    // too, as a thin system font (Windows) may leave no pixel of a stem fully dark.
    let across = |pm: &Pixmap| {
        let px = |mm: f64| (mm * PT_PER_MM) as u32;
        let shaded = |p: tiny_skia::PremultipliedColorU8| p.red() < 200 && p.green() < 200 && p.blue() < 200;
        (px(11.0)..px(30.0)).filter(|&x| (px(20.0)..px(80.0)).any(|y| pm.pixel(x, y).is_some_and(shaded))).count()
    };
    let horizontal = grid_page(g(GridEdges { top: false, bottom: false, left: true, right: false }, Orientation::Horizontal));
    let vertical = grid_page(g(GridEdges { top: false, bottom: false, left: true, right: false }, Orientation::Vertical));
    assert!(count(&vertical, left, ink) > 0);
    assert!(across(&vertical) < across(&horizontal), "{} columns, against {}", across(&vertical), across(&horizontal));
}

/// A grid's labels at its label size (#2581): at 16 points each is twice as tall as at the
/// default 8 and sits twice as far in from the edge past the ticks; at 8 they are where they
/// always were; out of range they are drawn in range.
#[test]
fn a_grids_labels_take_its_label_size() {
    use rustgis_map::layout::Grid;
    let (fw, fh, ppp) = (80.0 * PT_PER_MM, 80.0 * PT_PER_MM, 2.0);
    let map = Map::new("m");
    let view =
        View { width: fw, height: fh, pixels_per_point: ppp, camera: frame_camera(&map, Coord::new(0.0, 0.0), 2e8, 0.0, 0.0), sky: false };
    let labels = |label_size: f64| {
        let mut pm = Canvas::from_pixmap(Pixmap::new((fw * ppp) as u32, (fh * ppp) as u32).unwrap());
        let g = Grid { step: 30.0, tick_length: 0.0, label_size, ..Grid::default() };
        draw_grid(&mut pm, &map, &view, &g, false)
    };
    let (small, big) = (labels(GRID_LABEL_PT), labels(16.0));
    let bottom = |drawn: &[(Edge, [f32; 4])]| drawn.iter().find(|(e, _)| *e == Edge::Bottom).map(|(_, r)| *r).unwrap();
    let (s, b) = (bottom(&small), bottom(&big));
    assert!(((b[3] - b[1]) - 2.0 * (s[3] - s[1])).abs() < 1e-3, "{b:?} against {s:?}");
    let gap = |r: [f32; 4]| fh as f32 - r[3];
    assert!(gap(b) > gap(s), "farther in from the edge: {} against {}", gap(b), gap(s));
    // The default: 8 points, 1.33 pixels a point, 3 points past the ticks, its halo 1.2 lines.
    assert!(((s[3] - s[1]) - 8.0 * 1.33 * 1.2).abs() < 1e-3, "{s:?}");
    assert_eq!(labels(1e9), labels(100.0));
    assert_eq!(labels(f64::NAN), small);
}

/// A tile layer's credit on a frame with a labelled grid (#1767): in the bottom-right corner,
/// where it was drawn, it runs over the bottom labels; the frame moves it clear of every label,
/// whichever edges carry them and however the side ones run, and leaves the labels as the grid
/// drew them.
#[test]
fn a_frames_attribution_keeps_clear_of_its_grid_labels() {
    use rustgis_map::layout::{Grid, GridEdges};
    let (fw, fh, ppp) = (80.0 * PT_PER_MM, 80.0 * PT_PER_MM, 2.0);
    let map = Map::new("m");
    let view =
        View { width: fw, height: fh, pixels_per_point: ppp, camera: frame_camera(&map, Coord::new(0.0, 0.0), 2e8, 0.0, 0.0), sky: false };
    let credit = vec!["© OpenStreetMap contributors".to_string()];
    let white = || {
        let mut pm = Pixmap::new((fw * ppp) as u32, (fh * ppp) as u32).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        pm
    };
    // Whether each pixel is darkened by text, antialiased edges included.
    let shaded = |pm: &Pixmap| pm.pixels().iter().map(|p| p.red() < 200 && p.green() < 200 && p.blue() < 200).collect::<Vec<_>>();
    let grid = |edges, label_orientation| Grid { step: 30.0, color: Rgba::new(0, 0, 0, 0), edges, label_orientation, ..Grid::default() };
    // The grid's labels and the credit, each on its own frame (the credit `inset` from the
    // corner, else where the frame puts it); the pixels both darken.
    let overlap = |g: &Grid, credit: &[String], inset: Option<(f32, f32)>| {
        let mut labels_pm = Canvas::from_pixmap(white());
        let labels = draw_grid(&mut labels_pm, &map, &view, g, false);
        assert!(!labels.is_empty());
        let (w, h) = (fw as f32, fh as f32);
        let tw = crate::attribution_text(credit).unwrap().1;
        let inset = inset.unwrap_or_else(|| attribution_inset((w, h), tw, &labels));
        let mut credit_pm = Canvas::from_pixmap(white());
        crate::draw_attribution(&mut credit_pm, credit, ppp as f32, inset);
        shaded(labels_pm.pixmap()).iter().zip(shaded(credit_pm.pixmap())).filter(|(a, b)| **a && *b).count()
    };
    let default = grid(GridEdges::default(), Orientation::Horizontal);
    assert!(overlap(&default, &credit, Some((0.0, 0.0))) > 20, "in the corner, the credit runs over the bottom labels");
    for (bottom, right) in [(true, false), (true, true), (false, true)] {
        for orientation in [Orientation::Horizontal, Orientation::Vertical] {
            let g = grid(GridEdges { top: true, bottom, left: true, right }, orientation);
            assert_eq!(overlap(&g, &credit, None), 0, "bottom {bottom}, right {right}, {orientation:?}");
        }
    }
    // A credit wider than the frame (several layers' credits, a narrow frame) runs past both
    // edges; it keeps clear of the bottom and right labels all the same.
    let long = vec![
        "© OpenStreetMap contributors".to_string(),
        "MapBiomas, sistemas de irrigação (CC BY 4.0)".to_string(),
        "Instituto Brasileiro de Geografia e Estatística".to_string(),
    ];
    assert!(crate::attribution_text(&long).unwrap().1 > fw as f32);
    for (bottom, right) in [(true, true), (false, true)] {
        for orientation in [Orientation::Horizontal, Orientation::Vertical] {
            let g = grid(GridEdges { top: false, bottom, left: false, right }, orientation);
            assert_eq!(overlap(&g, &long, None), 0, "long credit: bottom {bottom}, right {right}, {orientation:?}");
        }
    }
    // Drawn with the frame's map: the labels' pixels are the grid's alone.
    let mut labels_pm = Canvas::from_pixmap(white());
    let all = grid(GridEdges { top: false, bottom: true, left: false, right: true }, Orientation::Horizontal);
    let labels = draw_grid(&mut labels_pm, &map, &view, &all, false);
    let img = crate::Rendered {
        pixmap: white(),
        labels: Vec::new(),
        unplaced: Vec::new(),
        attribution: credit.clone(),
        tiles_pending: false,
        data_pending: false,
        tile_errors: Vec::new(),
        partial: Vec::new(),
        pending_in_place: Vec::new(),
        recorded: None,
        label_layers: Vec::new(),
    };
    let framed = finish_frame(img, ppp, Some((&map, &view, &all, false)));
    let (a, b) = (labels_pm.pixmap(), framed.pixmap());
    assert_ne!(a.data(), b.data(), "the credit is drawn");
    for (_, [x0, y0, x1, y1]) in &labels {
        let px = |v: f32| (v * ppp as f32) as u32;
        for (x, y) in (px(*y0)..px(*y1)).flat_map(|y| (px(*x0)..px(*x1)).map(move |x| (x, y))) {
            assert_eq!(a.pixel(x, y), b.pixel(x, y), "({x}, {y})");
        }
    }
}

/// The credit stays in the corner with nothing under it, goes up above a bottom label and left
/// of a right one (above it, when going left would take it out of the frame), and pays no heed
/// to the top and left labels.
#[test]
fn attribution_inset_steps_round_the_labels_in_its_way() {
    let (size, tw) = ((200.0, 100.0), 60.0);
    assert_eq!(attribution_inset(size, tw, &[]), (0.0, 0.0));
    // A bottom label under the credit, another left of it.
    let under = [(Edge::Bottom, [150.0, 88.0, 170.0, 98.0]), (Edge::Bottom, [40.0, 88.0, 60.0, 98.0])];
    let (bottom, right) = attribution_inset(size, tw, &under);
    assert_eq!(right, 0.0);
    // The credit's foot (its centre plus half its height) sits 2 points over the label's top.
    assert!((100.0 - bottom - crate::ATTRIBUTION_SIZE * 0.9 + crate::ATTRIBUTION_SIZE * 0.8 - 86.0).abs() < 1e-3, "{bottom}");
    // A right label beside it once it is up: it moves left of it too.
    let both = [under[0], (Edge::Right, [180.0, 70.0, 197.0, 82.0])];
    let (b2, r2) = attribution_inset(size, tw, &both);
    assert_eq!(b2, bottom);
    assert!((200.0 - r2 - 4.0 - 178.0).abs() < 1e-3, "{r2}");
    // Labels on the other edges are not in its way.
    let others = [(Edge::Left, [130.0, 88.0, 196.0, 98.0]), (Edge::Top, [130.0, 88.0, 196.0, 98.0])];
    assert_eq!(attribution_inset(size, tw, &others), (0.0, 0.0));
    // Left of a right label it would leave the frame (180 points from x = 16, a label at
    // x = 163, it would start at x = -19): it goes above the label instead, still in the
    // corner's column.
    let side = [(Edge::Right, [163.0, 85.0, 197.0, 97.0])];
    let (b3, r3) = attribution_inset(size, 180.0, &side);
    assert_eq!(r3, 0.0);
    assert!((100.0 - b3 - crate::ATTRIBUTION_SIZE * 0.9 + crate::ATTRIBUTION_SIZE * 0.8 - 83.0).abs() < 1e-3, "{b3}");
    // No room above it either (a frame 20 points tall): as far left as the frame holds it,
    // starting at its left edge.
    let low = [(Edge::Right, [163.0, 3.0, 197.0, 15.0])];
    let (b4, r4) = attribution_inset((200.0, 20.0), 180.0, &low);
    assert_eq!(b4, 0.0);
    assert!((crate::attribution_center(200.0, 180.0, r4) - 90.0).abs() < 1e-3, "{r4}");
    // Wider than the frame: never left (it can't fit), above the label when there is room.
    let (b5, r5) = attribution_inset(size, 300.0, &side);
    assert_eq!((b5, r5), (b3, 0.0));
    let (b6, r6) = attribution_inset((200.0, 20.0), 300.0, &low);
    assert_eq!((b6, r6), (0.0, 0.0), "it stays where it was drawn");
}

#[test]
fn north_arrow_styles_differ_and_take_their_colours() {
    let draw = |style, fill| {
        let mut pm = Pixmap::new(80, 120).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            north_arrow(c, Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap(), style, fill, Rgba::rgb(0x22, 0x22, 0x22), 1.0, 0.0)
        });
        pm
    };
    let red_fill = Rgba::rgb(0xFF, 0, 0);
    let all: Vec<Pixmap> = ArrowStyle::ALL.iter().map(|&s| draw(s, red_fill)).collect();
    for (i, a) in all.iter().enumerate() {
        let filled = (0..120).flat_map(|y| (0..80).map(move |x| (x, y))).filter(|&(x, y)| a.pixel(x, y).is_some_and(red)).count();
        assert!(filled > 50, "{:?} uses the fill colour", ArrowStyle::ALL[i]);
        for b in &all[i + 1..] {
            assert_ne!(a.data(), b.data(), "each style draws differently");
        }
    }
}

#[test]
fn a_north_arrow_turns_with_its_frame() {
    // The mean position of the red arrowhead (its N and outline are dark), as an offset from
    // the middle of an 80 × 120 arrow.
    let lean = |bearing| {
        let mut pm = Pixmap::new(80, 120).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| {
            north_arrow(c, r, ArrowStyle::Triangle, Rgba::rgb(0xFF, 0, 0), Rgba::rgb(0x22, 0x22, 0x22), 1.0, bearing)
        });
        let pts: Vec<(f64, f64)> = (0..120)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .filter(|&(x, y)| pm.pixel(x, y).is_some_and(red))
            .map(|(x, y)| (f64::from(x), f64::from(y)))
            .collect();
        let n = pts.len().max(1) as f64;
        (pts.iter().map(|p| p.0).sum::<f64>() / n - 40.0, pts.iter().map(|p| p.1).sum::<f64>() / n - 60.0)
    };
    // North up: the arrowhead's bulk (its base) lies below the middle, its tip above.
    let (x, y) = lean(0.0);
    assert!(x.abs() < 1.0 && y > 3.0, "{x}, {y}");
    // East up (the map turned 90°): north points left, so the base lies right of the middle.
    let (x, y) = lean(90.0);
    assert!(x > 3.0 && y.abs() < 1.0, "{x}, {y}");
    let (x, _) = lean(-90.0);
    assert!(x < -3.0, "{x}");
}

/// A scale bar's default colour and fill.
const DEFAULT_COLORS: (Rgba, Rgba) = (Rgba::rgb(0x22, 0x22, 0x22), Rgba::rgb(0xFF, 0xFF, 0xFF));

/// A white `width` x 60 pixmap with `bar` drawn on it at 100 m of ground a point, a pixel a
/// point, with a decimal point, in the default colours.
fn draw_bar(bar: &BarOptions<'_>, width: u32) -> Pixmap {
    let mut pm = Pixmap::new(width, 60).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 0.0, width as f32, 60.0).unwrap();
    crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, bar, DEFAULT_COLORS, 1.0, false));
    pm
}

/// Dark runs along row `y`.
fn dark_runs(pm: &Pixmap, y: u32) -> usize {
    let dark: Vec<bool> = (0..pm.width()).map(|x| pm.pixel(x, y).is_some_and(ink)).collect();
    dark.windows(2).filter(|w| !w[0] && w[1]).count() + usize::from(dark.first() == Some(&true))
}

/// The first and last columns with anything but white on row `y`.
fn marked_span(pm: &Pixmap, y: u32) -> Option<(u32, u32)> {
    let marked = |x: &u32| pm.pixel(*x, y).is_some_and(|p| p.red() < 200 || p.green() < 200 || p.blue() < 200);
    let first = (0..pm.width()).find(marked)?;
    let last = (0..pm.width()).rev().find(marked)?;
    Some((first, last))
}

/// The rows holding ink.
fn bar_ink_rows(pm: &Pixmap) -> Vec<u32> {
    (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(ink))).collect()
}

/// How much darker than white the pixels in rows `from..=to` are, summed.
fn darkness(pm: &Pixmap, from: u32, to: u32) -> u32 {
    (from..=to)
        .flat_map(|y| (0..pm.width()).map(move |x| (x, y)))
        .filter_map(|(x, y)| pm.pixel(x, y))
        .map(|p| 255 - u32::from(p.red()))
        .sum()
}

/// 64-bit FNV-1a over the pixmaps' bytes.
#[cfg(not(target_arch = "wasm32"))]
fn fnv(pms: &[Pixmap]) -> u64 {
    pms.iter().flat_map(|p| p.data().iter()).fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// 64-bit FNV-1a over one byte slice.
#[cfg(not(target_arch = "wasm32"))]
fn fnv_bytes(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// A plain scale bar on a white `w` x 60 pixmap, in the default colours.
#[cfg(not(target_arch = "wasm32"))]
fn draw_default(style: ScaleBarStyle, units: ScaleUnits, divisions: usize, ground: f64, w: u32, ppp: f64, comma: bool) -> Pixmap {
    let mut pm = Pixmap::new(w, 60).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 0.0, w as f32, 60.0).unwrap();
    let bar = BarOptions { units, divisions, style, ..BarOptions::default() };
    crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, ground, &bar, DEFAULT_COLORS, ppp, comma));
    pm
}

/// How [`plain_texts_draw_as_before`]'s pages drew, and what their cut-off check found, before
/// texts had a block alignment, line spacing or case; in the built-in face.
#[cfg(not(target_arch = "wasm32"))]
const PINNED_TEXT_PIXELS: u64 = 0x0556_a0ef_54c0_2e87;
#[cfg(not(target_arch = "wasm32"))]
const PINNED_TEXT_LOSSES: u64 = 0x26af_01ce_67e5_7cfd;

/// A text with none of the block settings draws, and is measured for lines that don't fit and ink
/// off the page, exactly as main did: every page at 96 dpi, and the first eight (left-aligned,
/// unwrapped, upright and turned a quarter) at 300 dpi too, which is enough to hold the scaling
/// of the line pitch. In a process of its own, told to use the built-in face before any text.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn plain_texts_draw_as_before() {
    if std::env::var_os("RUSTGIS_TEXT_PIN_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "layout::tests::plain_texts_draw_as_before", "--nocapture"])
            .env("RUSTGIS_TEXT_PIN_CHILD", "1")
            .env("RUSTGIS_BUILTIN_FONTS", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "the test didn't run in the child process");
        return;
    }
    let info = PageInfo { page: 2, pages: 3, page_name: String::new() };
    let (mut drawn, mut measured) = (Vec::new(), String::new());
    for (i, l) in crate::layout::plain_text_pages().iter().enumerate() {
        for dpi in if i < 8 { &[96.0, 300.0][..] } else { &[96.0][..] } {
            drawn.push(render(l, &mut [], *dpi, &info).unwrap());
            measured.push_str(&format!("{:?}\n", text_losses(l, &[], *dpi, &info)));
        }
    }
    // The pages exercise both checks.
    assert!(measured.contains("below_box: true") && measured.contains("off_page: true"), "{measured}");
    let losses = fnv_bytes(measured.as_bytes());
    assert_eq!((fnv(&drawn), losses), (PINNED_TEXT_PIXELS, PINNED_TEXT_LOSSES), "{:#x} {:#x}", fnv(&drawn), losses);
}

/// How [`scale_bars_without_details_draw_as_before`]'s bars drew before scale bars had details,
/// their labels in the built-in face.
#[cfg(not(target_arch = "wasm32"))]
const PINNED: u64 = 0x3975_8d68_da45_882b;

/// A bar with none of the details draws exactly as main drew it: every style, unit, division
/// count, ground resolution, density and decimal mark. Its labels are drawn in the built-in
/// face, whatever fonts the machine has: in a process of its own, told so before any text.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scale_bars_without_details_draw_as_before() {
    if std::env::var_os("RUSTGIS_SCALE_BAR_PIN_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "layout::tests::scale_bars_without_details_draw_as_before", "--nocapture"])
            .env("RUSTGIS_SCALE_BAR_PIN_CHILD", "1")
            .env("RUSTGIS_BUILTIN_FONTS", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "the test didn't run in the child process");
        return;
    }
    let mut drawn = Vec::new();
    for style in ScaleBarStyle::ALL {
        for units in [ScaleUnits::Auto, ScaleUnits::Miles, ScaleUnits::Feet, ScaleUnits::Meters] {
            for divisions in [1, 4, 10] {
                for ground in [0.37, 100.0, 7.84] {
                    for (w, ppp) in [(93u32, 1.0), (300, 1.0), (300, 300.0 / 72.0)] {
                        for comma in [false, true] {
                            drawn.push(draw_default(style, units, divisions, ground, w, ppp, comma));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(fnv(&drawn), PINNED, "{:#x}", fnv(&drawn));
}

#[test]
fn scale_bars_count_in_their_units_and_divisions() {
    // 100 m of ground per point: about 25 km across the bar.
    let draw = |(units, divisions, style)| draw_bar(&BarOptions { units, divisions, style, ..BarOptions::default() }, 300);
    // Dark runs along the bar's middle row: one per dark division of an alternating bar.
    let runs = |pm: &Pixmap| {
        let y = (60.0 * 0.62) as u32;
        let dark: Vec<bool> = (0..pm.width()).map(|x| pm.pixel(x, y).is_some_and(ink)).collect();
        dark.windows(2).filter(|w| !w[0] && w[1]).count() + usize::from(dark.first() == Some(&true))
    };
    let four = draw((ScaleUnits::Auto, 4, ScaleBarStyle::Alternating));
    let five = draw((ScaleUnits::Auto, 5, ScaleBarStyle::Alternating));
    assert_eq!(runs(&four), 2, "two dark divisions");
    assert_eq!(runs(&five), 3, "three dark divisions, the last at the end");
    // Miles and kilometres give bars of different lengths and labels.
    assert_ne!(draw((ScaleUnits::Miles, 4, ScaleBarStyle::Alternating)).data(), four.data());
    // A line or a stepped line fills no division.
    for style in [ScaleBarStyle::Line, ScaleBarStyle::Stepped] {
        let pm = draw((ScaleUnits::Auto, 4, style));
        let dark = (0..300).filter(|&x| pm.pixel(x, 37).is_some_and(ink)).count();
        assert!(dark < 20, "{style:?}: {dark} dark pixels across the bar");
        assert_ne!(pm.data(), four.data());
    }
    let with = |f: &dyn Fn(&mut BarOptions<'static>)| {
        let mut b = BarOptions::default();
        f(&mut b);
        draw_bar(&b, 300)
    };
    let runs = |pm: &Pixmap| dark_runs(pm, 37);
    assert_eq!(runs(&with(&|_| {})), 2);
    // The first division cut in four alternating parts from zero, the first dark, and the
    // divisions after going on from them: parts 0 and 2, then divisions 1 and 3.
    assert_eq!(runs(&with(&|b| b.subdivisions = 4)), 4);
    assert_eq!(with(&|b| b.subdivisions = 1).data(), with(&|_| {}).data(), "one part is the division whole");
    // A stepped bar's upper steps are the alternating bar's dark divisions, after the cut one too:
    // its thin line, drawn soft, marks the bar's top row.
    let cut4 = |style| {
        let bar = BarOptions { style, subdivisions: 4, ..BarOptions::default() };
        let placed = bar_layout(Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap(), 100.0, &bar, 1.0, false).unwrap();
        (draw_bar(&bar, 300), placed)
    };
    let (alternating, placed) = cut4(ScaleBarStyle::Alternating);
    let (stepped, _) = cut4(ScaleBarStyle::Stepped);
    let seg = placed.len / placed.divs as f32;
    let top = placed.y0.round() as u32;
    let mid = |j: usize| (placed.x0 + seg * (j as f32 + 0.5)) as u32;
    let dark: Vec<bool> = (placed.before + 1..placed.divs).map(|j| alternating.pixel(mid(j), 37).is_some_and(ink)).collect();
    let upper: Vec<bool> = (placed.before + 1..placed.divs)
        .map(|j| (top - 1..=top + 1).any(|y| stepped.pixel(mid(j), y).is_some_and(|p| p.red() < 200)))
        .collect();
    assert_eq!(dark, [true, false, true], "divisions 1 and 3 dark");
    assert_eq!(upper, dark, "the stepped bar's upper steps");
    // Two whole divisions before zero: six divisions, three dark.
    assert_eq!(runs(&with(&|b| b.divisions_before_zero = 2)), 3);
    assert_eq!(bar_ink_rows(&with(&|b| b.divisions_before_zero = 2)), bar_ink_rows(&with(&|_| {})), "the same rows, bar and labels");
    // Across zero the light division before it meets a dark part: the bar's first ink is at
    // zero, as on the same bar uncut, with the light division left of it.
    let first_ink = |pm: &Pixmap| (0..pm.width()).find(|&x| pm.pixel(x, 37).is_some_and(ink));
    let uncut = with(&|b| b.divisions_before_zero = 1);
    let cut = with(&|b| {
        b.divisions_before_zero = 1;
        b.subdivisions = 4;
    });
    let zero = first_ink(&uncut).unwrap();
    assert!(zero > 20, "a division before zero: {zero}");
    assert_eq!(first_ink(&cut), Some(zero), "dark right of zero");
    assert!(cut.pixel(zero + 1, 37).is_some_and(ink) && !cut.pixel(zero - 2, 37).is_some_and(ink), "dark right of zero, light left");
    // A single or stepped line gets short ticks inside its first division.
    for style in [ScaleBarStyle::Line, ScaleBarStyle::Stepped] {
        let plain = with(&|b| b.style = style);
        let cut = with(&|b| {
            b.style = style;
            b.subdivisions = 3;
        });
        assert!(darkness(&cut, 35, 39) > darkness(&plain, 35, 39) + 100, "{style:?}");
    }
}

/// A bar whose first negative label leaves it no room still draws: the label takes at most
/// half the bar's room, reaching left of the box past that.
#[test]
fn a_scale_bar_before_zero_draws_where_its_first_label_leaves_no_room() {
    let bar = BarOptions { text_size: 24.0, divisions_before_zero: 2, divisions: 10, ..BarOptions::default() };
    let mut pm = Pixmap::new(40, 60).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 0.0, 40.0, 60.0).unwrap();
    // Filled in its colour throughout, so along its middle the bar is one solid run.
    let solid = (DEFAULT_COLORS.0, DEFAULT_COLORS.0);
    crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &bar, solid, 1.0, false));
    // Moved down so its big labels reach no higher than the box's top, the bar is drawn under
    // them: along its middle row, one solid run, half the bar's room at least (34 px, the 1 km
    // that fits in 17), not a sliver.
    let placed = bar_layout(r, 100.0, &bar, 1.0, false).unwrap();
    let middle = (placed.y0 + placed.bar_h / 2.0) as u32;
    assert!(placed.y0 > 30.0, "moved down: {}", placed.y0);
    assert!(dark_runs(&pm, middle) > 0, "the bar is drawn");
    let (a, b) = marked_span(&pm, middle).unwrap();
    assert!(b - a >= 8, "the bar keeps half its room: {a}..{b}");
}

#[test]
fn scale_bars_fit_their_box_by_the_strategy() {
    // 79 px to use at 100 m a point: 7.9 km of ground, in kilometres.
    let width = |fit| {
        let bar = BarOptions { units: ScaleUnits::Kilometers, fit, ..BarOptions::default() };
        let (a, b) = marked_span(&draw_bar(&bar, 93), 37).unwrap();
        b - a
    };
    assert!((width(ScaleBarFit::AdjustDivisionValue) as i32 - 50).abs() <= 2, "a round 5 km in four 1.25 km divisions");
    assert!((width(ScaleBarFit::AdjustDivision) as i32 - 70).abs() <= 2, "seven round 1 km divisions");
    assert!((width(ScaleBarFit::AdjustWidth) as i32 - 40).abs() <= 2, "four 1 km divisions, narrower than the box");
    // Ten asked: fifteen 0.5 km divisions would fit, so the division steps up to 1 km, seven.
    let bar = BarOptions { units: ScaleUnits::Kilometers, fit: ScaleBarFit::AdjustDivision, divisions: 10, ..BarOptions::default() };
    let (a, b) = marked_span(&draw_bar(&bar, 93), 37).unwrap();
    assert!(((b - a) as i32 - 70).abs() <= 2, "seven 1 km divisions, not ten of 0.5 km: {}", b - a);
    // Extreme grounds, every fit, divisions before zero: never past the box, never empty.
    for ground in [1e-4, 0.37, 100.0, 1e5, 1e7] {
        for fit in ScaleBarFit::ALL {
            for before in [0, 2] {
                let bar = BarOptions { fit, divisions_before_zero: before, divisions: 10, ..BarOptions::default() };
                let mut pm = Pixmap::new(120, 60).unwrap();
                pm.fill(tiny_skia::Color::WHITE);
                let r = Rect::from_xywh(10.0, 0.0, 100.0, 60.0).unwrap();
                crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, ground, &bar, DEFAULT_COLORS, 1.0, false));
                let (a, b) = marked_span(&pm, 37).unwrap_or((0, 0));
                assert!(b > a && b < 110, "{ground} {fit:?} {before}: {a}..{b}");
            }
        }
    }
    // Two divisions before zero count toward the ten: in a wide room, ten in all, never twelve.
    let bar =
        BarOptions { units: ScaleUnits::Kilometers, fit: ScaleBarFit::AdjustDivision, divisions_before_zero: 2, ..BarOptions::default() };
    let mut most = 0;
    for w in (300..1400).step_by(23) {
        let b = bar_layout(Rect::from_xywh(0.0, 0.0, w as f32, 60.0).unwrap(), 100.0, &bar, 1.0, false).unwrap();
        assert!(b.divs <= rustgis_map::layout::MAX_SCALE_DIVISIONS && b.before == 2, "{w} px: {} divisions", b.divs);
        let pm = draw_bar(&bar, w);
        let middle = (b.y0 + b.bar_h / 2.0) as u32;
        // An alternating bar's dark divisions: half of them, the first before zero dark.
        assert_eq!(dark_runs(&pm, middle), b.divs.div_ceil(2), "{w} px");
        let (a, z) = marked_span(&pm, middle).unwrap();
        assert!(((z - a) as f32 - b.len).abs() <= 2.0, "{w} px: {a}..{z} for {}", b.len);
        most = most.max(b.divs);
    }
    assert_eq!(most, rustgis_map::layout::MAX_SCALE_DIVISIONS, "a wide room reaches ten");
    // Ten asked and two before zero: ten in all under every fit, not twelve.
    for fit in ScaleBarFit::ALL {
        let bar = BarOptions { fit, divisions: 10, divisions_before_zero: 2, ..BarOptions::default() };
        let b = bar_layout(Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap(), 100.0, &bar, 1.0, false).unwrap();
        assert!(b.divs <= rustgis_map::layout::MAX_SCALE_DIVISIONS, "{fit:?}: {} divisions", b.divs);
        let middle = (b.y0 + b.bar_h / 2.0) as u32;
        assert!(dark_runs(&draw_bar(&bar, 300), middle) <= 5, "{fit:?}: at most five dark of ten");
    }
}

/// The narrowest element keeps its bar where an export rebuilds it from an f32 rectangle: near
/// an A2 sheet's right edge, at 300 and 600 dpi (#1379).
#[test]
fn a_scale_bar_keeps_its_bar_near_the_right_edge_at_print_density() {
    // As an export places an element: page mm at `k` pixels a mm, `ppp` pixels a point.
    let pt_per_mm = rustgis_map::layout::PT_PER_MM;
    for i in 0..400 {
        let ground = 10f64.powf(-1.0 + 4.0 * f64::from(i) / 400.0);
        for widest_mm in [25.0, 63.0, 126.0] {
            let w_pt = scale_bar_width_pt(ground, widest_mm * pt_per_mm, (ScaleUnits::Auto, 4, SCALE_BAR_TEXT_PT), false, 4.0).unwrap();
            let plan = bar_plan(ground, w_pt, ScaleUnits::Auto);
            for (x_mm, dpi) in [(380.0, 300.0), (560.0, 300.0), (380.0, 600.0), (560.0, 600.0)] {
                let k = dpi / 25.4;
                let ppp = k / pt_per_mm;
                let r = Rect::from_xywh((x_mm * k) as f32, 0.0, (w_pt / pt_per_mm * k) as f32, 10.0).unwrap();
                let drawn = bar_plan(ground, f64::from(r.width()) / ppp, ScaleUnits::Auto);
                assert_eq!(drawn, plan, "{ground} m a point, {widest_mm} mm at most, x {x_mm} mm, {dpi} dpi");
            }
        }
    }
}

/// The narrowest element for a scale bar draws the very bar a wider one does, labels and all,
/// within itself (#1379).
#[test]
fn a_scale_bar_draws_the_same_in_its_narrowest_element() {
    let draw = |ground: f64, width: f32| {
        let mut pm = Pixmap::new(320, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(0.0, 0.0, width, 60.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, ground, &BarOptions::default(), DEFAULT_COLORS, 1.0, true));
        pm
    };
    // 20 km on a bar of 300 points takes two thirds of it; 10 km at 37.2 m a point, a third.
    for (ground, narrower_than) in [(100.0, 260.0), (37.2, 190.0), (78.4, 300.0)] {
        let w = scale_bar_width_pt(ground, 300.0, (ScaleUnits::Auto, 4, SCALE_BAR_TEXT_PT), true, 4.0).unwrap();
        assert!(w < narrower_than, "{ground} m a point: {w} points");
        let (wide, narrow) = (draw(ground, 300.0), draw(ground, w as f32));
        assert_eq!(wide.data(), narrow.data(), "{ground} m a point: the same bar in {w} points");
        let past = (w.ceil() as u32..320).flat_map(|x| (0..60).map(move |y| (x, y))).filter(|&(x, y)| narrow.pixel(x, y).is_some_and(ink));
        assert_eq!(past.count(), 0, "{ground} m a point: nothing drawn past {w} points");
    }
    assert_eq!(scale_bar_width_pt(0.0, 300.0, (ScaleUnits::Auto, 4, SCALE_BAR_TEXT_PT), false, 4.0), None);
    // Miles count in other lengths: the box still draws the same bar.
    let w = scale_bar_width_pt(100.0, 300.0, (ScaleUnits::Miles, 4, SCALE_BAR_TEXT_PT), false, 4.0).unwrap();
    assert!(w <= 300.0 && bar_plan(100.0, w, ScaleUnits::Miles) == bar_plan(100.0, 300.0, ScaleUnits::Miles));
}

#[test]
fn scale_bars_draw_in_their_colours() {
    let mut pm = Pixmap::new(300, 60).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    type Px = tiny_skia::PremultipliedColorU8;
    // A red colour and a blue fill.
    let colors = (Rgba::rgb(0xD0, 0x10, 0x10), Rgba::rgb(0x10, 0x10, 0xD0));
    let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
    crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &BarOptions::default(), colors, 1.0, false));
    let count = |hit: fn(Px) -> bool, y: u32| (0..300).filter(|&x| pm.pixel(x, y).is_some_and(hit)).count();
    let y = (60.0 * 0.62) as u32;
    // Along the bar: the dark divisions in the colour, the light ones in the fill, nothing in
    // the old ink.
    assert!(count(red, y) > 40, "red divisions");
    assert!(count(|p| p.blue() > 180 && p.red() < 90 && p.green() < 90, y) > 40, "blue divisions");
    assert_eq!(count(ink, y), 0, "no default ink left");
    // The labels above the bar are in the colour too.
    let labels = (0..y.saturating_sub(8)).map(|row| count(|p| p.red() > 120 && p.green() < 90 && p.blue() < 90, row)).sum::<usize>();
    assert!(labels > 0, "red labels");
    assert_eq!((0..y.saturating_sub(8)).map(|row| count(ink, row)).sum::<usize>(), 0, "no ink labels");
}

/// A detailed scale bar's box (#1379) is the narrowest that draws the same bar with all it
/// has: its unit after the bar written in full, its label before zero, and nothing past the
/// box's side but its padding. A plain bar's is the one it always had.
#[test]
fn a_detailed_scale_bars_box_holds_all_it_draws() {
    let mut bar = ElementKind::scale_bar(0);
    if let ElementKind::ScaleBar { unit_label, unit_label_position, divisions_before_zero, .. } = &mut bar {
        (*unit_label, *unit_label_position, *divisions_before_zero) = (Some("Kilometres".into()), UnitLabelPosition::AfterBar, 1);
    }
    let options = BarOptions::of(&bar).unwrap();
    let w = scale_bar_box_width_pt(100.0, 300.0, &bar, false, 4.0).unwrap();
    assert!(w < 300.0, "{w}");
    let at = |w: f64| bar_layout(Rect::from_xywh(0.0, 0.0, w as f32, 60.0).unwrap(), 100.0, &options, 1.0, false).unwrap();
    let (wide, narrow) = (at(300.0), at(w));
    assert_eq!((narrow.total, narrow.divs, narrow.len), (wide.total, wide.divs, wide.len), "the same bar");
    assert_eq!(narrow.unit.as_ref().map(|u| u.text.as_str()), Some("Kilometres"), "its unit in full");
    assert!(f64::from(narrow.right) + 4.0 <= w + 1e-6, "{} in {w}", narrow.right);
    // A little narrower, it would draw another bar or cut what it writes.
    let less = at(w * 0.98);
    let cut = less.total != wide.total || less.unit.as_ref().map(|u| u.text.as_str()) != Some("Kilometres");
    assert!(cut || f64::from(less.right) + 4.0 > w * 0.98, "the narrowest");
    let plain = ElementKind::scale_bar(0);
    assert_eq!(
        scale_bar_box_width_pt(100.0, 300.0, &plain, false, 4.0),
        scale_bar_width_pt(100.0, 300.0, (ScaleUnits::Auto, 4, SCALE_BAR_TEXT_PT), false, 4.0)
    );
    assert_eq!(scale_bar_box_width_pt(0.0, 300.0, &bar, false, 4.0), None);
}

/// A scale bar's labels at their text size (#2581): at 16 points their room over the bar and
/// between them is twice the default's and the bar keeps its length and height; 8 points draws
/// as the bar always did; the width a box needs counts its last label at that size.
#[test]
fn a_scale_bars_labels_take_its_text_size() {
    let plain = |divisions: usize, text_size: f64| BarOptions { divisions, text_size, ..BarOptions::default() };
    let r = Rect::from_xywh(0.0, 0.0, 400.0, 80.0).unwrap();
    let at = |pt: f64| bar_layout(r, 100.0, &plain(4, pt), 1.0, false).unwrap();
    let (small, big) = (at(SCALE_BAR_TEXT_PT), at(16.0));
    assert_eq!((small.len, small.bar_h, small.y0), (big.len, big.bar_h, big.y0), "the bar keeps its size");
    assert!((big.px - 2.0 * small.px).abs() < 1e-3, "{} against {}", big.px, small.px);
    // Its inset follows, and more: its first label reaches no further left than at 8 points,
    // where its "0" pokes a little past the box's side.
    let reach = |b: &BarLayout| b.x0 - text::measure(&b.labels[0].1, b.px / 1.33, false) / 2.0;
    assert!(reach(&small) < 0.0, "{}", reach(&small));
    assert!(big.x0 >= 2.0 * BAR_INSET - 1e-3 && (reach(&big) - reach(&small)).abs() < 1e-3, "{} {}", big.x0, reach(&big));
    let over = |b: &BarLayout| b.y0 - b.label_y;
    assert!((over(&big) - 2.0 * over(&small)).abs() < 1e-3, "its room over the bar follows");
    // The default draws the bar as before: an 8-point label 1.33 pixels a point, 2 pixels in.
    assert_eq!((small.px, small.x0), (8.0 * 1.33, BAR_INSET));
    // Out of range or non-finite, as a program may hand it, it is drawn in range.
    assert_eq!(at(1e9).px, at(100.0).px);
    assert_eq!(at(f64::NAN).px, small.px);
    // Labels that would touch at the larger size are left out, never the ends.
    let crowded = bar_layout(Rect::from_xywh(0.0, 0.0, 120.0, 80.0).unwrap(), 100.0, &plain(10, 40.0), 1.0, false).unwrap();
    assert!(crowded.labels.len() < 11, "{} labels", crowded.labels.len());
    assert_eq!(crowded.labels.first().map(|l| l.1.as_str()), Some("0"));
    // In a box 34 pixels tall (9 mm at a point a pixel), the default leaves room over its
    // labels; at 14 points the bar moves down so they reach no higher than the box's top, and
    // at 40 the box is too short for them and the bar, which is drawn below it.
    let in_box = |h: f32, pt: f64| bar_layout(Rect::from_xywh(0.0, 0.0, 400.0, h).unwrap(), 100.0, &plain(4, pt), 1.0, false).unwrap();
    let rise = |b: &BarLayout| b.label_y - b.px * BAR_LABEL_RISE;
    let (eight, fourteen, forty) = (in_box(34.0, 8.0), in_box(34.0, 14.0), in_box(34.0, 40.0));
    assert!((eight.y0 - (34.0 * 0.62 - eight.bar_h / 2.0)).abs() < 1e-4 && rise(&eight) > 0.0, "the default where it was");
    assert!(fourteen.y0 > eight.y0 && rise(&fourteen).abs() < 1e-3 && !fourteen.cramped, "moved down: {}", fourteen.y0);
    assert!(forty.cramped && rise(&forty).abs() < 1e-3, "too short: the bar at {}", forty.y0);
    assert!(!eight.cramped && !fourteen.cramped);
    // Sideways, its last label at 40 points past a narrow box's right side, its bar the same
    // length: too narrow, its box tall enough. At 8 points the same box holds it.
    let narrow = |pt: f64| bar_layout(Rect::from_xywh(0.0, 0.0, 90.0, 400.0).unwrap(), 100.0, &plain(4, pt), 1.0, false).unwrap();
    let (n8, n40) = (narrow(8.0), narrow(40.0));
    assert!(!n8.cramped && n40.cramped && n40.y0 + n40.bar_h < 400.0 && n40.len == n8.len, "{} {}", n40.y0, n40.len);
    // Its end label, always drawn as its first is, running into the first above the default
    // size reaches past the box's side too: the box is too small, and said.
    for pt in [9.0, 12.0, 20.0, 40.0, 100.0] {
        for w in (20..400).step_by(7) {
            let Some(b) = bar_layout(Rect::from_xywh(0.0, 0.0, w as f32, 400.0).unwrap(), 100.0, &plain(4, pt), 1.0, false) else {
                continue;
            };
            let half = |s: &str| text::measure(s, b.px / 1.33, false) / 2.0;
            let ((fx, fs), (ex, es)) = (&b.labels[0], &b.labels[b.labels.len() - 1]);
            let clash = b.labels.len() > 1 && ex - half(es) < fx + half(fs);
            assert!(!clash || b.cramped, "{pt} pt in {w} px: its end label over its first, not said");
        }
    }
    // A publication's short box (17 pixels): the default reaches over it, as it always did,
    // and a larger size reaches no higher.
    let (short8, short12) = (in_box(17.0, 8.0), in_box(17.0, 12.0));
    assert!((short8.y0 - (17.0 * 0.62 - short8.bar_h / 2.0)).abs() < 1e-4 && rise(&short8) < 0.0);
    assert!((rise(&short12) - rise(&short8)).abs() < 1e-3, "{} against {}", rise(&short12), rise(&short8));
    let width = |pt| scale_bar_width_pt(100.0, 300.0, (ScaleUnits::Auto, 4, pt), false, 4.0).unwrap();
    assert!(width(40.0) > width(SCALE_BAR_TEXT_PT), "{} against {}", width(40.0), width(SCALE_BAR_TEXT_PT));
}

/// A scale bar's labels that don't fit are reported as a text is, measured where they are
/// drawn (#2581): at 40 points a box too short for them and its bar, and a label past the
/// page's right edge.
#[test]
fn a_scale_bars_labels_off_the_page_are_reported() {
    let maps = vec![Map::new("m")];
    let mut l = Layout::new("Page", 120.0, 90.0);
    let frame = l.add(
        "Frame",
        (10.0, 30.0, 100.0, 50.0),
        serde_json::from_value(serde_json::json!({"MapFrame":{"map":"m", "center":{"x":0,"y":0}, "scale":1000000}})).unwrap(),
    );
    let bar = l.add("Scale Bar", (10.0, 0.0, 100.0, 12.0), ElementKind::scale_bar(frame));
    let info = PageInfo::default();
    assert!(clipped_text(&l, &maps, 96.0, &info).is_empty(), "8 points fit");
    if let Some(ElementKind::ScaleBar { text_size, .. }) = l.element_mut(bar).map(|e| &mut e.kind) {
        *text_size = 40.0;
    }
    assert_eq!(clipped_text(&l, &maps, 96.0, &info), vec![(bar, "Scale Bar".into())]);
    let note = clipped_text_notice(&l, &clipped_text(&l, &maps, 96.0, &info)).unwrap();
    assert!(note.ends_with("move it inside the page or make its box larger, or reduce its text size"), "{note}");
    let loss = text_loss(&l, &maps, 96.0, &info, bar).unwrap();
    assert!(loss.below_box && scale_bar_fit_notices(std::slice::from_ref(&loss))[0].contains("its box is too small"), "{loss:?}");
    // A tall box holds them at 20 points; off the page's right edge, its last label is cut.
    l.element_mut(bar).unwrap().h = 60.0;
    if let Some(ElementKind::ScaleBar { text_size, .. }) = l.element_mut(bar).map(|e| &mut e.kind) {
        *text_size = 20.0;
    }
    assert!(clipped_text(&l, &maps, 96.0, &info).is_empty(), "a tall box holds them");
    l.element_mut(bar).unwrap().x = 60.0;
    let loss = text_loss(&l, &maps, 96.0, &info, bar).unwrap();
    assert!(
        !loss.below_box && loss.off_page && scale_bar_fit_notices(std::slice::from_ref(&loss))[0].contains("runs off the page"),
        "{loss:?}"
    );
    // Too small and off the page both: each said.
    l.element_mut(bar).unwrap().h = 12.0;
    if let Some(ElementKind::ScaleBar { text_size, .. }) = l.element_mut(bar).map(|e| &mut e.kind) {
        *text_size = 40.0;
    }
    let both = text_loss(&l, &maps, 96.0, &info, bar).unwrap();
    let said = scale_bar_fit_notices(std::slice::from_ref(&both));
    assert!(both.below_box && both.off_page && said.len() == 2 && said[1].contains("runs off the page"), "{both:?} {said:?}");
    // Several bars are told in one notice a kind, their count and names.
    let small = TextLoss { below_box: true, off_page: false, ..loss.clone() };
    let two = scale_bar_fit_notices(&[small.clone(), TextLoss { name: "B".into(), ..small.clone() }]);
    assert!(two.len() == 1 && two[0].starts_with("2 scale bars (\"Scale Bar\", \"B\"): their boxes are too small"), "{two:?}");
    assert_eq!(scale_bar_fit_notices(&[small, loss]).len(), 2, "a notice a kind");
    // Drawn without ink, its labels show nothing and aren't measured, as a text's.
    if let Some(ElementKind::ScaleBar { color, .. }) = l.element_mut(bar).map(|e| &mut e.kind) {
        *color = Rgba::new(0, 0, 0, 0);
    }
    assert!(text_loss(&l, &maps, 96.0, &info, bar).is_none(), "a bar without ink");
    l.element_mut(bar).unwrap().visible = false;
    assert!(clipped_text(&l, &maps, 96.0, &info).is_empty(), "a hidden bar draws nothing");
}

/// A 100 x 60 mm page with a 90 x 20 mm frame at its foot showing a map at 1:10,000,000: the
/// map, the page and the frame's id.
fn colour_test_page() -> (Vec<Map>, Layout, u32) {
    let maps = vec![Map::new("m")];
    let mut l = Layout::new("Layout", 100.0, 60.0);
    let frame = l.add(
        "Map",
        (5.0, 35.0, 90.0, 20.0),
        ElementKind::MapFrame {
            map: "m".into(),
            center: Coord::new(0.0, 0.0),
            scale: 10_000_000.0,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    (maps, l, frame)
}

/// A page draws a scale bar in its own colour and fill: the element's, through `draw_page`.
#[test]
fn a_page_draws_its_scale_bar_in_its_colours() {
    let (mut maps, mut l, frame) = colour_test_page();
    let (crimson, navy) = (Rgba::rgb(0xD0, 0x10, 0x10), Rgba::rgb(0x10, 0x10, 0xD0));
    let mut bar = ElementKind::scale_bar(frame);
    if let ElementKind::ScaleBar { color, fill, .. } = &mut bar {
        (*color, *fill) = (crimson, navy);
    }
    l.add("Scale Bar", (5.0, 5.0, 90.0, 20.0), bar);
    let pm = render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap();
    // Above the frame (30 mm of the page's 60): the bar's divisions in its colour and fill, and
    // nothing in the default ink.
    let above = pm.height() / 2;
    let count = |hit: fn(tiny_skia::PremultipliedColorU8) -> bool| {
        (0..above).flat_map(|y| (0..pm.width()).map(move |x| (x, y))).filter(|&(x, y)| pm.pixel(x, y).is_some_and(hit)).count()
    };
    assert!(count(red) > 100, "red divisions and labels");
    assert!(count(|p| p.blue() > 180 && p.red() < 90 && p.green() < 90) > 100, "blue divisions");
    assert_eq!(count(ink), 0, "no default ink");
}

#[test]
fn scale_numbers_have_decimals_and_thousands() {
    // Auto: as each label needs, with no separators, exactly as before.
    assert_eq!(bar_number(5000.0, 1000.0, None, false), "5000");
    assert_eq!(bar_number(0.0125, 0.0125, None, false), "0.0125");
    assert_eq!(bar_number(-500.0, 500.0, None, false), "-500");
    assert_eq!(bar_number(2.5, 1.25, None, true), "2,5");
    // Fixed decimals with their thousands apart, the marks swapped under a decimal comma.
    assert_eq!(bar_number(5000.0, 1000.0, Some(0), false), "5,000");
    assert_eq!(bar_number(5000.0, 1000.0, Some(0), true), "5.000");
    assert_eq!(bar_number(3.75, 1.25, Some(0), false), "4", "fixed at none, rounded");
    assert_eq!(bar_number(1250.0, 250.0, Some(2), false), "1,250.00");
    assert_eq!(bar_number(1250.0, 250.0, Some(2), true), "1.250,00");
    assert_eq!(bar_number(12500.0, 2500.0, Some(3), true), "12.500,000");
    assert_eq!(bar_number(-1500.0, 500.0, Some(1), true), "-1.500,0");
    assert_eq!(bar_number(0.0, 5.0, Some(2), false), "0.00");
    assert_eq!(bar_number(-0.001, 5.0, Some(1), false), "0.0", "no sign on a zero");
    assert_eq!(bar_number(f64::NAN, 1.0, Some(2), false), "0.00", "never a panic");
    assert_eq!(bar_number(1250.0, 250.0, Some(9), false), "1,250.000", "at most 3");
}

#[test]
fn scale_bars_draw_subdivisions_and_place_labels_above_or_below() {
    let with = |f: &dyn Fn(&mut BarOptions<'static>)| {
        let mut b = BarOptions::default();
        f(&mut b);
        draw_bar(&b, 300)
    };
    let above = with(&|_| {});
    let rows = bar_ink_rows(&above);
    assert!(rows.iter().any(|&y| y < 32) && rows.iter().all(|&y| y < 42), "labels over the bar: {rows:?}");
    let below = with(&|b| b.label_position = ScaleLabelPosition::Below);
    let rows = bar_ink_rows(&below);
    assert!(rows.iter().all(|&y| y >= 18) && rows.iter().any(|&y| y > 28), "labels under it: {rows:?}");
    // Subdivisions aren't labelled: the first ink row stays where the labels put it.
    let cut = with(&|b| b.subdivisions = 3);
    assert_eq!(bar_ink_rows(&cut).first(), bar_ink_rows(&above).first());
    // A bigger label is taller; decimals change the labels.
    assert!(bar_ink_rows(&with(&|b| b.text_size = 16.0)).len() > rows.len() + 4);
    assert_ne!(with(&|b| b.decimals = Some(2)).data(), above.data());
    assert_ne!(with(&|b| b.divisions_before_zero = 1).data(), above.data());
}

#[test]
fn unit_labels_sit_after_the_bar_when_asked() {
    let draw = |text: Option<&'static str>, at| {
        draw_bar(&BarOptions { unit_label: text, unit_label_position: at, units: ScaleUnits::Kilometers, ..BarOptions::default() }, 300)
    };
    let right_of = |pm: &Pixmap| (0..pm.height()).filter_map(|y| (0..pm.width()).rev().find(|&x| pm.pixel(x, y).is_some_and(ink))).max();
    let none = draw(Some(""), UnitLabelPosition::AfterBar);
    let feet = draw(Some("Feet"), UnitLabelPosition::AfterBar);
    let (_, bare_end) = marked_span(&none, 37).unwrap();
    let (_, word_end) = marked_span(&feet, 37).unwrap();
    assert!(word_end > bare_end + 15, "level with the bar, after it: {word_end} against {bare_end}");
    // Left out, the abbreviation follows the last label; "" prints none.
    let default = draw(None, UnitLabelPosition::AfterLabels);
    let blank = draw(Some(""), UnitLabelPosition::AfterLabels);
    assert!(right_of(&default) > right_of(&blank));
    // Above or below, on a line of its own.
    let top = |pm: &Pixmap| bar_ink_rows(pm).first().copied().unwrap_or(0);
    let bottom = |pm: &Pixmap| bar_ink_rows(pm).last().copied().unwrap_or(0);
    assert!(top(&draw(Some("Feet"), UnitLabelPosition::Above)) < top(&blank));
    assert!(bottom(&draw(Some("Feet"), UnitLabelPosition::Below)) > bottom(&blank));
    assert_ne!(draw(Some("FEET"), UnitLabelPosition::AfterBar).data(), feet.data(), "verbatim");
    // Other scripts and emoji draw without a panic.
    for text in ["距離 km", "مسافة", "🚶"] {
        for at in UnitLabelPosition::ALL {
            let _ = draw(Some(text), at);
        }
    }
    // A label of spaces prints nothing and takes no room: after the bar or on a line of its own.
    assert_eq!(draw(Some("   "), UnitLabelPosition::AfterBar).data(), none.data());
    assert_eq!(draw(Some("   "), UnitLabelPosition::Above).data(), draw(Some(""), UnitLabelPosition::Above).data());
    // A 100-character unit stays inside a 93 px element wherever it is placed.
    let long: String = "Kilometres ".repeat(10).chars().take(100).collect();
    let narrow = |text: &str, at| {
        draw_bar(
            &BarOptions { unit_label: Some(text), unit_label_position: at, units: ScaleUnits::Kilometers, ..BarOptions::default() },
            93,
        )
    };
    let marked = |p: tiny_skia::PremultipliedColorU8| p.red() < 200 || p.green() < 200 || p.blue() < 200;
    for at in UnitLabelPosition::ALL {
        let pm = narrow(&long, at);
        let past: Vec<(u32, u32)> = (0..pm.height())
            .flat_map(|y| (92..pm.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| pm.pixel(x, y).is_some_and(marked))
            .collect();
        assert!(past.is_empty(), "{at:?}: ink past the element at {past:?}");
        assert_ne!(pm.data(), narrow("", at).data(), "{at:?}: the unit is drawn, shortened");
    }
    // After the labels, the unit is shortened and the last label's number kept whole: the ink
    // columns of the number drawn alone are all there, moved left with the label.
    let columns = |pm: &Pixmap| -> Vec<bool> { (0..pm.width()).map(|x| (0..33).any(|y| pm.pixel(x, y).is_some_and(ink))).collect() };
    let bare = columns(&narrow("", UnitLabelPosition::AfterLabels));
    let long_cols = columns(&narrow(&long, UnitLabelPosition::AfterLabels));
    let end = bare.iter().rposition(|&c| c).unwrap();
    // The last number: back from its right edge to a 3 px gap.
    let start = (0..end).rev().find(|&x| x >= 2 && !bare[x] && !bare[x - 1] && !bare[x - 2]).map_or(0, |x| x + 1);
    let number = &bare[start..=end];
    let found = (0..=start).any(|d| long_cols[start - d..=end - d] == *number);
    assert!(found, "the last number {start}..={end} is drawn whole: {long_cols:?}");
}

/// A long unit after the labels of a short bar, centred on its end, stays inside both edges of
/// the element, the last number drawn whole.
#[test]
fn a_unit_after_the_labels_of_a_short_bar_stays_inside_both_edges() {
    let long = "k".repeat(100);
    // 79 px to use at 250 m a point, 19.8 km: four 2 km divisions, a 32 px bar ending nearer
    // the left edge than the right.
    let draw = |unit: &str| {
        let bar =
            BarOptions { units: ScaleUnits::Kilometers, fit: ScaleBarFit::AdjustWidth, unit_label: Some(unit), ..BarOptions::default() };
        let mut pm = Pixmap::new(93, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(0.0, 0.0, 93.0, 60.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 250.0, &bar, DEFAULT_COLORS, 1.0, false));
        pm
    };
    let (pm, bare) = (draw(&long), draw(""));
    // The first label, "0", reaches the left edge as on every bar: the long unit adds nothing
    // there, nor anything at the right edge.
    let at = |pm: &Pixmap, x: u32| (0..pm.height()).map(|y| pm.pixel(x, y).is_some_and(ink)).collect::<Vec<_>>();
    assert_eq!(at(&pm, 0), at(&bare, 0), "nothing more at the left edge");
    assert!(at(&pm, 92).iter().all(|d| !d), "nothing at the right edge");
    // The last number: its ink columns (rows above the bar) drawn alone are all there.
    let columns = |pm: &Pixmap| -> Vec<bool> { (0..pm.width()).map(|x| (0..33).any(|y| pm.pixel(x, y).is_some_and(ink))).collect() };
    let (bare, long_cols) = (columns(&bare), columns(&pm));
    let end = bare.iter().rposition(|&c| c).unwrap();
    let start = (0..end).rev().find(|&x| x >= 2 && !bare[x] && !bare[x - 1] && !bare[x - 2]).map_or(0, |x| x + 1);
    let number = &bare[start..=end];
    let found = (0..=start).any(|d| long_cols[start - d..=end - d] == *number);
    assert!(found, "the last number {start}..={end} is drawn whole: {long_cols:?}");
}

#[test]
fn a_wide_unit_label_after_the_bar_keeps_the_bar() {
    let long = "Kilometres of ground distance, as measured on the map ".repeat(2);
    for width in [12u32, 40, 93, 300] {
        for height in [8u32, 60] {
            let bar = BarOptions {
                unit_label: Some(long.as_str()),
                unit_label_position: UnitLabelPosition::AfterBar,
                units: ScaleUnits::Kilometers,
                ..BarOptions::default()
            };
            let mut pm = Pixmap::new(width, height).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let r = Rect::from_xywh(0.0, 0.0, width as f32, height as f32).unwrap();
            // Filled in its colour throughout, so along its middle the bar is one solid run.
            let solid = (DEFAULT_COLORS.0, DEFAULT_COLORS.0);
            crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &bar, solid, 1.0, false));
            if width >= 40 && height == 60 {
                let marked = |x: u32| pm.pixel(x, 37).is_some_and(|p| p.red() < 200 || p.green() < 200 || p.blue() < 200);
                let a = (0..width).find(|&x| marked(x)).unwrap_or(width);
                // The bar ends where the 3 px gap before its unit begins; the unit follows.
                let gap = (a..width).find(|&x| (x..x + 3).all(|x| !marked(x))).unwrap_or(width);
                let end = gap.saturating_sub(1);
                let unit = (gap..width).find(|&x| marked(x));
                // Half the 85 % room is the bar's; its round length is at least 40 % of that.
                let room = width as f32 * 0.85 * 0.5;
                let (least, most) = ((room * 0.4) as u32 - 2, (room + 2.0) as u32);
                assert!(end >= a + least && end <= a + most, "width {width}: the bar keeps half its room, {a}..{end}");
                // Shortened to the room kept for it, the unit stays inside the bar's room and
                // the element; at 40 px it may not fit at all and is left out.
                if width > 40 {
                    assert!(unit.is_some_and(|x| x >= end + 3), "width {width}: the unit after the bar, from {unit:?}");
                }
                let inside = (BAR_INSET + width as f32 * 0.85 + 1.0).min(width as f32 - 1.0) as u32;
                let past = (inside..width).find(|&x| marked(x));
                assert!(past.is_none(), "width {width}: the unit stays in the bar's room, ink at {past:?}");
            }
        }
    }
}

#[test]
fn a_unit_label_after_the_bar_is_shortened_to_its_room() {
    let measure = |s: &str| s.chars().count() as f32 * 10.0;
    assert_eq!(shortened("Feet", 40.0, measure).as_deref(), Some("Feet"));
    assert_eq!(shortened("Kilometres", 40.0, measure).as_deref(), Some("Kil…"));
    assert_eq!(shortened("Kilo metres", 60.0, measure).as_deref(), Some("Kilo…"), "no space before the ellipsis");
    assert_eq!(shortened("Kilometres", 20.0, measure).as_deref(), Some("K…"));
    assert_eq!(shortened("Kilometres", 19.0, measure), None, "nothing when not even a letter fits");
    assert_eq!(shortened("", -1.0, measure), None);
    // A long unit takes a handful of measurements, not one a character.
    let calls = std::cell::Cell::new(0);
    let counted = |s: &str| {
        calls.set(calls.get() + 1);
        measure(s)
    };
    let long = "k".repeat(100);
    assert_eq!(shortened(&long, 105.0, counted).as_deref(), Some("kkkkkkkkk…"));
    assert!(calls.get() <= 9, "{} measurements", calls.get());
    // After the last label, only the unit is shortened, never the number.
    assert_eq!(last_label("25", "Kilometres", 130.0, measure), ("25 Kilometres".into(), None));
    assert_eq!(last_label("25", "Kilometres", 80.0, measure), ("25 Kilo…".into(), Some(UnitLabelCut::Shortened)));
    assert_eq!(
        last_label("25", "Kilometres", 40.0, measure),
        ("25".into(), Some(UnitLabelCut::LeftOut)),
        "the number alone when not a letter fits"
    );
    assert_eq!(last_label("1,250", "km", 10.0, measure), ("1,250".into(), Some(UnitLabelCut::LeftOut)), "the number is never cut");
}

/// A unit label shortened with an ellipsis, or left out with no room for it, is said, as a cut
/// text is, wherever it is placed; one drawn whole, or a plain bar's abbreviation, is not.
#[test]
fn a_shortened_or_left_out_unit_label_is_said() {
    let long = "Kilometres ".repeat(9);
    let cut = |w: f32, unit: Option<&str>, unit_label_position| {
        let bar = BarOptions { unit_label: unit, unit_label_position, units: ScaleUnits::Kilometers, ..BarOptions::default() };
        bar_layout(Rect::from_xywh(0.0, 0.0, w, 60.0).unwrap(), 100.0, &bar, 1.0, false).unwrap().unit_cut
    };
    for at in UnitLabelPosition::ALL {
        assert_eq!(cut(200.0, Some(&long), at), Some(UnitLabelCut::Shortened), "{at:?}");
        assert_eq!(cut(400.0, Some("Km"), at), None, "{at:?}: drawn whole");
        assert_eq!(cut(200.0, None, at), None, "{at:?}: the abbreviation");
    }
    // Not even a letter of it fits after the bar's last label in a box this narrow.
    assert_eq!(cut(20.0, Some(&long), UnitLabelPosition::AfterLabels), Some(UnitLabelCut::LeftOut));
    let loss = |name: &str, unit_label| TextLoss {
        id: 1,
        name: name.into(),
        left_out: 0,
        at_least: false,
        below_box: false,
        past_limit: false,
        off_page: false,
        unit_label,
    };
    let said = scale_bar_fit_notices(&[loss("A", Some(UnitLabelCut::Shortened))]);
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("Scale bar \"A\": its unit label is shortened to fit"), "{said:?}");
    let said = scale_bar_fit_notices(&[loss("A", Some(UnitLabelCut::LeftOut)), loss("B", Some(UnitLabelCut::LeftOut))]);
    assert!(said.len() == 1 && said[0].starts_with("2 scale bars (\"A\", \"B\"): their unit labels are left out"), "{said:?}");
    assert!(scale_bar_fit_notices(&[loss("A", None)]).is_empty());
}

/// A bar whose lines just fit its box, so none is written smaller, still draws them all inside it.
#[test]
fn a_scale_bar_rows_stay_inside_a_box_they_just_fit() {
    let bar = BarOptions {
        unit_label: Some("Kilometres"),
        unit_label_position: UnitLabelPosition::Above,
        label_position: ScaleLabelPosition::Above,
        text_size: 24.0,
        units: ScaleUnits::Kilometers,
        ..BarOptions::default()
    };
    // The 93 x 88 box in the middle of a taller page, so ink outside it would show.
    let mut pm = Pixmap::new(93, 128).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 20.0, 93.0, 88.0).unwrap();
    crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &bar, DEFAULT_COLORS, 1.0, false));
    let marked = |p: tiny_skia::PremultipliedColorU8| p.red() < 200 || p.green() < 200 || p.blue() < 200;
    let rows: Vec<u32> = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(marked))).collect();
    assert!(!rows.is_empty() && rows.iter().all(|&y| (20..108).contains(&y)), "every line inside the box: {rows:?}");
}

/// A bar in a box too short for its lines draws them at their size and is said, as a bar too
/// short for its labels at their text size is (#2581): its labels below and its unit above
/// count, where a plain bar's labels alone fit. A box tall enough draws them all inside it.
#[test]
fn a_scale_bar_unit_label_row_in_a_short_box_is_said() {
    let bar = BarOptions {
        unit_label: Some("Kilometres"),
        unit_label_position: UnitLabelPosition::Above,
        label_position: ScaleLabelPosition::Below,
        units: ScaleUnits::Kilometers,
        ..BarOptions::default()
    };
    let short = Rect::from_xywh(0.0, 20.0, 300.0, 20.0).unwrap();
    let placed = bar_layout(short, 100.0, &bar, 1.0, false).unwrap();
    assert!(placed.cramped, "too short for its lines");
    assert_eq!(placed.px, SCALE_BAR_TEXT_PT as f32 * 1.33, "drawn at its size, not smaller");
    assert!(!bar_layout(short, 100.0, &BarOptions::default(), 1.0, false).unwrap().cramped, "a plain bar fits");
    // In a box tall enough, every line inside it: the unit above and the labels under the bar.
    let mut pm = Pixmap::new(300, 80).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let r = Rect::from_xywh(0.0, 20.0, 300.0, 40.0).unwrap();
    assert!(!bar_layout(r, 100.0, &bar, 1.0, false).unwrap().cramped);
    crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &bar, DEFAULT_COLORS, 1.0, false));
    let marked = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|p| p.red() < 250 || p.green() < 250 || p.blue() < 250);
    let rows: Vec<u32> = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| marked(x, y))).collect();
    assert!(!rows.is_empty() && rows.iter().all(|y| (20..60).contains(y)), "inside the box: {rows:?}");
    // On a page, the short box is told as the text size's is: too small for its labels.
    let maps = vec![Map::new("m")];
    let mut l = Layout::new("Page", 120.0, 90.0);
    let frame = l.add(
        "Frame",
        (10.0, 30.0, 100.0, 50.0),
        serde_json::from_value(serde_json::json!({"MapFrame":{"map":"m", "center":{"x":0,"y":0}, "scale":1000000}})).unwrap(),
    );
    let id = l.add("Scale Bar", (10.0, 0.0, 100.0, 12.0), ElementKind::scale_bar(frame));
    let info = PageInfo::default();
    let set = |l: &mut Layout, size: f64, rows: bool| {
        if let Some(ElementKind::ScaleBar { text_size, label_position, unit_label_position, .. }) = l.element_mut(id).map(|e| &mut e.kind) {
            *text_size = size;
            (*label_position, *unit_label_position) = if rows {
                (ScaleLabelPosition::Below, UnitLabelPosition::Above)
            } else {
                (ScaleLabelPosition::Above, UnitLabelPosition::AfterLabels)
            };
        }
    };
    set(&mut l, 20.0, false);
    assert!(text_loss(&l, &maps, 96.0, &info, id).is_none(), "a plain bar's labels at 20 points fit");
    set(&mut l, 20.0, true);
    let loss = text_loss(&l, &maps, 96.0, &info, id).unwrap();
    assert!(loss.below_box && scale_bar_fit_notices(std::slice::from_ref(&loss))[0].contains("its box is too small"), "{loss:?}");
    set(&mut l, SCALE_BAR_TEXT_PT, true);
    assert!(text_loss(&l, &maps, 96.0, &info, id).is_none(), "its rows at 8 points fit");
}

/// A page draws its scale bar's details through `draw_page`: the unit label in the bar's colour.
#[test]
fn a_page_draws_a_scale_bar_with_its_details() {
    let (mut maps, mut l, frame) = colour_test_page();
    let mut bar = ElementKind::scale_bar(frame);
    if let ElementKind::ScaleBar { color, label_position, unit_label, unit_label_position, subdivisions, divisions_before_zero, .. } =
        &mut bar
    {
        *color = Rgba::rgb(0xD0, 0x10, 0x10);
        (*label_position, *unit_label, *unit_label_position) =
            (ScaleLabelPosition::Below, Some("Kilometres".into()), UnitLabelPosition::Above);
        (*subdivisions, *divisions_before_zero) = (4, 1);
    }
    l.add("Scale Bar", (5.0, 5.0, 90.0, 20.0), bar.clone());
    let pm = render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap();
    let rows = |from: u32, to: u32, hit: fn(tiny_skia::PremultipliedColorU8) -> bool| {
        (from..to).flat_map(|y| (0..pm.width()).map(move |x| (x, y))).filter(|&(x, y)| pm.pixel(x, y).is_some_and(hit)).count()
    };
    // The strip of the bar's 20 mm box over the bar (its middle at 15 mm, its top 4 px above)
    // holds the unit label, in the bar's colour.
    let mm = pm.height() as f32 / 60.0;
    assert!(rows((5.0 * mm) as u32, (13.5 * mm) as u32, red) > 30, "the unit label over the bar, crimson");
    assert_eq!(rows(0, pm.height() / 2, ink), 0, "no default ink");
    // The same bar without its details draws differently on the same page.
    let (mut maps2, mut plain, frame2) = colour_test_page();
    let mut same = ElementKind::scale_bar(frame2);
    if let ElementKind::ScaleBar { color, .. } = &mut same {
        *color = Rgba::rgb(0xD0, 0x10, 0x10);
    }
    plain.add("Scale Bar", (5.0, 5.0, 90.0, 20.0), same);
    assert_ne!(pm.data(), render(&plain, &mut maps2, 96.0, &PageInfo::default()).unwrap().data());
}

#[test]
fn scale_numbers_are_short() {
    assert_eq!(scale_number(5.0, 1.25), "5");
    assert_eq!(scale_number(2.5, 1.25), "2.5");
    assert_eq!(scale_number(1.25, 1.25), "1.25");
    assert_eq!(scale_number(0.0, 1.25), "0");
    assert_eq!(scale_number(100.0, 50.0), "100");
    // Small totals keep their precision: 0.05 km in four divisions.
    assert_eq!(scale_number(0.0125, 0.0125), "0.0125");
    assert_eq!(scale_number(0.0375, 0.0125), "0.0375");
    assert_eq!(scale_number(0.125, 0.125), "0.125");
    // A step that never comes out even keeps two significant digits of it.
    assert_eq!(scale_number(1.0 / 3.0, 1.0 / 3.0), "0.33");
    assert_eq!(scale_number(2.0 / 3.0, 1.0 / 3.0), "0.67");
    assert_eq!(scale_number(1.0, 1.0 / 3.0), "1");
    assert_eq!(scale_number(20.0 / 3.0, 10.0 / 3.0), "6.7");
    assert_eq!(scale_number(200.0 / 3.0, 100.0 / 3.0), "67");
    assert_eq!(scale_number(0.01 / 3.0, 0.01 / 3.0), "0.0033");
}

/// Graticule labels: meridians along the top and bottom and parallels along the sides come
/// first; a slanting meridian leaving through a side is labelled there only clear of the
/// parallels' labels, and none lands by a corner.
#[test]
fn graticule_labels_keep_clear_of_each_other() {
    let along = |_: Edge, s: &str| s.len() as f32 * 5.0;
    let label = |edge, at: f32, s: &str, lon| (edge, at, s.to_string(), lon);
    let labels = vec![
        // A meridian leaving through the right edge just by a parallel's label (put first, so
        // only the order fixes it), and another well clear of any.
        label(Edge::Right, 205.0, "36°W", true),
        label(Edge::Right, 200.0, "6°S", false),
        label(Edge::Right, 400.0, "34°W", true),
        label(Edge::Top, 150.0, "42°W", true),
        // A parallel curving out through the top near a meridian's label: clear of it, but
        // too close to read as its own.
        label(Edge::Top, 185.0, "2°S", false),
        // By a corner: left out, and a side's label within two label heights of the top.
        label(Edge::Top, 4.0, "44°W", true),
        label(Edge::Right, 15.0, "2°S", false),
    ];
    let kept: Vec<String> =
        kept_labels(labels.clone(), (600.0, 500.0), (along, |_: Edge, _: &str| 20.0), 10.0, 1.0).into_iter().map(|(.., s)| s).collect();
    assert_eq!(kept, ["6°S", "42°W", "34°W"]);
    // Labels reaching further in than a short frame is across are left out, not cut by it:
    // on a frame 30 pixels tall, the top's; the sides', 600 wide, keep theirs.
    let kept: Vec<String> = kept_labels(labels, (600.0, 30.0), (along, |_: Edge, _: &str| 40.0), 10.0, 1.0)
        .into_iter()
        .filter(|(edge, ..)| *edge == Edge::Top)
        .map(|(.., s)| s)
        .collect();
    assert!(kept.is_empty(), "{kept:?}");
    // And on a frame 15 pixels wide, the sides' labels, whose flat labels reach their width in.
    let across = |edge: Edge, s: &str| if matches!(edge, Edge::Left | Edge::Right) { s.len() as f32 * 5.0 + 6.0 } else { 6.0 };
    let side = vec![label(Edge::Right, 200.0, "6°S", false), label(Edge::Left, 250.0, "8°S", false)];
    assert_eq!(kept_labels(side.clone(), (600.0, 500.0), (along, across), 10.0, 1.0).len(), 2, "a wide frame keeps them");
    assert!(kept_labels(side, (15.0, 500.0), (along, across), 10.0, 1.0).is_empty(), "a narrow one leaves them out");
}

/// A text's layer statistics (#2853) fill in from the frame's map, written as a chart's values
/// with the layout's decimal mark, beside `{scale}`; a token with no figure, or with no frame,
/// is drawn as written. What each such token is missing is said, once.
#[test]
fn a_text_shows_its_layers_statistics() {
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    let mut ds = Dataset::new("Plants", GeometryType::None, vec![Field::new("mw", FieldType::Double), Field::new("name", FieldType::Text)]);
    for mw in [12_000.5, 11_553.25] {
        ds.push(None, vec![Value::Float(mw), Value::Text("x".into())]);
    }
    let mut map = Map::new("m");
    map.add_dataset(ds);
    let cam = frame_camera(&map, Coord::new(0.0, 0.0), 6_300_000.0, 0.0, 0.0);
    let info = PageInfo { page: 1, pages: 1, page_name: String::new() };
    let mut layout = Layout::new("L", 100.0, 100.0);
    let text = "{count:Plants} plants, {sum:Plants:mw} MW, mean {mean:Plants:mw}; {sum:Plants:name} {count:Nowhere}";
    assert_eq!(
        expand(text, &layout, &info, Some((&map, cam))),
        "2 plants, 23,553.75 MW, mean 11,776.88; {sum:Plants:name} {count:Nowhere}"
    );
    layout.decimal_comma = true;
    assert_eq!(
        expand(text, &layout, &info, Some((&map, cam))),
        "2 plants, 23.553,75 MW, mean 11.776,88; {sum:Plants:name} {count:Nowhere}"
    );
    // Without a frame, every token as written, and said so; a text without tokens says nothing.
    assert_eq!(expand("{count:Plants} plants", &layout, &info, None), "{count:Plants} plants");
    let maps = [map];
    assert_eq!(
        statistic_notices(&layout, &maps, "{count:Plants} and {count:Plants}"),
        vec!["{count:Plants} shows as written: the layout has no map frame whose map it would read".to_string()]
    );
    assert_eq!(statistic_notices(&layout, &maps, "Plants at {scale}"), Vec::<String>::new());
    let kind = serde_json::from_value(serde_json::json!({"MapFrame": {"map": "m", "center": {"x": 0, "y": 0}, "scale": 6300000}})).unwrap();
    layout.add("Frame", (0.0, 0.0, 50.0, 50.0), kind);
    let said = statistic_notices(&layout, &maps, "{sum:Plants:name} {sum:Plants:name} {count:Nowhere} {min:Plants:nosuch} {count:Plants}");
    assert_eq!(
        said,
        vec![
            "{sum:Plants:name} shows as written: 'name' of 'Plants' holds no numbers".to_string(),
            "{count:Nowhere} shows as written: the map of the layout's first frame has no layer named 'Nowhere'".to_string(),
            "{min:Plants:nosuch} shows as written: 'Plants' has no field named 'nosuch'".to_string(),
        ]
    );
}

/// A layer read in place holds only the features loaded, so a figure over it is partial: said
/// when the text is set (#2853), as is a layer that holds everything not.
#[test]
fn a_streamed_layers_statistic_says_it_covers_what_is_loaded() {
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    let layer = |name: &str| {
        let fields = vec![Field::new("mw", FieldType::Double), Field::new("blank", FieldType::Double)];
        let mut ds = Dataset::new(name, GeometryType::None, fields);
        ds.push(None, vec![Value::Float(1.0), Value::Null]);
        ds
    };
    let mut map = Map::new("m");
    map.add_dataset(layer("Held"));
    map.add_dataset(layer("Streamed"));
    map.layers.iter_mut().find(|l| l.name == "Streamed").unwrap().source = Some(Default::default());
    let mut layout = Layout::new("L", 100.0, 100.0);
    let kind = serde_json::from_value(serde_json::json!({"MapFrame": {"map": "m", "center": {"x": 0, "y": 0}, "scale": 6300000}})).unwrap();
    layout.add("Frame", (0.0, 0.0, 50.0, 50.0), kind);
    let maps = [map];
    assert_eq!(
        statistic_notices(&layout, &maps, "{count:Held} {count:Streamed} {count:Streamed} {sum:Streamed:mw} {sum:Streamed:no} {max:Streamed:blank}"),
        vec![
            "{count:Streamed} covers only the features of 'Streamed' that are loaded: it is read in place".to_string(),
            "{sum:Streamed:mw} covers only the features of 'Streamed' that are loaded: it is read in place".to_string(),
            "{sum:Streamed:no} shows as written: 'Streamed' has no field named 'no'".to_string(),
            // No number loaded yet, but a later load may bring some: the scope is said as well.
            "{max:Streamed:blank} covers only the features of 'Streamed' that are loaded: it is read in place".to_string(),
            "{max:Streamed:blank} shows as written: no feature of 'Streamed' its definition query keeps has a number in 'blank', or the figure is past a number's range".to_string(),
        ]
    );
}

/// A raster layer has no features, so a figure over it has none to give: drawn as written and
/// said (#2853), never a 0 that reads like a count.
#[test]
fn a_raster_layers_statistic_is_drawn_as_written() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
    let r = Raster::new("e", 1, 1, vec![vec![1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    let mut map = Map::new("m");
    map.add_raster("Elevation", rustgis_map::RasterLayer::from_path(r, "e.tif"));
    let cam = frame_camera(&map, Coord::new(0.0, 0.0), 6_300_000.0, 0.0, 0.0);
    let info = PageInfo { page: 1, pages: 1, page_name: String::new() };
    let mut layout = Layout::new("L", 100.0, 100.0);
    assert_eq!(expand("{count:Elevation}", &layout, &info, Some((&map, cam))), "{count:Elevation}");
    let kind = serde_json::from_value(serde_json::json!({"MapFrame": {"map": "m", "center": {"x": 0, "y": 0}, "scale": 6300000}})).unwrap();
    layout.add("Frame", (0.0, 0.0, 50.0, 50.0), kind);
    let maps = [map];
    assert_eq!(
        statistic_notices(&layout, &maps, "{count:Elevation}"),
        vec!["{count:Elevation} shows as written: 'Elevation' is a raster layer, which has no features".to_string()]
    );
    // The same figures over a map given directly, as a presentation map page's title is.
    assert_eq!(
        map_statistic_notices(&maps[0], "the page's map", "{count:Elevation}"),
        vec!["{count:Elevation} shows as written: 'Elevation' is a raster layer, which has no features".to_string()]
    );
}

/// A frame whose map the project doesn't hold is told apart from a layout with no frame (#2853).
#[test]
fn a_statistic_names_a_frame_whose_map_is_missing() {
    let mut layout = Layout::new("L", 100.0, 100.0);
    let said = |layout: &Layout| statistic_notices(layout, &[Map::new("m")], "{count:Cities}");
    assert_eq!(said(&layout), vec!["{count:Cities} shows as written: the layout has no map frame whose map it would read".to_string()]);
    let kind =
        serde_json::from_value(serde_json::json!({"MapFrame": {"map": "gone", "center": {"x": 0, "y": 0}, "scale": 6300000}})).unwrap();
    layout.add("Frame", (0.0, 0.0, 50.0, 50.0), kind);
    assert_eq!(
        said(&layout),
        vec!["{count:Cities} shows as written: the map of the layout's first frame is not in the project".to_string()]
    );
}

/// A figure written without its field (`{sum:Plants}`) is drawn as written, which is no token for
/// the page, and is said (#2853), as one with a field the layer lacks is.
#[test]
fn a_statistic_without_its_field_is_said() {
    use rustgis_data::{Dataset, Field, FieldType, GeometryType};
    let mut map = Map::new("m");
    map.add_dataset(Dataset::new("Plants", GeometryType::None, vec![Field::new("mw", FieldType::Double)]));
    let mut layout = Layout::new("L", 100.0, 100.0);
    let kind = serde_json::from_value(serde_json::json!({"MapFrame": {"map": "m", "center": {"x": 0, "y": 0}, "scale": 6300000}})).unwrap();
    layout.add("Frame", (0.0, 0.0, 50.0, 50.0), kind);
    let maps = [map];
    assert_eq!(
        statistic_notices(&layout, &maps, "{sum:Plants} {count:Plants} {max:Nowhere} {mean:Plants:mw}"),
        vec![
            "{sum:Plants} shows as written: a sum takes a field: write {sum:Plants:field}".to_string(),
            "{max:Nowhere} shows as written: the map of the layout's first frame has no layer named 'Nowhere'".to_string(),
            "{mean:Plants:mw} shows as written: no feature of 'Plants' its definition query keeps has a number in 'mw', or the figure is past a number's range".to_string(),
        ]
    );
}

/// A layout with a decimal comma swaps the marks of the numbers it prints, `{scale}` included,
/// and leaves other text alone.
#[test]
fn a_decimal_comma_swaps_the_marks() {
    assert_eq!(decimal("0.05".into(), true), "0,05");
    assert_eq!(decimal("6,300,000".into(), true), "6.300.000");
    assert_eq!(decimal("0.05".into(), false), "0.05");
    let map = Map::new("m");
    let cam = frame_camera(&map, Coord::new(0.0, 0.0), 6_300_000.0, 0.0, 0.0);
    let info = PageInfo { page: 1, pages: 1, page_name: String::new() };
    let mut layout = Layout::new("L", 100.0, 100.0);
    let point = expand("Scale {scale}, page {page}.", &layout, &info, Some((&map, cam)));
    layout.decimal_comma = true;
    let comma = expand("Scale {scale}, page {page}.", &layout, &info, Some((&map, cam)));
    // "Scale 1:6,300,000, page 1." and "Scale 1:6.300.000, page 1.": the scale's marks swap, the
    // text's comma stays.
    let scale = point.strip_prefix("Scale ").and_then(|t| t.strip_suffix(", page 1.")).unwrap_or_else(|| panic!("{point}"));
    assert!(scale.starts_with("1:") && scale.contains(','), "{point}");
    assert_eq!(comma, format!("Scale {}, page 1.", scale.replace(',', ".")));
}

/// A map of one point layer, one point, set by `edit`.
fn point_legend_map(edit: impl FnOnce(&mut rustgis_map::Layer)) -> Map {
    use rustgis_data::{Dataset, Geometry};
    let mut map = Map::new("m");
    let mut d = Dataset::new("pts", GeometryType::Point, vec![]);
    d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    map.add_dataset(d);
    if let Some(l) = map.layers.last_mut() {
        edit(l);
    }
    map
}

/// The legend of `map` at 1:1e6, every class, on white.
fn point_legend(map: &Map) -> Pixmap {
    let mut pm = Pixmap::new(200, 200).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 200.0, 200.0).unwrap(), map, ("", 1), None, opts, 1.0));
    pm
}

/// The legend of `map` at 1:1e6 recorded for a vector export, on a page of `side` pixels, drawn
/// back on white.
fn recorded_legend(map: &Map, side: u32) -> Pixmap {
    let mut c = Canvas::recording(side, side).unwrap();
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    legend(&mut c, Rect::from_xywh(0.0, 0.0, side as f32, side as f32).unwrap(), map, ("", 1), None, opts, 1.0);
    let (_, _, items) = c.into_drawing().unwrap().finish();
    let mut pm = Pixmap::new(side, side).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    crate::vector::replay(&items, &mut pm, Transform::identity());
    pm
}

/// What the map's code draws for a point at (`x`, `y`) on white.
fn drawn_point(s: &rustgis_map::Symbol, strokes: &[rustgis_map::LineStroke], (x, y): (f32, f32), ppp: f32) -> Pixmap {
    let mut pm = Pixmap::new(200, 200).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    crate::canvas::with_canvas(&mut pm, |c| {
        if strokes.is_empty() {
            crate::draw_marker(c, x, y, s, 1.0, None, ppp);
        } else {
            point_marks::draw(c, x, y, s, strokes, 0.0, None, ppp);
        }
    });
    pm
}

/// What the map's code draws for a point at (`x`, `y`), composited as one at `opacity`, on
/// white.
fn drawn_see_through(s: &rustgis_map::Symbol, strokes: &[rustgis_map::LineStroke], (x, y): (f32, f32), ppp: f32, opacity: f32) -> Pixmap {
    let mut own = Pixmap::new(200, 200).unwrap();
    crate::canvas::with_canvas(&mut own, |c| draw_point(c, (x, y), s, strokes, ppp));
    let mut pm = Pixmap::new(200, 200).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    pm.draw_pixmap(0, 0, own.as_ref(), &PixmapPaint { opacity, ..PixmapPaint::default() }, Transform::identity(), None);
    pm
}

/// Whether two pictures differ by no more than a rounding over `w` × `h` from the top left
/// corner.
fn near_corner(a: &Pixmap, b: &Pixmap, w: u32, h: u32) -> bool {
    (0..h).all(|y| {
        (0..w).all(|x| match (a.pixel(x, y), b.pixel(x, y)) {
            (Some(p), Some(q)) => p.red().abs_diff(q.red()).max(p.green().abs_diff(q.green())).max(p.blue().abs_diff(q.blue())) <= 2,
            _ => false,
        })
    })
}

/// Whether two pictures are the same over `w` × `h` from the top left corner.
fn same_corner(a: &Pixmap, b: &Pixmap, w: u32, h: u32) -> bool {
    (0..h).all(|y| (0..w).all(|x| a.pixel(x, y) == b.pixel(x, y)))
}

/// A stacked stroke: a square of `size` points in `color`, with no outline.
fn square_mark(color: Rgba, size: f32) -> rustgis_map::LineStroke {
    let markers = rustgis_map::LineMarkers {
        mark: rustgis_map::LineMark::Marker(rustgis_map::Marker::Square),
        size,
        ..rustgis_map::LineMarkers::default()
    };
    rustgis_map::LineStroke { color: Some(color), width: Some(0.0), markers: Some(markers), ..rustgis_map::LineStroke::default() }
}

/// Every marker, tiny and ordinary, with an outline and without, at the pixels the map's code
/// gives it.
#[test]
fn a_legend_draws_a_point_as_the_map_does() {
    let red = Rgba::rgb(255, 0, 0);
    let base = rustgis_map::Symbol { fill: red, stroke: Rgba::rgb(0, 0, 255), ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    // Every shape, tiny and ordinary, with no outline and with one.
    for marker in rustgis_map::Marker::ALL {
        for (size, stroke_width) in [(2.0, 0.0), (10.0, 0.0), (10.0, 1.0)] {
            let s = rustgis_map::Symbol { marker, size, stroke_width, ..base };
            let map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s));
            // The first row, 15 points tall, its swatch 16 wide.
            let want = drawn_point(&s, &[], (8.0, 7.5), 1.0);
            assert!(same_corner(&point_legend(&map), &want, 16, 15), "{marker:?} of {size} pt, outline {stroke_width} pt");
        }
    }
    // A class's symbol too, indented under its layer's name.
    let s = rustgis_map::Symbol { marker: rustgis_map::Marker::Triangle, size: 10.0, stroke_width: 0.0, ..base };
    let map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".to_string(), s)],
            other: s,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    });
    let got = point_legend(&map);
    let want = drawn_point(&s, &[], (6.0 + 8.0, 15.0 + 7.5), 1.0);
    assert!((15..30).all(|y| (6..22).all(|x| got.pixel(x, y) == want.pixel(x, y))), "the class's triangle");
}

/// An outline under half a pixel is as thin in a legend as on the map.
#[test]
fn a_legend_draws_a_hairline_outline_as_the_map_does() {
    // Thinner than half a pixel: the map's marker strokes it at its width, with no minimum.
    let s = rustgis_map::Symbol {
        size: 10.0,
        stroke: Rgba::rgb(0, 0, 255),
        stroke_width: 0.1,
        ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
    };
    let map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s));
    assert!(same_corner(&point_legend(&map), &drawn_point(&s, &[], (8.0, 7.5), 1.0), 16, 15));
}

/// A layer's stacked marks replace its marker, in a row that holds them.
#[test]
fn a_legend_draws_a_points_stacked_marks() {
    let strokes = vec![square_mark(Rgba::rgb(255, 0, 0), 12.0), square_mark(Rgba::rgb(0, 0, 0), 4.0)];
    let s = rustgis_map::Symbol::default_for(GeometryType::Point, 0);
    let map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.line_strokes = strokes.clone();
    });
    // 12 pt squares reach 7.2 pt: the row is 15 points tall still.
    assert!(same_corner(&point_legend(&map), &drawn_point(&s, &strokes, (8.0, 7.5), 1.0), 16, 15));
}

/// Stacked marks at the opacity of a layer and its groups, on pixels and in a recording.
#[test]
fn a_see_through_point_is_composited_as_one_with_its_groups() {
    let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    let strokes = vec![square_mark(Rgba::rgb(255, 0, 0), 12.0), square_mark(Rgba::rgb(0, 0, 0), 12.0)];
    let mut map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.line_strokes = strokes.clone();
        l.transparency = 50.0;
    });
    let middle = |map: &Map| point_legend(map).pixel(8, 7).map(|p| (p.red(), p.green(), p.blue())).unwrap();
    // Black at half over white: grey, with none of the red under it showing.
    let (r, g, b) = middle(&map);
    assert!((120..=135).contains(&r) && r == g && g == b, "{r} {g} {b}");
    // Its group's transparency applies too: half of half.
    let id = map.layers[0].id;
    let mut group = rustgis_map::LayerGroup::new("g");
    group.transparency = 50.0;
    group.children.push(id);
    map.add_group(group);
    let (r, g, b) = middle(&map);
    assert!((185..=197).contains(&r) && r == g && g == b, "{r} {g} {b}");
    // Recorded for a vector export, it is half of half too.
    let p = recorded_legend(&map, 200).pixel(8, 7).unwrap();
    let (r, g, b) = (p.red(), p.green(), p.blue());
    assert!((185..=197).contains(&r) && r == g && g == b, "recorded: {r} {g} {b}");
}

/// A square turned by its own angle reaches past 0.6 of its size: its row holds its corners,
/// which the picture of a see-through symbol would cut.
#[test]
fn a_see_through_mark_with_its_own_angle_keeps_its_corners() {
    let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    let mut mark = square_mark(Rgba::rgb(0, 0, 255), 40.0);
    if let Some(m) = &mut mark.markers {
        m.angle = 45.0;
    }
    let strokes = vec![mark];
    let map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.line_strokes = strokes.clone();
        l.transparency = 50.0;
    });
    let side = 2.0 * point_marks::reach(&s, &strokes);
    assert!(side > 50.0, "{side}");
    let drawn = drawn_see_through(&s, &strokes, (side / 2.0, side / 2.0), 1.0, 0.5);
    assert!(near_corner(&point_legend(&map), &drawn, side.ceil() as u32, side.ceil() as u32));
}

/// A plain point at half its opacity, its outline over its fill: composited as one, so the fill
/// doesn't show through the outline.
#[test]
fn a_see_through_plain_point_is_composited_as_the_map_composites_a_layer() {
    let s = rustgis_map::Symbol {
        fill: Rgba::rgb(255, 0, 0),
        stroke: Rgba::rgb(0, 0, 255),
        stroke_width: 1.0,
        size: 8.0,
        marker: rustgis_map::Marker::Square,
    };
    let map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.transparency = 50.0;
    });
    assert!(near_corner(&point_legend(&map), &drawn_see_through(&s, &[], (8.0, 7.5), 1.0, 0.5), 16, 15));
}

/// A large symbol takes a taller row and a wider column in a legend's size; an ordinary one
/// doesn't.
#[test]
fn a_legend_grows_to_hold_a_large_point() {
    let mm = 25.4 / 96.0;
    let s = |size| rustgis_map::Symbol { size, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    let label = f64::from(6.0 + text::measure("pts", 9.0, false));
    // An ordinary symbol keeps the row and the swatch's column.
    let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(10.0)));
    let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, false, (&[], &NO_LABELS)).unwrap();
    assert!((h - f64::from(LEGEND_ROW) * mm).abs() < 1e-6, "{h} mm");
    // Its width within a rounding: it is summed in f32, past a name as wide as its font makes it.
    assert!((w - (f64::from(LEGEND_SWATCH) + label) * mm).abs() < 1e-4, "{w} mm");
    // One of 40 pt reaches 24 pt from its point: 48 pt each way.
    let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(40.0)));
    let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, false, (&[], &NO_LABELS)).unwrap();
    assert!((h - 48.0 * mm).abs() < 1e-6, "{h} mm");
    assert!((w - (48.0 + label) * mm).abs() < 1e-4, "{w} mm");
    assert!(same_corner(&point_legend(&map), &drawn_point(&s(40.0), &[], (24.0, 24.0), 1.0), 48, 48));
    // A frame too short for the row draws no part of it.
    let mut pm = Pixmap::new(200, 200).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let blank = pm.clone();
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 200.0, 47.0).unwrap(), &map, ("", 1), None, opts, 1.0));
    assert!(pm.data() == blank.data());
    // A size no page could hold stays a size a legend can take.
    let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(f32::MAX)));
    let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, false, (&[], &NO_LABELS)).unwrap();
    assert!(w.is_finite() && h.is_finite(), "{w} × {h} mm");
}

/// A legend's text size (#2581) scales its text, rows, swatches and gaps, and its title's room,
/// from 9 points; a point symbol keeps the size the map draws it at, its row growing only as
/// far as it needs.
#[test]
fn a_legends_text_size_scales_its_rows_but_not_its_point_symbols() {
    let mm = 25.4 / 96.0;
    let s = |size| rustgis_map::Symbol { size, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    let sized = |map: &mut Map, pt: f64| legend_sizes_with_no_data_mm(map, 1e6, true, false, (false, pt), (&[], &NO_LABELS)).unwrap()[0];
    // An ordinary symbol at 18 points: the row, the swatch's column, the gap and the name twice
    // their size at 9.
    let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(10.0)));
    let (w, h) = sized(&mut map, 18.0);
    assert!((h - f64::from(2.0 * LEGEND_ROW) * mm).abs() < 1e-6, "{h} mm");
    let label = f64::from(12.0 + text::measure("pts", 18.0, false));
    assert!((w - (f64::from(2.0 * LEGEND_SWATCH) + label) * mm).abs() < 1e-4, "{w} mm");
    assert_eq!(
        sized(&mut map, rustgis_map::layout::LEGEND_TEXT_PT),
        legend_sizes_mm(&mut map, 1e6, true, false, (&[], &NO_LABELS)).unwrap()[0]
    );
    // One of 40 points keeps its 48: the row (30 at 18 points) and the column grow to it.
    let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(40.0)));
    let (w, h) = sized(&mut map, 18.0);
    assert!((h - 48.0 * mm).abs() < 1e-6, "{h} mm");
    assert!((w - (48.0 + label) * mm).abs() < 1e-4, "{w} mm");
    let mut pm = Pixmap::new(200, 200).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 2.0 };
    crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 200.0, 200.0).unwrap(), &map, ("", 1), None, opts, 1.0));
    assert!(same_corner(&pm, &drawn_point(&s(40.0), &[], (24.0, 24.0), 1.0), 48, 48), "the symbol as the map draws it");
    // A polygon's swatch is twice as wide, as its column is.
    let red = Rgba::rgb(255, 0, 0);
    let fill = rustgis_map::Symbol { fill: red, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0) };
    let map = shape_legend_map(GeometryType::Polygon, |l| l.renderer = rustgis_map::Renderer::Single(fill));
    let across = |size: f32| {
        let mut pm = Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { size, ..opts };
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 200.0, 200.0).unwrap(), &map, ("", 1), None, opts, 1.0)
        });
        (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green()) == (255, 0)))).count()
    };
    // Its fill inside its outline's edge pixels: 16 points more across, at twice the size.
    let (one, two) = (across(1.0), across(2.0));
    assert_eq!(two - one, 16, "{one} and {two} px");
    // A line wider than its row, at a small size, is cut to its row (#2518): nothing above the
    // box.
    let small = LegendOptions { size: 4.0 / 9.0, ..opts };
    let drawn = |map: &Map, at: (f32, f32), opts: LegendOptions| {
        let mut pm = Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(at.0, at.1, 200.0 - at.0, 200.0 - at.1).unwrap(), map, ("", 1), None, opts, 1.0)
        });
        pm
    };
    // The first row and column `colour` takes.
    let ink = |pm: &Pixmap, (r, g, b): (u8, u8, u8)| {
        let at = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (r, g, b));
        ((0..pm.height()).find(|&y| (0..pm.width()).any(|x| at(x, y))), (0..pm.width()).find(|&x| (0..pm.height()).any(|y| at(x, y))))
    };
    let wide = rustgis_map::Symbol { stroke: red, stroke_width: 20.0, ..rustgis_map::Symbol::default_for(GeometryType::Polyline, 0) };
    let map = shape_legend_map(GeometryType::Polyline, |l| l.renderer = rustgis_map::Renderer::Single(wide));
    let (top, _) = ink(&drawn(&map, (0.0, 50.0), small), (255, 0, 0));
    assert!(top.is_some_and(|y| y >= 50), "drawn, nothing above the legend's box: {top:?}");
    // A thin one stays within its row and keeps the plain swatch it always had.
    let thin = rustgis_map::Symbol { stroke_width: 1.0, ..wide };
    let row = (0.0, 0.0, LEGEND_SWATCH, LEGEND_ROW);
    let b = swatch_ink(row, GeometryType::Polyline, (&thin, &[], &[]), (LEGEND_SWATCH, 1.0)).unwrap();
    assert!(b.top() >= 0.0 && b.bottom() <= LEGEND_ROW, "{b:?}");
    // A polygon's wide outline is cut at its row's top and bottom, plain or stacked, and on the
    // left at the legend's edge, where nothing lies before its column.
    let blue = Rgba::rgb(0, 0, 255);
    let outlined =
        rustgis_map::Symbol { fill: red, stroke: blue, stroke_width: 12.0, ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0) };
    let solid = rustgis_map::fills::FillLayer { color: Some(red), pattern: rustgis_map::fills::FillPattern::Solid };
    // (The stacked one at the default size: a stack is always composited, and cut, as #2518 set.)
    for (fills, opts) in [(vec![], small), (vec![solid], LegendOptions { size: 1.0, ..opts })] {
        let stacked = !fills.is_empty();
        let map = shape_legend_map(GeometryType::Polygon, |l| {
            l.renderer = rustgis_map::Renderer::Single(outlined);
            l.fill_layers = fills;
        });
        let (top, left) = ink(&drawn(&map, (50.0, 50.0), opts), (0, 0, 255));
        assert!(top.is_some_and(|y| y >= 50), "stacked {stacked}: nothing above the legend's box: {top:?}");
        assert_eq!(left, Some(50), "stacked {stacked}: drawn to the legend's edge, not past it");
    }
    // Left of a column that has room before it (a class's indent, the gap between columns as
    // drawn), the outline reaches into that room and no further.
    let map = shape_legend_map(GeometryType::Polygon, |l| l.renderer = rustgis_map::Renderer::Single(outlined));
    for lead in [6.0f32, 2.0] {
        let mut pm = Pixmap::new(100, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            swatch(
                c,
                (50.0, 20.0, LEGEND_SWATCH, LEGEND_ROW),
                &map.layers[0],
                (&outlined, &[], &[]),
                1.0,
                1e6,
                (1.0, LEGEND_SWATCH, (lead, 6.0)),
            )
        });
        let (_, left) = ink(&pm, (0, 0, 255));
        assert_eq!(left, Some(50 - lead as u32), "room {lead}: its outer half, 6 points, as far as its room");
    }
    // The title's room follows; a size past the range, or none at all, is brought into it.
    assert!((legend_title_mm(18.0) - 2.0 * legend_title_mm(9.0)).abs() < 1e-12);
    assert_eq!(legend_size(1e9), 100.0 / 9.0);
    assert_eq!((legend_size(f64::NAN), legend_size(9.0)), (1.0, 1.0));
}

/// A layer's reference scale sizes its symbol at the frame's scale.
#[test]
fn a_legend_scales_a_point_with_its_reference_scale() {
    let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    // Twice as large at 1:1e6: 10 pt reaches 12 pt from its point, in a row of 24.
    let map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.reference_scale = Some(2e6);
    });
    assert!(same_corner(&point_legend(&map), &drawn_point(&s, &[], (12.0, 12.0), 2.0), 24, 24));
}

/// A diagram's categories on a point layer with stacked marks: each row shows its own colour,
/// not the layer's marks.
#[test]
fn a_diagrams_rows_keep_their_own_mark_on_a_layer_with_stacked_marks() {
    let (red, green) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 255, 0));
    let category = |field: &str, colour| rustgis_map::DiagramCategory { field: field.into(), colour };
    let map = point_legend_map(|l| {
        l.line_strokes = vec![square_mark(Rgba::rgb(0, 0, 0), 12.0)];
        l.diagram = Some(rustgis_map::DiagramSettings {
            kind: rustgis_map::DiagramKind::Pie,
            categories: vec![category("a", red), category("b", green)],
            size: rustgis_map::DiagramSize::Fixed(12.0),
            start_angle: 0.0,
            background: None,
            stroke: rustgis_map::DiagramSettings::default_stroke(),
            stroke_width: 0.75,
            labels: rustgis_map::DiagramLabel::None,
        });
    });
    let pm = point_legend(&map);
    let at = |x, y| pm.pixel(x, y).map(|p| (p.red(), p.green(), p.blue()));
    // The layer's name, then a row each, the swatch 6 points in.
    assert_eq!(at(14, 22), Some((255, 0, 0)));
    assert_eq!(at(14, 37), Some((0, 255, 0)));
}

/// A one-feature line or polygon layer, edited by `edit`.
fn shape_legend_map(gt: GeometryType, edit: impl FnOnce(&mut rustgis_map::Layer)) -> Map {
    use rustgis_data::{Dataset, Geometry};
    let mut map = Map::new("m");
    let mut d = Dataset::new("shapes", gt, vec![]);
    let (a, b, c) = (Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0));
    let g = match gt {
        GeometryType::Polyline => Geometry::Line(vec![vec![a, c]]),
        _ => Geometry::Polygon(vec![vec![vec![a, b, c, a]]]),
    };
    d.push(Some(g), vec![]);
    map.add_dataset(d);
    if let Some(l) = map.layers.last_mut() {
        edit(l);
    }
    map
}

/// How many pixels in `xs` × `ys` are within 40 of `c` in every channel.
fn inked(pm: &Pixmap, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>, c: Rgba) -> usize {
    let near = |a: u8, b: u8| a.abs_diff(b) <= 40;
    ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
        .filter(|(x, y)| pm.pixel(*x, *y).is_some_and(|p| near(p.red(), c.r()) && near(p.green(), c.g()) && near(p.blue(), c.b())))
        .count()
}

/// A stacked stroke `width` points wide in `color`.
fn plain_stroke(color: Rgba, width: f32) -> rustgis_map::LineStroke {
    rustgis_map::LineStroke { color: Some(color), width: Some(width), ..rustgis_map::LineStroke::default() }
}

/// Hatch lines `spacing` points apart, a point wide, across.
fn hatch(color: Rgba, spacing: f32) -> rustgis_map::fills::FillLayer {
    rustgis_map::fills::FillLayer {
        color: Some(color),
        pattern: rustgis_map::fills::FillPattern::Hatch { angle: 0.0, spacing, width: 1.0, offset: 0.0 },
    }
}

/// A line or polygon layer with no stacked strokes or fills keeps the swatch it always had,
/// pixel for pixel (#2490): a line through three points, or a box with its outline.
#[test]
fn a_plain_line_or_polygon_keeps_its_swatch() {
    let s = rustgis_map::Symbol {
        fill: Rgba::rgb(255, 0, 0),
        stroke: Rgba::rgb(0, 0, 255),
        stroke_width: 2.0,
        ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0)
    };
    for gt in [GeometryType::Polyline, GeometryType::Polygon] {
        let map = shape_legend_map(gt, |l| l.renderer = rustgis_map::Renderer::Single(s));
        let mut want = Pixmap::new(200, 200).unwrap();
        want.fill(tiny_skia::Color::WHITE);
        let stroke = Stroke { width: 2.0, ..Stroke::default() };
        crate::canvas::with_canvas(&mut want, |c| {
            if gt == GeometryType::Polyline {
                let mut pb = PathBuilder::new();
                pb.move_to(0.0, 10.5);
                pb.line_to(6.4, 4.5);
                pb.line_to(16.0, 9.0);
                c.stroke_path(&pb.finish().unwrap(), &paint(s.stroke, 1.0), &stroke, Transform::identity(), None);
            } else {
                let b = Rect::from_xywh(0.0, 3.0, 16.0, 9.0).unwrap();
                c.fill_rect(b, &paint(s.fill, 1.0), Transform::identity(), None);
                c.stroke_path(&PathBuilder::from_rect(b), &paint(s.stroke, 1.0), &stroke, Transform::identity(), None);
            }
        });
        assert!(same_corner(&point_legend(&map), &want, 16, 15), "{gt:?}");
    }
}

/// A line's stacked strokes show in its swatch as on the map: a casing under its core, and
/// dashes (#2490).
#[test]
fn a_legend_draws_a_lines_casing_and_dashes() {
    let (black, red) = (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 0, 0));
    let drawn =
        |strokes: Vec<rustgis_map::LineStroke>| point_legend(&shape_legend_map(GeometryType::Polyline, |l| l.line_strokes = strokes));
    let cased = drawn(vec![plain_stroke(black, 8.0), plain_stroke(red, 2.0)]);
    assert!(inked(&cased, 0..16, 0..15, black) > 20, "the casing");
    assert!(inked(&cased, 0..16, 0..15, red) > 5, "the core over it");
    let dashed = rustgis_map::LineStroke {
        dash: rustgis_map::symbology::Dash::new(&[2.0, 4.0]),
        cap: rustgis_map::LineCap::Butt,
        ..plain_stroke(red, 2.0)
    };
    let dashed = drawn(vec![plain_stroke(black, 8.0), dashed]);
    assert!(inked(&dashed, 0..16, 0..15, red) * 3 < inked(&cased, 0..16, 0..15, red) * 2, "a dashed core shows less of it");
    // One plain stroke draws in its own colour alone.
    assert_eq!(inked(&drawn(vec![plain_stroke(red, 2.0)]), 0..16, 0..15, black), 0);
}

/// Markers along a line show in its swatch (#2490).
#[test]
fn a_legend_draws_markers_along_a_line() {
    let blue = Rgba::rgb(0, 0, 255);
    let map = shape_legend_map(GeometryType::Polyline, |l| {
        l.line_strokes = vec![plain_stroke(Rgba::rgb(128, 128, 128), 1.0), square_mark(blue, 6.0)];
    });
    assert!(inked(&point_legend(&map), 0..16, 0..15, blue) > 10);
}

/// With symbol levels on, a line's swatch draws its strokes level by level, as the map does.
#[test]
fn a_legend_draws_a_lines_strokes_by_level() {
    let (black, red) = (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 0, 0));
    let drawn = |levels: bool| {
        let map = shape_legend_map(GeometryType::Polyline, |l| {
            l.line_strokes = vec![
                rustgis_map::LineStroke { level: Some(1), ..plain_stroke(red, 2.0) },
                rustgis_map::LineStroke { level: Some(0), ..plain_stroke(black, 8.0) },
            ];
            l.stroke_levels = levels;
        });
        inked(&point_legend(&map), 0..16, 0..15, red)
    };
    assert!(drawn(true) > 5, "the core's level is above the casing's");
    assert_eq!(drawn(false), 0, "in their order, the casing covers the core");
}

/// A stroke shifted past its row is cut at the swatch's box, not drawn over the next row.
#[test]
fn a_legend_cuts_a_shifted_stroke_at_its_box() {
    let red = Rgba::rgb(255, 0, 0);
    for (offset, shows) in [(4.0, true), (30.0, false)] {
        let map = shape_legend_map(GeometryType::Polyline, |l| {
            l.line_strokes = vec![rustgis_map::LineStroke { offset, ..plain_stroke(red, 3.0) }];
        });
        let pm = point_legend(&map);
        assert_eq!(inked(&pm, 0..16, 0..15, red) > 0, shows, "offset {offset}");
        assert_eq!(inked(&pm, 0..200, 15..200, red), 0, "offset {offset}: nothing below its row");
    }
    // One plain stroke wider than the row is cut at it too.
    let map = shape_legend_map(GeometryType::Polyline, |l| l.line_strokes = vec![plain_stroke(red, 30.0)]);
    let pm = point_legend(&map);
    assert!(inked(&pm, 0..16, 0..15, red) > 50);
    assert_eq!(inked(&pm, 0..200, 15..200, red), 0, "a wide stroke: nothing below its row");
    // Its round ends reach into the gap before the text, 6 points, and no further (#2581): a
    // label beside it keeps its own pixels. The same for a plain polygon's wide outline.
    assert!(inked(&pm, 16..22, 0..15, red) > 0, "into the gap");
    assert_eq!(inked(&pm, 22..200, 0..200, red), 0, "nothing where the text starts");
    let outline = rustgis_map::Symbol { stroke: red, stroke_width: 30.0, ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0) };
    let map = shape_legend_map(GeometryType::Polygon, |l| l.renderer = rustgis_map::Renderer::Single(outline));
    let pm = point_legend(&map);
    assert!(inked(&pm, 16..22, 0..15, red) > 0, "a plain outline into the gap");
    assert_eq!(inked(&pm, 22..200, 0..200, red), 0, "a plain outline: nothing where the text starts");
}

/// A polygon's stacked fills show in its swatch: a hatch over a fill, and a class's own fills
/// in its row, the other classes' plain (#2490).
#[test]
fn a_legend_draws_a_polygons_stacked_fills() {
    let (black, red, white) = (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 0, 0), Rgba::rgb(255, 255, 255));
    let solid = rustgis_map::fills::FillLayer { color: Some(Rgba::rgb(0, 200, 0)), pattern: rustgis_map::fills::FillPattern::Solid };
    let map = shape_legend_map(GeometryType::Polygon, |l| l.fill_layers = vec![solid, hatch(black, 3.0)]);
    let pm = point_legend(&map);
    assert!(inked(&pm, 1..15, 4..11, black) > 5, "the hatch");
    assert!(inked(&pm, 1..15, 4..11, Rgba::rgb(0, 200, 0)) > 5, "the fill under it");
    // Classes: only the first has fills of its own, and the layer none.
    let s = rustgis_map::Symbol { fill: red, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0) };
    let map = shape_legend_map(GeometryType::Polygon, |l| {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".to_string(), s), ("b".to_string(), s)],
            other: s,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: [("a".to_string(), vec![hatch(black, 3.0)])].into_iter().collect(),
            other_fills: Vec::new(),
            expression: None,
        };
    });
    let pm = point_legend(&map);
    // The layer's name, then a row a class, the swatch 6 points in.
    assert!(inked(&pm, 7..21, 19..26, black) > 5, "class a's hatch");
    assert_eq!(inked(&pm, 7..21, 19..26, red), 0, "a hatch with no fill under it leaves the paper");
    assert!(inked(&pm, 7..21, 19..26, white) > 5);
    assert_eq!(inked(&pm, 7..21, 34..41, black), 0, "class b has none");
    assert!(inked(&pm, 7..21, 34..41, red) > 50, "its symbol's fill");
}

/// A polygon's outline is drawn over its stacked fills, as on the map (#2490).
#[test]
fn a_legend_draws_a_polygons_outline_over_its_fills() {
    let (blue, green) = (Rgba::rgb(0, 0, 255), Rgba::rgb(0, 200, 0));
    let s = rustgis_map::Symbol { stroke: blue, stroke_width: 2.0, ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0) };
    let solid = rustgis_map::fills::FillLayer { color: Some(green), pattern: rustgis_map::fills::FillPattern::Solid };
    let map = shape_legend_map(GeometryType::Polygon, |l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.fill_layers = vec![solid];
    });
    let pm = point_legend(&map);
    // The box's top edge, 3 points down, under the 2-point outline; its inside, the fill.
    assert!(inked(&pm, 1..15, 3..4, blue) > 10, "the outline");
    assert_eq!(inked(&pm, 1..15, 3..4, green), 0, "no fill over it");
    assert!(inked(&pm, 2..14, 5..10, green) > 50, "the fill inside");
}

/// A diagram's categories keep their own colour on a layer with stacked strokes or fills.
#[test]
fn a_diagrams_rows_keep_their_colour_on_lines_and_polygons() {
    let (black, red, green) = (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 0, 0), Rgba::rgb(0, 255, 0));
    let category = |field: &str, colour| rustgis_map::DiagramCategory { field: field.into(), colour };
    for gt in [GeometryType::Polyline, GeometryType::Polygon] {
        let map = shape_legend_map(gt, |l| {
            l.line_strokes = vec![plain_stroke(black, 8.0)];
            l.fill_layers = vec![hatch(black, 3.0)];
            l.diagram = Some(rustgis_map::DiagramSettings {
                kind: rustgis_map::DiagramKind::Pie,
                categories: vec![category("a", red), category("b", green)],
                size: rustgis_map::DiagramSize::Fixed(12.0),
                start_angle: 0.0,
                background: None,
                stroke: rustgis_map::DiagramSettings::default_stroke(),
                stroke_width: 0.75,
                labels: rustgis_map::DiagramLabel::None,
            });
        });
        let pm = point_legend(&map);
        assert_eq!(inked(&pm, 6..22, 15..45, black), 0, "{gt:?}: not the layer's stack");
        // A one-point line is mostly part-covered pixels: its hue tells it.
        let tinted = |ys: std::ops::Range<u32>, hue: fn(tiny_skia::PremultipliedColorU8) -> bool| {
            ys.flat_map(|y| (6..22).map(move |x| (x, y))).filter(|(x, y)| pm.pixel(*x, *y).is_some_and(hue)).count()
        };
        assert!(tinted(15..30, |p| p.red() > p.green().saturating_add(60)) > 5, "{gt:?}: red");
        assert!(tinted(30..45, |p| p.green() > p.red().saturating_add(60)) > 5, "{gt:?}: green");
    }
}

/// A see-through line or polygon layer's swatch is see-through too, as its points' are.
#[test]
fn a_legend_draws_a_see_through_polygon_see_through() {
    let s =
        rustgis_map::Symbol { fill: Rgba::rgb(255, 0, 0), stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0) };
    let map = shape_legend_map(GeometryType::Polygon, |l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.transparency = 50.0;
    });
    let p = point_legend(&map).pixel(8, 7).unwrap();
    assert_eq!(p.red(), 255);
    assert!((100..160).contains(&p.green()), "{}", p.green());
}

/// A line grows with its layer's reference scale in the swatch, as on the map.
#[test]
fn a_legend_scales_a_line_with_its_reference_scale() {
    let blue = Rgba::rgb(0, 0, 255);
    let drawn = |reference: Option<f64>| {
        let map = shape_legend_map(GeometryType::Polyline, |l| {
            l.line_strokes = vec![plain_stroke(blue, 2.0)];
            l.reference_scale = reference;
        });
        inked(&point_legend(&map), 0..16, 0..15, blue)
    };
    // Drawn at 1:1e6: a reference of 1:2e6 makes it twice as wide.
    assert!(drawn(Some(2e6)) * 2 > drawn(None) * 3, "{} against {}", drawn(Some(2e6)), drawn(None));
}

/// A vector export draws the stacked fills and strokes a raster one does.
#[test]
fn a_recorded_legend_draws_stacked_fills_and_strokes() {
    let (black, red) = (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 0, 0));
    let map = shape_legend_map(GeometryType::Polygon, |l| l.fill_layers = vec![hatch(black, 3.0)]);
    assert!(inked(&recorded_legend(&map, 200), 1..15, 4..11, black) > 5);
    let map = shape_legend_map(GeometryType::Polyline, |l| l.line_strokes = vec![plain_stroke(black, 8.0), plain_stroke(red, 2.0)]);
    let pm = recorded_legend(&map, 200);
    assert!(inked(&pm, 0..16, 0..15, black) > 20 && inked(&pm, 0..16, 0..15, red) > 5);
}

/// A box off the pixel grid: a recording cuts its symbol at the box, not at the whole pixels
/// round it, so the pixel its edge crosses is half covered.
#[test]
fn a_recorded_point_is_cut_at_its_box() {
    let s = rustgis_map::Symbol {
        fill: Rgba::rgb(0, 0, 255),
        size: 3000.0,
        stroke_width: 0.0,
        marker: rustgis_map::Marker::Square,
        ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
    };
    let map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s));
    let mut c = Canvas::recording(2100, 2100).unwrap();
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    legend(&mut c, Rect::from_xywh(0.5, 0.5, 2099.0, 2099.0).unwrap(), &map, ("", 1), None, opts, 1.0);
    let (_, _, items) = c.into_drawing().unwrap().finish();
    let mut pm = Pixmap::new(2100, 2100).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    crate::vector::replay(&items, &mut pm, Transform::identity());
    // The box runs from 0.5 to 2048.5.
    let red = |x, y| pm.pixel(x, y).map_or(0, |p| p.red());
    assert_eq!(red(10, 10), 0);
    assert!((60..200).contains(&red(2048, 10)), "{}", red(2048, 10));
    assert!((60..200).contains(&red(10, 2048)), "{}", red(10, 2048));
}

/// An upright square's and triangle's corners lie further than 0.6 of their size from their
/// centre, but along each axis, which is what the box holds, they stay inside it.
#[test]
fn an_upright_mark_stays_in_its_row() {
    let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    for marker in [rustgis_map::Marker::Square, rustgis_map::Marker::Triangle] {
        let mut mark = square_mark(Rgba::rgb(0, 0, 255), 40.0);
        if let Some(m) = &mut mark.markers {
            m.mark = rustgis_map::LineMark::Marker(marker);
        }
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.line_strokes = vec![mark];
        });
        let pm = point_legend(&map);
        let white = |x, y| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (255, 255, 255));
        // A box of 48 points: its edges are clear, and the mark is in it.
        assert!((0..48).all(|i| white(i, 0) && white(i, 47) && white(0, i) && white(47, i)), "{marker:?}");
        assert!(!white(24, 24), "{marker:?}");
    }
}

/// A class's row draws the marks the map gives its features, else its layer's.
#[test]
fn a_legend_draws_a_class_with_its_own_marks() {
    let s = rustgis_map::Symbol::default_for(GeometryType::Point, 0);
    let (own, others, layers) = (
        vec![square_mark(Rgba::rgb(255, 0, 0), 12.0)],
        vec![square_mark(Rgba::rgb(0, 255, 0), 12.0)],
        vec![square_mark(Rgba::rgb(0, 0, 255), 12.0)],
    );
    let map = point_legend_map(|l| {
        l.line_strokes = layers.clone();
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".to_string(), s), ("b".to_string(), s)],
            other: s,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: [("a".to_string(), own.clone())].into_iter().collect(),
            other_strokes: others.clone(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    });
    let got = point_legend(&map);
    // Under the layer's name, each row 15 points tall, its swatch 6 in: class a has its own,
    // class b takes the layer's, and the fallback has its own.
    for (row, strokes) in [(1u32, &own), (2, &layers), (3, &others)] {
        let top = row * 15;
        let want = drawn_point(&s, strokes, (14.0, top as f32 + 7.5), 1.0);
        assert!((top..top + 15).all(|y| (6..22).all(|x| got.pixel(x, y) == want.pixel(x, y))), "row {row}");
    }
}

/// A large point widens the column: a line or a box stays a swatch wide, and the names line up.
#[test]
fn a_wide_column_keeps_lines_and_boxes_a_swatch_wide_in_its_middle() {
    use rustgis_data::Dataset;
    let blue = Rgba::rgb(0, 0, 255);
    let point =
        rustgis_map::Symbol { fill: blue, size: 40.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    for gt in [GeometryType::Polyline, GeometryType::Polygon] {
        // A point of 40 pt makes the column 48 wide: a swatch of 16 in its middle runs from 16 to 32.
        let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(point));
        let id = map.add_dataset(Dataset::new("line", gt, vec![]));
        let red = Rgba::rgb(255, 0, 0);
        if let Some(l) = map.layer_mut(id) {
            l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
                fill: red,
                stroke: red,
                stroke_width: 2.0,
                ..rustgis_map::Symbol::default_for(gt, 0)
            });
        }
        let pm = point_legend(&map);
        let columns = |ink: &dyn Fn(u8, u8, u8) -> bool| -> Vec<u32> {
            (0..pm.width())
                .filter(|&x| (0..pm.height()).any(|y| pm.pixel(x, y).is_some_and(|p| p.alpha() > 0 && ink(p.red(), p.green(), p.blue()))))
                .collect()
        };
        let swatch = columns(&|r, g, b| r > 200 && g < 60 && b < 60);
        let (left, right) = (swatch.first().copied().unwrap_or(0), swatch.last().copied().unwrap_or(0));
        assert!((14..=17).contains(&left) && (31..=34).contains(&right), "{gt:?}: {left} to {right}");
        // Both names start past the column, where the blue point has no part in it.
        let names = columns(&|r, g, b| r < 100 && g < 100 && b < 100);
        assert!(names.first().is_some_and(|x| (53..60).contains(x)), "{gt:?}: {names:?}");
    }
}

/// A symbol past the limit fills its box and stays in it, on pixels and in a recording.
#[test]
fn an_oversized_point_stays_in_its_row() {
    let blue = Rgba::rgb(0, 0, 255);
    let s = rustgis_map::Symbol {
        fill: blue,
        size: 3000.0,
        stroke_width: 0.0,
        marker: rustgis_map::Marker::Square,
        ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
    };
    // Opaque and see-through: a row and a column of 2048 pt, which the square fills.
    for transparency in [0.0, 50.0] {
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.transparency = transparency;
        });
        let mut pm = Pixmap::new(2100, 2100).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 2100.0, 2100.0).unwrap(), &map, ("", 1), None, opts, 1.0)
        });
        // Drawn, and recorded for a vector export, where only its clip cuts it.
        for pm in [pm, recorded_legend(&map, 2100)] {
            let white = |x, y| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (255, 255, 255));
            assert!(!white(10, 10) && !white(2040, 2040), "transparency {transparency}: it fills the box");
            assert!(white(10, 2060) && white(2060, 10), "transparency {transparency}: it stays in the box");
        }
    }
}

/// A legend starting off the canvas: a symbol larger than the canvas still covers all of it.
#[test]
fn a_see_through_point_larger_than_the_canvas_covers_it_from_off_its_edge() {
    let s = rustgis_map::Symbol {
        fill: Rgba::rgb(0, 0, 255),
        size: 3000.0,
        stroke_width: 0.0,
        marker: rustgis_map::Marker::Square,
        ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
    };
    let map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Single(s);
        l.transparency = 50.0;
    });
    let mut pm = Pixmap::new(300, 300).unwrap();
    pm.fill(tiny_skia::Color::WHITE);
    let opts = LegendOptions { show_no_data: false, hide_other: false, exclude: &[], labels: &NO_LABELS, scale: 1e6, size: 1.0 };
    crate::canvas::with_canvas(&mut pm, |c| {
        legend(c, Rect::from_xywh(-50.0, -50.0, 2100.0, 2100.0).unwrap(), &map, ("", 1), None, opts, 1.0)
    });
    // Its box runs from -50 to 1998: every corner of the canvas is in it.
    for (x, y) in [(5, 5), (295, 5), (5, 295), (295, 295)] {
        let white = pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (255, 255, 255));
        assert!(!white, "({x}, {y})");
    }
}

/// A class's large symbol counts in a legend's size as a plain row's does.
#[test]
fn a_class_row_takes_the_wide_column_in_a_legends_size() {
    let mm = 25.4 / 96.0;
    let s = rustgis_map::Symbol { size: 40.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
    let mut map = point_legend_map(|l| {
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".to_string(), s)],
            other: s,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    });
    // The layer's name over the class: a row of 15 and one of 48, 6 + 48 + 6 before its label.
    let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, true, (&[], &NO_LABELS)).unwrap();
    assert!((h - f64::from(LEGEND_ROW + 48.0) * mm).abs() < 1e-6, "{h} mm");
    assert!((w - f64::from(48.0 + 12.0 + text::measure("a", 8.5, false)) * mm).abs() < 1e-4, "{w} mm");
}

/// An element's background (#2257) is drawn under it on the page, grown by its padding and
/// turned with a turned text; drawn alone, as an SVG's picture of it, the element leaves it to
/// the SVG's own shape.
#[test]
fn a_background_is_drawn_under_its_element_and_turns_with_it() {
    use rustgis_map::layout::Background;
    let px = |mm: f64| (mm * 96.0 / 25.4).round() as u32;
    let red = |pm: &Pixmap, x: f64, y: f64| rgb(pm, px(x), px(y)) == (255, 0, 0);
    let page = |turn: f64, background: Option<Background>| {
        let mut l = Layout::new("L", 100.0, 100.0);
        let mut kind = ElementKind::text("", 12.0, Rgba::rgb(0, 0, 0), false);
        if let ElementKind::Text { rotation, .. } = &mut kind {
            *rotation = turn;
        }
        let id = l.add("T", (40.0, 40.0, 20.0, 10.0), kind);
        l.element_mut(id).unwrap().background = background;
        (l, id)
    };
    let back = Background { padding: 5.0, ..Background::filled(Rgba::rgb(255, 0, 0)) };
    let (plain, _) = page(0.0, None);
    assert!(!red(&render(&plain, &mut [], 96.0, &PageInfo::default()).unwrap(), 45.0, 45.0), "none by default");
    let (l, id) = page(0.0, Some(back));
    let pm = render(&l, &mut [], 96.0, &PageInfo::default()).unwrap();
    assert!(red(&pm, 45.0, 45.0), "inside the box");
    assert!(red(&pm, 37.0, 37.0), "in the padding");
    assert!(!red(&pm, 33.0, 45.0), "past the padding");
    // Turned 90°, the box (and so its background) stands upright about its centre (50, 45).
    let (turned, _) = page(90.0, Some(Background::filled(Rgba::rgb(255, 0, 0))));
    let pm = render(&turned, &mut [], 96.0, &PageInfo::default()).unwrap();
    assert!(red(&pm, 50.0, 37.0), "above the unturned box, inside the turned one");
    assert!(!red(&pm, 42.0, 45.0), "left of the turned box");
    // Clockwise, as the text turns: at 30° the box's right end dips below its centre line, at
    // -30° it rises (90° alone would look the same either way).
    let at = |turn: f64| {
        red(
            &render(&page(turn, Some(Background::filled(Rgba::rgb(255, 0, 0)))).0, &mut [], 96.0, &PageInfo::default()).unwrap(),
            59.0,
            46.0,
        )
    };
    assert!(at(30.0) && !at(-30.0), "turned clockwise");
    // Alone, the element's picture holds no background.
    let (alone, _) = render_element(&l, id, &mut [], 96.0, (&PageInfo::default(), &Env::default()), &mut None).unwrap().unwrap();
    assert!(alone.pixels().iter().all(|c| c.alpha() == 0 || c.green() > 0), "no red in the element's own picture");
}
