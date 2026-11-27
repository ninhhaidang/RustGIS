use super::*;
use crate::symbology::MAX_DASH_STEP;

/// Several figures of one layer, worked out in one pass over its rows, are the figures worked out
/// one by one, errors included (#2853).
#[test]
fn a_layers_figures_in_one_pass_are_the_figures_one_by_one() {
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    let fields = vec![Field::new("n", FieldType::Double), Field::new("m", FieldType::Integer), Field::new("name", FieldType::Text)];
    let mut ds = Dataset::new("sites", GeometryType::None, fields);
    for (n, m) in [(Value::Float(2.5), 3), (Value::Float(-1.0), 4), (Value::Null, 5), (Value::Float(f64::NAN), 6), (Value::Float(100.0), 7)]
    {
        ds.push(None, vec![n, Value::Int(m), Value::Text("x".into())]);
    }
    let mut layer = crate::Layer::new(1, ds);
    let wanted = [
        (StatKind::Count, None),
        (StatKind::Sum, Some("n")),
        (StatKind::Mean, Some("n")),
        (StatKind::Min, Some("m")),
        (StatKind::Max, Some("m")),
        (StatKind::Sum, Some("name")),
        (StatKind::Sum, Some("nosuch")),
        (StatKind::Sum, None),
        (StatKind::Mean, Some("n")),
    ];
    for query in [None, Some("m > 3")] {
        layer.definition_query = query.map(String::from);
        let one_by_one: Vec<_> = wanted.iter().map(|(stat, field)| layer_statistic(&layer, *stat, *field)).collect();
        assert_eq!(layer_statistics(&layer, &wanted), one_by_one, "{query:?}");
    }
}

/// A layer renamed is renamed in the tokens that name it, whatever their statistic or field, and
/// a text none of them names is left as it is (#2853).
#[test]
fn statistic_tokens_follow_a_renamed_layer() {
    let to_z = |l: &str| (l == "A").then(|| "Z".to_string());
    assert_eq!(
        rename_stat_layers("{count:A} {sum:A:f} {count:A} {max:B:A} {scale}", to_z).as_deref(),
        Some("{count:Z} {sum:Z:f} {count:Z} {max:B:A} {scale}")
    );
    assert_eq!(rename_stat_layers("{count:B} {sum:B:A} A", to_z), None);
}

/// The tokens a notice reads also include the figure kinds written without their field, which
/// are no tokens to draw or to require a capability for (#2853).
#[test]
fn statistic_tokens_and_gaps_include_a_figure_without_its_field() {
    fn read(t: &str) -> Vec<(&str, StatKind, &str, Option<&str>)> {
        stat_tokens_and_gaps(t).into_iter().map(|s| (s.token, s.stat, s.layer, s.field)).collect()
    }
    assert_eq!(
        read("{sum:A} {sum:A:b} {count:C} {mean:Zone: north} {max:} {sum::x} {min:A:} {foo:A}"),
        vec![
            ("{sum:A}", StatKind::Sum, "A", None),
            ("{sum:A:b}", StatKind::Sum, "A", Some("b")),
            ("{count:C}", StatKind::Count, "C", None),
            ("{mean:Zone: north}", StatKind::Mean, "Zone", Some(" north")),
            ("{min:A:}", StatKind::Min, "A", None),
        ]
    );
    assert_eq!(stat_tokens("{sum:A} {sum:A:b}").len(), 1, "a figure without its field is no token");
}

/// A text's statistic tokens (#2853): each kind, a layer named with a colon, and braces that
/// aren't one left alone.
#[test]
fn statistic_tokens_are_read_from_a_text() {
    fn read(t: &str) -> Vec<(&str, StatKind, &str, Option<&str>)> {
        stat_tokens(t).into_iter().map(|s| (s.token, s.stat, s.layer, s.field)).collect()
    }
    assert_eq!(
        read("{count:Houses} houses, {sum:Plants:mw} MW at {scale}"),
        vec![("{count:Houses}", StatKind::Count, "Houses", None), ("{sum:Plants:mw}", StatKind::Sum, "Plants", Some("mw")),]
    );
    assert_eq!(
        read("{mean:Zone: north:pop}{min:A:b}{max:A:b}{count:Zone: north}"),
        vec![
            ("{mean:Zone: north:pop}", StatKind::Mean, "Zone: north", Some("pop")),
            ("{min:A:b}", StatKind::Min, "A", Some("b")),
            ("{max:A:b}", StatKind::Max, "A", Some("b")),
            ("{count:Zone: north}", StatKind::Count, "Zone: north", None),
        ]
    );
    // Not tokens: other names, an empty layer or field, a sum without its field, an alias the
    // chart's codes take, a brace left open, a brace inside.
    for t in ["{foo}", "{sum:}", "{count:}", "{sum:A}", "{sum::b}", "{sum:A:}", "{average:A:b}", "{Count:A}", "{count:A", "{count:{A}}"] {
        assert_eq!(read(t), vec![], "{t}");
    }
}

/// A layer's statistic (#2853) over the features its definition query keeps, its numbers read
/// as a chart's: nulls and non-finite values skipped; a field it lacks, one with no numbers, or
/// no number among the kept features, none.
#[test]
fn layer_statistics_read_the_features_the_definition_query_keeps() {
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    let fields = vec![Field::new("n", FieldType::Double), Field::new("name", FieldType::Text), Field::new("keep", FieldType::Integer)];
    let mut ds = Dataset::new("sites", GeometryType::None, fields);
    for (n, keep) in
        [(Value::Float(2.5), 1), (Value::Float(-1.0), 1), (Value::Null, 1), (Value::Float(f64::NAN), 1), (Value::Float(100.0), 0)]
    {
        ds.push(None, vec![n, Value::Text("x".into()), Value::Int(keep)]);
    }
    let mut layer = crate::Layer::new(1, ds);
    let stat = layer_statistic;
    // Every feature: five, and the 100 counted.
    assert_eq!(stat(&layer, StatKind::Count, None), Ok(5.0));
    assert_eq!(stat(&layer, StatKind::Max, Some("n")), Ok(100.0));
    layer.definition_query = Some("keep = 1".into());
    assert_eq!(stat(&layer, StatKind::Count, None), Ok(4.0));
    assert_eq!(stat(&layer, StatKind::Sum, Some("n")), Ok(1.5));
    assert_eq!(stat(&layer, StatKind::Mean, Some("n")), Ok(0.75));
    assert_eq!(stat(&layer, StatKind::Min, Some("n")), Ok(-1.0));
    assert_eq!(stat(&layer, StatKind::Max, Some("n")), Ok(2.5));
    assert_eq!(stat(&layer, StatKind::Sum, Some("nosuch")), Err(StatMissing::Field));
    assert_eq!(stat(&layer, StatKind::Sum, None), Err(StatMissing::Field));
    assert_eq!(stat(&layer, StatKind::Sum, Some("name")), Err(StatMissing::NotNumeric));
    // A figure is over the whole layer, so a query that reads the series page's values (`@page_number`)
    // is read with no page: the values are null and it keeps nothing.
    layer.definition_query = Some("@page_number > 0".into());
    assert_eq!(stat(&layer, StatKind::Count, None), Ok(0.0));
    // A query keeping none: a count of 0, no sum.
    layer.definition_query = Some("keep = 7".into());
    assert_eq!(stat(&layer, StatKind::Count, None), Ok(0.0));
    assert_eq!(stat(&layer, StatKind::Sum, Some("n")), Err(StatMissing::NoValue));
    // A query that doesn't read keeps every feature, as the map draws them all.
    layer.definition_query = Some("keep = = 1".into());
    assert_eq!(stat(&layer, StatKind::Count, None), Ok(5.0));
}

/// A chart of layer "t" with `extra` keys (its field among them).
fn chart(extra: serde_json::Value) -> ElementKind {
    let mut c = serde_json::json!({"frame": 1, "layer": "t"});
    if let (Some(c), Some(e)) = (c.as_object_mut(), extra.as_object()) {
        c.extend(e.clone());
    }
    serde_json::from_value(serde_json::json!({ "Chart": c })).unwrap()
}

/// Places: name, region, people; a missing number for region "S", a blank region.
fn places() -> rustgis_data::Dataset {
    use rustgis_data::{Dataset, Field, FieldType, Value};
    let mut ds = Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![Field::new("name", FieldType::Text), Field::new("region", FieldType::Text), Field::new("people", FieldType::Double)],
    );
    let text = |t: &str| Value::Text(t.into());
    for (name, region, people) in [
        ("a", text("N"), Value::Float(10.0)),
        ("b", text("S"), Value::Float(4.0)),
        ("c", text("N"), Value::Float(2.0)),
        ("d", text("E"), Value::Float(7.0)),
        ("e", text("S"), Value::Null),
        ("f", Value::Null, Value::Float(1.0)),
        ("g", text("W"), Value::Float(-3.0)),
    ] {
        ds.push(None, vec![text(name), region, people]);
    }
    ds
}

fn bars(d: &ChartData) -> Vec<(f64, &str)> {
    d.bars.iter().map(|(v, n)| (*v, n.as_str())).collect()
}

/// A layer's colours as its map frame draws them (#2487 part 3): each feature by its own
/// geometry (a line in a point layer by its stroke), one without by the layer's, and a colour
/// that depends on `@map_scale` at the frame's scale.
#[test]
fn chart_bars_read_their_layers_colours_as_the_frame_draws_them() {
    use rustgis_data::query::Env;
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
    let mut ds = Dataset::new("mixed", GeometryType::Point, vec![Field::new("n", FieldType::Double)]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Float(3.0)]);
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), vec![Value::Float(2.0)]);
    ds.push(None, vec![Value::Float(1.0)]);
    let mut layer = crate::Layer::new(1, ds);
    let (fill, stroke) = (Rgba::rgb(0, 0, 200), Rgba::rgb(200, 0, 0));
    let mut symbol = layer.renderer.base_symbol();
    (symbol.fill, symbol.stroke, symbol.stroke_width) = (fill, stroke, 1.0);
    layer.renderer = crate::symbology::Renderer::Single(symbol);
    let kind = chart(serde_json::json!({"field": "n", "classes": true}));
    // Largest first: the point, the line, the row without a geometry (drawn as a point).
    let d = kind.chart_data_of(&layer, &Env::default()).unwrap();
    assert_eq!(d.colors, vec![Some(fill), Some(stroke), Some(fill)]);
    // A fill that depends on the scale: the frame's, or none without one.
    layer.symbol_by.fill = Some("CASE WHEN @map_scale < 100000 THEN '#00ff00' ELSE '#ff00ff' END".into());
    let near = kind.chart_data_of(&layer, &Env::default().at_scale(Some(25_000.0))).unwrap();
    assert_eq!(near.colors.first(), Some(&Some(Rgba::rgb(0, 255, 0))));
    let far = kind.chart_data_of(&layer, &Env::default().at_scale(Some(1e6))).unwrap();
    assert_eq!(far.colors.first(), Some(&Some(Rgba::rgb(255, 0, 255))));
}

/// Coloured by classes (#2487 part 3): a bar per feature takes its feature's colour, a grouped
/// bar the colour its counted features share, none (the chart's) when they have several, which
/// are counted, and an Other bar none. Without `classes`, or without colours, none are held.
#[test]
fn chart_bars_take_their_features_colours() {
    let ds = places();
    // A colour per name, so every feature has its own; and per region, shared within one.
    let by_name = |row: usize| Some(Rgba::rgb(row as u8, 0, 0));
    let region = ds.field_index("region").unwrap();
    let by_region = |row: usize| match ds.value(row, region) {
        rustgis_data::Value::Text(t) if t == "N" => Some(Rgba::rgb(1, 0, 0)),
        _ => Some(Rgba::rgb(2, 0, 0)),
    };
    let one = chart(serde_json::json!({"field": "people", "classes": true}));
    let d = one.chart_data_with(&ds, Some(&by_name)).unwrap();
    // Largest first: a (10), d (7), b (4), c (2), f (1), g (-3).
    let rows = [0u8, 3, 1, 2, 5, 6];
    assert_eq!(d.colors, rows.iter().map(|r| Some(Rgba::rgb(*r, 0, 0))).collect::<Vec<_>>());
    assert_eq!(d.mixed, 0);
    // Grouped by region: N is a and c, which have a colour each by name.
    let grouped = chart(serde_json::json!({"field": "people", "group": "region", "classes": true}));
    let d = grouped.chart_data_with(&ds, Some(&by_name)).unwrap();
    assert_eq!(bars(&d), vec![(12.0, "N"), (7.0, "E"), (4.0, "S"), (1.0, CHART_BLANK), (-3.0, "W")]);
    // S is b alone: e has no number, so its sum doesn't read it.
    assert_eq!(
        d.colors,
        vec![None, Some(Rgba::rgb(3, 0, 0)), Some(Rgba::rgb(1, 0, 0)), Some(Rgba::rgb(5, 0, 0)), Some(Rgba::rgb(6, 0, 0))]
    );
    assert_eq!(d.mixed, 1);
    // A count reads every feature: S is b and e then.
    let counted = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "classes": true}));
    let d = counted.chart_data_with(&ds, Some(&by_name)).unwrap();
    assert_eq!(d.mixed, 2);
    // By region, every bar has one colour; the Other bar keeps the chart's.
    let cut = chart(serde_json::json!({"field": "people", "group": "region", "bars": 3, "other": true, "classes": true}));
    let d = cut.chart_data_with(&ds, Some(&by_region)).unwrap();
    assert_eq!(bars(&d).last().map(|b| b.1), Some(CHART_OTHER));
    assert_eq!(d.colors, vec![Some(Rgba::rgb(1, 0, 0)), Some(Rgba::rgb(2, 0, 0)), None]);
    assert_eq!((d.mixed, d.unshown), (0, 0), "the Other bar is neither");
    // Features drawn with nothing (a hidden class) give their bar none, not a clear one: N's
    // features are hidden, so N keeps the chart's colour without being mixed, and is counted.
    let hidden_n = |row: usize| by_region(row).filter(|c| *c != Rgba::rgb(1, 0, 0));
    let d = grouped.chart_data_with(&ds, Some(&hidden_n)).unwrap();
    assert_eq!(d.colors.first(), Some(&None));
    assert_eq!((d.mixed, d.unshown), (0, 1));
    let d = one.chart_data_with(&ds, Some(&hidden_n)).unwrap();
    assert_eq!(d.colors.first(), Some(&None), "a, in N");
    assert_eq!(d.unshown, 2, "a and c, in N");
    // Only some hidden, the bar takes the colour of those that show: N is a, hidden, and c.
    let hidden_a = |row: usize| by_name(row).filter(|_| row != 0);
    let d = grouped.chart_data_with(&ds, Some(&hidden_a)).unwrap();
    assert_eq!(d.colors.first(), Some(&Some(Rgba::rgb(2, 0, 0))));
    assert_eq!((d.mixed, d.unshown), (0, 0));
    // The colour a symbol shows: a line's stroke, an area's fill or its outline where the
    // fill is clear, none when it draws nothing.
    use crate::symbology::Symbol;
    use rustgis_data::GeometryType as G;
    let red = Rgba::rgb(200, 0, 0);
    let area = Symbol { fill: red, ..Symbol::default_for(G::Polygon, 0) };
    assert_eq!(area.shown_colour(G::Polygon), Some(red));
    assert_eq!(area.shown_colour(G::Polyline), Some(area.stroke));
    let outline = Symbol { fill: Rgba::new(0, 0, 0, 0), ..area };
    assert_eq!(outline.shown_colour(G::Polygon), Some(area.stroke));
    let hidden = Symbol { stroke: Rgba::new(0, 0, 0, 0), ..outline };
    assert_eq!(hidden.shown_colour(G::Polygon), None);
    // A point drawn as an image shows no one colour; drawn as a shape, its fill.
    let dot = Symbol { fill: red, ..Symbol::default_for(G::Point, 0) };
    assert_eq!(dot.shown_colour(G::Point), Some(red));
    let image = Symbol { marker: crate::symbology::Marker::Picture(crate::symbology::Picture::new("iVBORw0KGgo=")), ..dot };
    assert_eq!(image.shown_colour(G::Point), None);
    // Not coloured: no colours are read.
    assert!(one.chart_data(&ds).unwrap().colors.is_empty());
    let plain = chart(serde_json::json!({"field": "people"}));
    assert!(plain.chart_data(&ds).unwrap().colors.is_empty());
    assert!(!plain.chart_uses_classes() && one.chart_uses_classes());
}

/// Places by region and kind (#2487 part 3b): N has an x and a y, S an x and a y with no
/// number, E a y and one without a kind, W a negative x.
fn kinds() -> rustgis_data::Dataset {
    use rustgis_data::{Dataset, Field, FieldType, Value};
    let mut ds = Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Text), Field::new("people", FieldType::Double)],
    );
    let text = |t: &str| Value::Text(t.into());
    for (region, kind, people) in [
        ("N", text("x"), Value::Float(10.0)),
        ("N", text("y"), Value::Float(2.0)),
        ("S", text("x"), Value::Float(4.0)),
        ("S", text("y"), Value::Null),
        ("E", text("y"), Value::Float(7.0)),
        ("E", Value::Null, Value::Float(1.0)),
        ("W", text("x"), Value::Float(-3.0)),
    ] {
        ds.push(None, vec![text(region), kind, people]);
    }
    ds
}

fn key(d: &ChartData) -> Vec<&str> {
    d.key.iter().map(|(n, _)| n.as_str()).collect()
}

/// A grouped chart that sums or counts splits each bar by the values of its stack field, in
/// the order of the key: their totals, largest first, or by label A to Z; the stretches add up
/// to the bar, an Other bar's to its groups'. Ungrouped, by its own group field, or taking a
/// mean, it isn't stacked.
#[test]
fn stacked_bars_split_by_a_second_field() {
    let ds = kinds();
    let palette = crate::diagram::CATEGORY_COLORS;
    let summed = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind"}));
    let d = summed.chart_data(&ds).unwrap();
    assert_eq!(bars(&d), vec![(12.0, "N"), (8.0, "E"), (4.0, "S"), (-3.0, "W")]);
    assert_eq!(key(&d), vec!["x", "y", CHART_BLANK]);
    assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), palette[..3].to_vec());
    assert_eq!(d.stretches, vec![vec![10.0, 2.0, 0.0], vec![0.0, 7.0, 1.0], vec![4.0, 0.0, 0.0], vec![-3.0, 0.0, 0.0]]);
    assert_eq!((d.no_number, d.key_kept), (1, 0));
    assert!(d.colors.is_empty());

    // Counted, every feature is in a stretch, the one with no number too.
    let counted = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "stack": "kind"}));
    let d = counted.chart_data(&ds).unwrap();
    assert_eq!(bars(&d), vec![(2.0, "N"), (2.0, "S"), (2.0, "E"), (1.0, "W")]);
    assert_eq!(d.stretches, vec![vec![1.0, 1.0, 0.0], vec![1.0, 1.0, 0.0], vec![0.0, 1.0, 1.0], vec![1.0, 0.0, 0.0]]);

    // An Other bar adds up the stretches of the groups it gathers.
    let cut = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "bars": 2, "other": true}));
    let d = cut.chart_data(&ds).unwrap();
    assert_eq!(bars(&d), vec![(12.0, "N"), (9.0, CHART_OTHER)]);
    assert_eq!(d.stretches, vec![vec![10.0, 2.0, 0.0], vec![1.0, 7.0, 1.0]]);
    // The bars left out leave the key too: N shows no blank kind.
    let left = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "bars": 1}));
    let d = left.chart_data(&ds).unwrap();
    assert_eq!((key(&d), d.stretches.clone(), d.left_out), (vec!["x", "y"], vec![vec![10.0, 2.0]], 3));

    // By label, the key goes A to Z as the bars do.
    let by_label = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "order": "category"}));
    let d = by_label.chart_data(&ds).unwrap();
    assert_eq!(key(&d), vec![CHART_BLANK, "x", "y"]);
    assert_eq!(d.stretches.first(), Some(&vec![1.0, 0.0, 7.0]));

    for flat in [
        serde_json::json!({"field": "people", "stack": "kind"}),
        serde_json::json!({"field": "people", "group": "region", "stack": "Region"}),
        serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "stack": "kind"}),
        serde_json::json!({"field": "people", "group": "region", "stack": " "}),
        serde_json::json!({"field": "people", "group": "region", "stack": "region "}),
    ] {
        let kind = chart(flat.clone());
        // A blank one doesn't ask a reader for stacking.
        assert!(kind.chart_uses_stack() != (flat["stack"] == " ") && kind.chart_stack().is_none());
        let d = kind.chart_data(&ds).unwrap();
        assert!(d.key.is_empty() && d.stretches.is_empty());
    }
    assert!(summed.chart_uses_stack() && !chart(serde_json::json!({"field": "people"})).chart_uses_stack());
    // A stack field the layer hasn't (deleted since, say) leaves the bars whole, and says so.
    let gone = chart(serde_json::json!({"field": "people", "group": "region", "stack": "gone"})).chart_data(&ds).unwrap();
    let whole = chart(serde_json::json!({"field": "people", "group": "region"})).chart_data(&ds).unwrap();
    assert!(gone.stack_gone && gone.key.is_empty() && gone.stretches.is_empty() && gone.bars == whole.bars && !whole.stack_gone);
}

/// A bar a stretch of which adds up past a number's range, or whose stretches do once stacked,
/// its total within it, is drawn whole and counted, its stretches left empty rather than that
/// one taken as 0; the others stack.
#[test]
fn a_stretch_past_a_numbers_range_leaves_its_bar_whole() {
    let names = || vec![Some("x".to_string()), Some("y".to_string())];
    // Bar 0: x = 1e308 twice, past the range, y = -1e308. Bar 1, Other of groups 1 and 2,
    // each with an x of 1e308 and group 1 a y of -1e308: x adds up past it once merged.
    let cells =
        |c: &[((usize, usize), f64)]| c.iter().map(|(p, v)| (*p, (*v, Shade::Unread))).collect::<std::collections::BTreeMap<_, _>>();
    let pairs =
        [((0, 0), f64::INFINITY), ((0, 1), -1e308), ((1, 0), 1e308), ((1, 1), -1e308), ((2, 0), 1e308), ((3, 0), 2.0), ((3, 1), 1.0)];
    let bar_of = [Some(0), Some(1), Some(1), Some(2)];
    let (key, stretches, _, _, whole) = stack_bars(&cells(&pairs), &bar_of, 3, names(), false, true);
    assert_eq!(key.len(), 2);
    assert_eq!(stretches, vec![Vec::new(), Vec::new(), vec![2.0, 1.0]]);
    assert_eq!(whole, 2);
    // Each stretch within the range, x and y above zero adding up past it once stacked, the
    // bar's total (z below zero) still within it.
    let names = vec![Some("x".to_string()), Some("y".to_string()), Some("z".to_string())];
    let pairs = [((0, 0), 1e308), ((0, 1), 1e308), ((0, 2), -1e308)];
    let (_, stretches, _, _, whole) = stack_bars(&cells(&pairs), &[Some(0)], 1, names, false, true);
    assert_eq!((stretches, whole), (vec![Vec::new()], 1));
    // Its values' totals past the range too, x and y order as 0, after z's 1.
    let names = vec![Some("x".to_string()), Some("y".to_string()), Some("z".to_string())];
    let pairs = [((0, 0), 1e308), ((0, 1), 1e308), ((1, 0), 1e308), ((1, 1), 1e308), ((1, 2), 1.0)];
    let (key, ..) = stack_bars(&cells(&pairs), &[Some(0), Some(1)], 2, names, false, false);
    assert_eq!(key.first().map(|k| k.0.as_str()), Some("z"));
}

/// Past the palette's colours, the key keeps all but the last of the values with the largest
/// totals and gathers the rest in a grey Other.
#[test]
fn a_stack_key_past_its_colours_gathers_an_other() {
    use rustgis_data::{Dataset, Field, FieldType, Value};
    let mut ds = Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Integer), Field::new("people", FieldType::Double)],
    );
    for k in 0..10 {
        ds.push(None, vec![Value::Text("A".into()), Value::Int(k), Value::Float(10.0 - k as f64)]);
    }
    let d = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind"})).chart_data(&ds).unwrap();
    assert_eq!(key(&d), vec!["0", "1", "2", "3", "4", "5", "6", CHART_OTHER]);
    assert_eq!(d.key.len(), CHART_STACKS);
    assert_eq!(d.key.last().map(|k| k.1), Some(crate::symbology::OTHER_FILL));
    assert_eq!(d.stretches, vec![vec![10.0, 9.0, 8.0, 7.0, 6.0, 5.0, 4.0, 6.0]]);
    assert_eq!(d.key_gathered, 3);
    assert_eq!(
        chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "bars": 1}))
            .chart_data(&kinds())
            .unwrap()
            .key_gathered,
        0
    );
    // By label, the key still keeps the largest values, ordered A to Z once kept.
    let mut ds = Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Integer), Field::new("people", FieldType::Double)],
    );
    for k in 0..10 {
        ds.push(None, vec![Value::Text("A".into()), Value::Int(k), Value::Float(k as f64 + 1.0)]);
    }
    let d = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "order": "category"})).chart_data(&ds).unwrap();
    assert_eq!(key(&d), vec!["3", "4", "5", "6", "7", "8", "9", CHART_OTHER]);
    assert_eq!(d.stretches, vec![vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 6.0]]);
}

/// Stacked and coloured by classes, each value of the key takes the colour all its features on
/// the bars shown share on the map; one whose features have several, or show none, keeps its
/// palette colour and is counted. The bars hold no colours of their own.
#[test]
fn stacked_bars_colour_their_key_by_classes() {
    let ds = kinds();
    let palette = crate::diagram::CATEGORY_COLORS;
    let kind = chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "classes": true}));
    let ki = ds.field_index("kind").unwrap();
    let (red, blue, green) = (Rgba::rgb(200, 0, 0), Rgba::rgb(0, 0, 200), Rgba::rgb(0, 200, 0));
    let by_kind = |row: usize| match ds.value(row, ki) {
        rustgis_data::Value::Text(t) if t == "x" => Some(red),
        rustgis_data::Value::Text(_) => Some(blue),
        _ => Some(green),
    };
    let d = kind.chart_data_with(&ds, Some(&by_kind)).unwrap();
    assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), vec![red, blue, green]);
    assert_eq!((d.key_kept, d.mixed, d.unshown), (0, 0, 0));
    assert!(d.colors.is_empty());
    // A colour per feature: x and y have several, the blank one feature's own.
    let by_row = |row: usize| Some(Rgba::rgb(row as u8, 0, 0));
    let d = kind.chart_data_with(&ds, Some(&by_row)).unwrap();
    assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), vec![palette[0], palette[1], Rgba::rgb(5, 0, 0)]);
    assert_eq!(d.key_kept, 2);
    // Drawn with nothing, every value keeps its palette colour.
    let d = kind.chart_data_with(&ds, Some(&|_| None)).unwrap();
    assert_eq!(d.key.iter().map(|k| k.1).collect::<Vec<_>>(), palette[..3].to_vec());
    assert_eq!(d.key_kept, 3);
    // A value's colour is read over the bars shown: x red on A's bar, blue on B's left out,
    // and mixed once B is gathered in an Other bar.
    use rustgis_data::{Dataset, Field, FieldType, Value};
    let mut ds = Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Text), Field::new("people", FieldType::Double)],
    );
    ds.push(None, vec![Value::Text("A".into()), Value::Text("x".into()), Value::Float(10.0)]);
    ds.push(None, vec![Value::Text("B".into()), Value::Text("x".into()), Value::Float(1.0)]);
    let by_region = |row: usize| Some(if row == 0 { red } else { blue });
    let one = |other: bool| {
        chart(serde_json::json!({"field": "people", "group": "region", "stack": "kind", "classes": true, "bars": 1, "other": other}))
            .chart_data_with(&ds, Some(&by_region))
            .unwrap()
    };
    let d = one(false);
    assert_eq!((d.key.iter().map(|k| k.1).collect::<Vec<_>>(), d.key_kept), (vec![red], 0));
    let d = one(true);
    assert_eq!((d.key.iter().map(|k| k.1).collect::<Vec<_>>(), d.key_kept), (vec![palette[0]], 1));
}

/// Ungrouped, a bar per feature with a number, largest first, labelled by its field; the
/// rest are counted, not drawn.
#[test]
fn an_ungrouped_chart_draws_a_bar_per_feature() {
    let d = chart(serde_json::json!({"field": "people", "label": "name"})).chart_data(&places()).unwrap();
    assert_eq!(bars(&d), [(10.0, "a"), (7.0, "d"), (4.0, "b"), (2.0, "c"), (1.0, "f"), (-3.0, "g")]);
    assert_eq!((d.labelled, d.left_out, d.no_number, d.title.as_str()), (true, 0, 1, "people"));
    // A label field the layer hasn't leaves the bars unlabelled; a field it hasn't, no chart.
    assert!(!chart(serde_json::json!({"field": "people", "label": "gone"})).chart_data(&places()).unwrap().labelled);
    assert!(chart(serde_json::json!({"field": "gone"})).chart_data(&places()).is_none());
    assert!(chart(serde_json::json!({"field": "people", "group": "gone"})).chart_data(&places()).is_none());
}

/// Grouped, a bar per value (blank ones together), the statistic over its features; a count
/// counts features with no number, the others leave them out and say how many.
#[test]
fn a_grouped_chart_draws_the_statistic_per_value() {
    let ds = places();
    let grouped = |stat: &str| chart(serde_json::json!({"field": "people", "group": "region", "statistic": stat})).chart_data(&ds).unwrap();
    let sum = grouped("sum");
    assert_eq!(bars(&sum), [(12.0, "N"), (7.0, "E"), (4.0, "S"), (1.0, CHART_BLANK), (-3.0, "W")]);
    assert_eq!((sum.labelled, sum.no_number, sum.title.as_str()), (true, 1, "Sum of people by region"));
    let count = grouped("count");
    assert_eq!(bars(&count), [(2.0, "N"), (2.0, "S"), (1.0, "E"), (1.0, CHART_BLANK), (1.0, "W")]);
    assert_eq!((count.no_number, count.title.as_str()), (0, "Count by region"));
    assert_eq!(bars(&grouped("mean")), [(7.0, "E"), (6.0, "N"), (4.0, "S"), (1.0, CHART_BLANK), (-3.0, "W")]);
    assert_eq!(bars(&grouped("min"))[0], (7.0, "E"));
    assert_eq!(bars(&grouped("max"))[0], (10.0, "N"));
    // A value whose features have no number has no bar.
    let mut only_null = places();
    only_null.features.truncate(1);
    let i = only_null.field_index("people").unwrap();
    only_null.features[0].attrs[i] = rustgis_data::Value::Null;
    let d = chart(serde_json::json!({"field": "people", "group": "region"})).chart_data(&only_null).unwrap();
    assert_eq!((d.bars.len(), d.no_number), (0, 1));
    // A value that reads "(blank)" is a bar of its own; a blank one joins the features with none.
    let mut blanks = places();
    let i = blanks.field_index("region").unwrap();
    blanks.features[0].attrs[i] = rustgis_data::Value::Text(CHART_BLANK.into());
    blanks.features[1].attrs[i] = rustgis_data::Value::Text("  ".into());
    let d = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "order": "layer"}))
        .chart_data(&blanks)
        .unwrap();
    assert_eq!(bars(&d), [(1.0, CHART_BLANK), (2.0, CHART_BLANK), (1.0, "N"), (1.0, "E"), (1.0, "S"), (1.0, "W")]);
}

/// The four orders; ties keep the layer's order.
#[test]
fn a_chart_orders_its_bars() {
    let ds = places();
    let ordered = |order: &str| {
        let d =
            chart(serde_json::json!({"field": "people", "group": "region", "statistic": "count", "order": order})).chart_data(&ds).unwrap();
        d.bars.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
    };
    assert_eq!(ordered("descending"), ["N", "S", "E", CHART_BLANK, "W"]);
    assert_eq!(ordered("ascending"), ["E", CHART_BLANK, "W", "N", "S"]);
    assert_eq!(ordered("category"), [CHART_BLANK, "E", "N", "S", "W"]);
    assert_eq!(ordered("layer"), ["N", "S", "E", CHART_BLANK, "W"]);
}

/// Past its limit, bars are left out and counted, or gathered in a last Other bar measured
/// over their features, whatever the order; a limit of 1 gathers them all.
#[test]
fn a_chart_past_its_limit_leaves_bars_out_or_gathers_them() {
    let ds = places();
    let cut = |extra: serde_json::Value| {
        let mut keys = serde_json::json!({"field": "people", "group": "region"});
        if let (Some(k), Some(e)) = (keys.as_object_mut(), extra.as_object()) {
            k.extend(e.clone());
        }
        chart(keys).chart_data(&ds).unwrap()
    };
    let left = cut(serde_json::json!({"bars": 2}));
    assert_eq!((bars(&left), left.left_out), (vec![(12.0, "N"), (7.0, "E")], 3));
    let other = cut(serde_json::json!({"bars": 3, "other": true}));
    assert_eq!((bars(&other), other.left_out), (vec![(12.0, "N"), (7.0, "E"), (2.0, CHART_OTHER)], 0));
    // Ascending, Other still comes last; its mean is over the features of the bars it gathers.
    let mean = cut(serde_json::json!({"bars": 2, "other": true, "order": "ascending", "statistic": "mean"}));
    assert_eq!(bars(&mean), [(-3.0, "W"), (4.8, CHART_OTHER)]);
    let all = cut(serde_json::json!({"bars": 1, "other": true, "statistic": "count"}));
    assert_eq!(bars(&all), [(7.0, CHART_OTHER)]);
    // Fewer bars than the limit: no Other bar.
    assert_eq!(cut(serde_json::json!({"bars": 9, "other": true})).bars.len(), 5);
    // Ungrouped, Other sums the features past the limit; a limit out of range is read clamped.
    // Unlabelled, the bars are labelled then, so the Other bar is named.
    let ungrouped = chart(serde_json::json!({"field": "people", "bars": 3, "other": true})).chart_data(&ds).unwrap();
    assert_eq!((bars(&ungrouped), ungrouped.labelled), (vec![(10.0, ""), (7.0, ""), (4.0, CHART_OTHER)], true));
    assert!(!chart(serde_json::json!({"field": "people", "bars": 3})).chart_data(&ds).unwrap().labelled);
    assert_eq!(cut(serde_json::json!({"bars": 0})).bars.len(), 1);
    let mut many = rustgis_data::Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![rustgis_data::Field::new("people", rustgis_data::FieldType::Double)],
    );
    for i in 0..150 {
        many.push(None, vec![rustgis_data::Value::Float(i as f64)]);
    }
    let d = chart(serde_json::json!({"field": "people", "bars": 5000})).chart_data(&many).unwrap();
    assert_eq!((d.bars.len(), d.left_out), (MAX_CHART_BARS, 50));
    // An Other bar past a number's range isn't drawn: the bars it would gather are left out.
    let mut huge = places();
    let i = huge.field_index("people").unwrap();
    for f in &mut huge.features {
        f.attrs[i] = rustgis_data::Value::Float(f64::MAX);
    }
    let d = chart(serde_json::json!({"field": "people", "bars": 2, "other": true})).chart_data(&huge).unwrap();
    assert_eq!((d.bars.len(), d.left_out, d.out_of_range), (1, 0, 6));
    // So is a grouped bar's: N sums two of them; a count of the same features is drawn.
    let grouped =
        |stat: &str| chart(serde_json::json!({"field": "people", "group": "region", "statistic": stat})).chart_data(&huge).unwrap();
    let sum = grouped("sum");
    assert_eq!((sum.bars.len(), sum.out_of_range), (3, 2));
    assert!(sum.bars.iter().all(|(v, n)| v.is_finite() && n != "N" && n != "S"));
    assert_eq!((grouped("count").bars.len(), grouped("count").out_of_range), (5, 0));
}

/// The options are left out at their defaults, so a chart saved before keeps its bytes, and
/// read back as written otherwise.
#[test]
fn a_charts_options_are_saved_only_when_set() {
    let plain = chart(serde_json::json!({"field": "people"}));
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::json!({"Chart": {"frame": 1, "layer": "t", "field": "people", "label": null}})
    );
    assert!(!plain.chart_uses_data_options());
    let set = chart(
        serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "order": "category", "bars": 5, "other": true}),
    );
    let back: ElementKind = serde_json::from_value(serde_json::to_value(&set).unwrap()).unwrap();
    assert_eq!(back, set);
    assert!(set.chart_uses_data_options());
    for one in [
        serde_json::json!({"group": "region"}),
        // Kept while ungrouped, which an older reader would save without.
        serde_json::json!({"statistic": "count"}),
        serde_json::json!({"order": "layer"}),
        serde_json::json!({"bars": 21}),
        serde_json::json!({"other": true}),
    ] {
        let mut keys = one.clone();
        keys["field"] = serde_json::json!("people");
        assert!(chart(keys).chart_uses_data_options(), "{one}");
    }
}

/// Its look options are left out at their defaults too, read back as written otherwise, and
/// each one alone tells an older reader it can't draw the chart.
#[test]
fn a_charts_look_is_saved_only_when_set() {
    let plain = chart(serde_json::json!({"field": "people"}));
    assert!(!plain.chart_uses_look_options());
    assert_eq!(plain.chart_look(), Some(ChartLook::default()));
    let set = chart(serde_json::json!({
        "field": "people", "orientation": "Vertical", "values": true, "decimals": 0, "title": "",
        "bar_color": [194, 24, 91, 255], "label_align": "Right"
    }));
    let back: ElementKind = serde_json::from_value(serde_json::to_value(&set).unwrap()).unwrap();
    assert_eq!(back, set);
    let look = set.chart_look().unwrap();
    assert_eq!((look.orientation, look.values, look.decimals), (Orientation::Vertical, true, Some(0)));
    assert_eq!((look.bar_color, look.label_align), (Rgba::rgb(194, 24, 91), TextAlign::Right));
    for one in [
        serde_json::json!({"orientation": "Vertical"}),
        serde_json::json!({"values": true}),
        serde_json::json!({"decimals": 2}),
        serde_json::json!({"title": ""}),
        serde_json::json!({"bar_color": [0, 0, 0, 255]}),
        serde_json::json!({"label_align": "Right"}),
    ] {
        let mut keys = one.clone();
        keys["field"] = serde_json::json!("people");
        let c = chart(keys);
        assert!(c.chart_uses_look_options() && !c.chart_uses_data_options(), "{one}");
    }
}

/// Its own title, none, or the title it is given; the data's title follows it.
#[test]
fn a_chart_takes_its_own_title_or_none() {
    let ds = places();
    let titled = |extra: serde_json::Value| {
        let mut keys = serde_json::json!({"field": "people", "group": "region", "statistic": "mean"});
        if let (Some(k), Some(e)) = (keys.as_object_mut(), extra.as_object()) {
            k.extend(e.clone());
        }
        chart(keys).chart_data(&ds).unwrap().title
    };
    assert_eq!(titled(serde_json::json!({})), "Mean of people by region");
    assert_eq!(titled(serde_json::json!({"title": "People"})), "People");
    assert_eq!(titled(serde_json::json!({"title": ""})), "");
    assert_eq!(ElementKind::text("x", 9.0, INK, false).chart_title(), None);
}

/// A value as written on its bar: thousands apart, up to two decimals with the zeros they
/// end in dropped, or a fixed number of them.
#[test]
fn a_charts_values_are_written_with_thousands_and_decimals() {
    for (v, decimals, written) in [
        (2095.0, None, "2,095"),
        (13.3, None, "13.3"),
        (1_349_524.0, None, "1,349,524"),
        (0.126, None, "0.13"),
        (0.5, None, "0.5"),
        (-1234.5, None, "-1,234.5"),
        (100.0, None, "100"),
        (-0.001, None, "0"),
        (6.6, Some(0), "7"),
        (6.0, Some(2), "6.00"),
        (-12345.678, Some(1), "-12,345.7"),
        (1.0, Some(9), "1.000000"),
    ] {
        assert_eq!(chart_value_text(v, decimals), written, "{v} {decimals:?}");
    }
}

/// Numbers group by their value, looked up without writing each one out, and values written
/// alike stay one bar: a whole number and its decimal spelling, a number and its text.
#[test]
fn a_chart_groups_numbers_by_what_they_read() {
    use rustgis_data::{Dataset, Field, FieldType, Value};
    let mut ds = Dataset::new(
        "t",
        rustgis_data::GeometryType::Point,
        vec![Field::new("code", FieldType::Double), Field::new("people", FieldType::Double)],
    );
    for code in [Value::Int(3), Value::Float(3.0), Value::Float(2.5), Value::Text("3".into()), Value::Float(2.5), Value::Bool(true)] {
        ds.push(None, vec![code, Value::Float(1.0)]);
    }
    let d = chart(serde_json::json!({"field": "people", "group": "code", "statistic": "count", "order": "layer"})).chart_data(&ds).unwrap();
    assert_eq!(bars(&d), [(3.0, "3"), (2.0, "2.5"), (1.0, "True")]);
}

/// A mean past what a sum can hold is still drawn: kept as the numbers come, not summed.
#[test]
fn a_charts_mean_stays_in_range_where_its_sum_doesnt() {
    let mut huge = places();
    let i = huge.field_index("people").unwrap();
    for f in &mut huge.features {
        f.attrs[i] = rustgis_data::Value::Float(f64::MAX);
    }
    let mean = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "mean"})).chart_data(&huge).unwrap();
    assert_eq!((mean.bars.len(), mean.out_of_range), (5, 0));
    assert!(mean.bars.iter().all(|(v, _)| *v == f64::MAX), "{:?}", mean.bars);
    // So is an Other bar's, gathered from such groups.
    let other = chart(serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "bars": 2, "other": true}))
        .chart_data(&huge)
        .unwrap();
    assert_eq!((bars(&other).last().copied(), other.out_of_range), (Some((f64::MAX, CHART_OTHER)), 0));
    // Gathered from groups of opposite signs, too: the largest number and twice its opposite.
    let mut mixed = places();
    mixed.features.clear();
    for (region, n) in [("A", f64::MAX), ("B", -f64::MAX), ("B", -f64::MAX)] {
        mixed.push(
            None,
            vec![rustgis_data::Value::Text("x".into()), rustgis_data::Value::Text(region.into()), rustgis_data::Value::Float(n)],
        );
    }
    let all =
        chart(serde_json::json!({"field": "people", "group": "region", "statistic": "mean", "order": "layer", "bars": 1, "other": true}))
            .chart_data(&mixed)
            .unwrap();
    let (v, name) = bars(&all)[0];
    assert!(name == CHART_OTHER && (v / (-f64::MAX / 3.0) - 1.0).abs() < 1e-9, "{:?}", bars(&all));
}

/// Under the balanced light (#1977) flat ground is drawn in the ramp's own colour, so a relief's
/// colour bar is that colour, whatever the sun's height; under the plain light, the ramp.
#[test]
fn a_balanced_reliefs_colour_bar_is_its_flat_ground() {
    use crate::Ramp;
    use crate::raster::{Colorizer, RasterLayer, hillshade};
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let gt = GeoTransform { origin_x: 0.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
    let r = Raster::new("dem", 10, 10, vec![(0..100).map(|v| v as f32).collect()], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32")
        .unwrap();
    for (altitude, balanced) in [(45.0, true), (20.0, true), (45.0, false)] {
        let mut rl = RasterLayer::from_path(r.clone(), "dem.tif");
        rl.style.renderer = RasterRenderer::ShadedRelief {
            band: 0,
            ramp: Some(Ramp::Terrain),
            azimuth: 315.0,
            altitude,
            z_factor: 1.0,
            shadow: 0.0,
            sky: 0.0,
            fill: 0.0,
            rock: None,
            ambient: None,
            balanced,
        };
        let Some(ColorScale::Ramp { lo, hi, colors }) = color_scale(&rl) else { panic!("a ramp") };
        let colorizer = Colorizer::new(&rl.style, &r, None);
        let flat = if balanced { hillshade(&[0.0; 9], 1.0, 1.0, 315.0, altitude, 1.0) } else { 255 };
        for (i, c) in colors.iter().enumerate() {
            let v = lo + (hi - lo) * i as f64 / (colors.len() - 1) as f64;
            assert_eq!(Some(c.0), colorizer.color(&[v as f32], Some(flat)), "{altitude}° balanced {balanced}: sample {i}");
        }
    }
}

#[test]
fn elements_are_arranged_in_the_drawing_order() {
    let mut l = Layout::new("L", 100.0, 100.0);
    let rect = || ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0, dash: None };
    let ids: Vec<u32> = (0..4).map(|i| l.add(format!("E{i}"), (10.0, 10.0, 50.0, 50.0), rect())).collect();
    let order = |l: &Layout| l.elements.iter().map(|e| e.id).collect::<Vec<_>>();
    // All four overlap: the one drawn last is the one a click picks.
    assert_eq!(l.hit(20.0, 20.0), Some(ids[3]));
    assert_eq!(l.arrange(ids[0], Arrange::Front), Some(3));
    assert_eq!(order(&l), [ids[1], ids[2], ids[3], ids[0]]);
    assert_eq!(l.hit(20.0, 20.0), Some(ids[0]), "brought to the front, it's on top");
    assert_eq!(l.arrange(ids[0], Arrange::Backward), Some(2));
    assert_eq!(l.arrange(ids[0], Arrange::Back), Some(0));
    assert_eq!(l.arrange(ids[0], Arrange::Forward), Some(1));
    assert_eq!(order(&l), [ids[1], ids[0], ids[2], ids[3]]);
    // At an end, a step that way stays put.
    assert_eq!(l.arrange(ids[3], Arrange::Forward), Some(3));
    assert_eq!(l.arrange(ids[1], Arrange::Backward), Some(0));
    assert_eq!(l.arrange(999, Arrange::Front), None);
    assert_eq!(l.draw_order(ids[0]), Some((1, 4)));
    assert_eq!(Arrange::from_name(" Back "), Some(Arrange::Back));
}

#[test]
fn lines_turn_to_any_angle_and_keep_their_length() {
    // Within half a degree: lines within a quarter millimetre of level or upright are made so.
    let near = |a: f64, b: f64| ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs() < 0.5;
    for head in LineHead::ALL {
        for angle in (-180..=180).step_by(5).map(f64::from).chain([0.1, 89.9, 179.95]) {
            let (r, dir, h) = line_at_angle((100.0, 80.0), 40.0, angle, head);
            // Centred where asked; Both and None stay; one head moves to keep pointing.
            assert!(((r.0 + r.2 / 2.0) - 100.0).abs() < 1e-9 && ((r.1 + r.3 / 2.0) - 80.0).abs() < 1e-9);
            assert_eq!(h == LineHead::None || h == LineHead::Both, head == LineHead::None || head == LineHead::Both);
            assert!((line_length(r, dir) - 40.0).abs() < 0.3, "{angle}°: {}", line_length(r, dir));
            // Read back as it was set: plain lines and two-headed ones have no way to point, so
            // either way along them will do.
            let back = line_angle(r, dir, h);
            let pointed = matches!(head, LineHead::End | LineHead::Start);
            assert!(near(back, angle) || (!pointed && near(back, angle + 180.0)), "{head:?} {angle}° came back {back}°");
        }
    }
    // Level and upright lines keep a box to pick them up by.
    assert_eq!(line_at_angle((50.0, 50.0), 30.0, 0.0, LineHead::End).0, (35.0, 47.0, 30.0, LINE_BOX_MM));
    assert_eq!(line_at_angle((50.0, 50.0), 30.0, 90.0, LineHead::End).1, LineDir::Vertical);
    // Pointing left or down puts the single head on the start.
    assert_eq!(line_at_angle((0.0, 0.0), 10.0, 180.0, LineHead::End).2, LineHead::Start);
    assert_eq!(line_at_angle((0.0, 0.0), 10.0, -90.0, LineHead::Start).2, LineHead::End);
    // Junk in: a usable line out.
    let (r, ..) = line_at_angle((0.0, 0.0), f64::NAN, f64::INFINITY, LineHead::End);
    assert!(r.2.is_finite() && r.3.is_finite() && r.2 > 0.0);
}

#[test]
fn elements_add_hit_and_remove_with_surrounds() {
    let mut l = Layout::new("Layout", 297.0, 210.0);
    let f = l.add(
        "Map Frame",
        (10.0, 10.0, 200.0, 150.0),
        ElementKind::MapFrame {
            map: "Map".into(),
            center: Coord::new(0.0, 0.0),
            scale: 1e8,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: INDICATOR_COLOR,
            extent_width: INDICATOR_WIDTH,
            border_color: FRAME_BORDER_COLOR,
            border_width: FRAME_BORDER_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    let n = l.add("North Arrow", (215.0, 10.0, 15.0, 20.0), ElementKind::north_arrow(f));
    let t = l.add("Title", (20.0, 165.0, 100.0, 12.0), ElementKind::text("World", 18.0, Rgba::rgb(0, 0, 0), true));
    assert_eq!(l.hit(50.0, 50.0), Some(f));
    assert_eq!(l.hit(220.0, 15.0), Some(n));
    assert_eq!(l.hit(290.0, 200.0), None);
    assert!(l.remove(f));
    assert!(l.element(n).is_none(), "the frame's north arrow goes with it");
    assert!(l.element(t).is_some());
}

/// A legend's text size (#2581): 9 points unless set, and left out then, so a layout saved
/// before keeps its bytes; one past its range is brought into it, and counted.
#[test]
fn a_legends_text_size_defaults_and_is_kept_in_range() {
    let page = |legend: &str| {
        format!(
            r#"{{"name":"x","width":297,"height":210,"elements":[
                {{"id":1,"name":"f","x":0,"y":0,"w":100,"h":100,"kind":{{"MapFrame":{{"map":"M","center":{{"x":0,"y":0}},"scale":1e6}}}}}},
                {{"id":2,"name":"l","x":0,"y":0,"w":50,"h":50,"kind":{{"Legend":{legend}}}}}]}}"#
        )
    };
    let size = |l: &Layout| match &l.elements[1].kind {
        ElementKind::Legend { text_size, .. } => *text_size,
        _ => panic!("legend expected"),
    };
    let old: Layout = serde_json::from_str(&page(r#"{"frame":1,"title":"L"}"#)).unwrap();
    assert_eq!(size(&old), LEGEND_TEXT_PT);
    assert!(!old.elements[1].kind.has_custom_text_size());
    assert!(!serde_json::to_string(&old).unwrap().contains("text_size"), "the default isn't written");
    let mut big: Layout = serde_json::from_str(&page(r#"{"frame":1,"title":"L","text_size":1e9}"#)).unwrap();
    assert_eq!(big.clamp().elements_adjusted, 1);
    assert_eq!(size(&big), *LAYOUT_TEXT_SIZE.end());
    assert!(big.elements[1].kind.has_custom_text_size());
    let saved = serde_json::to_string(&big).unwrap();
    assert!(saved.contains(r#""text_size":100.0"#), "{saved}");
    assert_eq!(serde_json::from_str::<Layout>(&saved).unwrap(), big);
}

#[test]
fn scale_bar_and_grid_label_sizes_default_and_are_kept_in_range() {
    let page = |grid: &str, bar: &str| {
        format!(
            r#"{{"name":"x","width":297,"height":210,"elements":[
                {{"id":1,"name":"f","x":0,"y":0,"w":100,"h":100,"kind":{{"MapFrame":{{"map":"M","center":{{"x":0,"y":0}},"scale":1e6,"grid":{grid}}}}}}},
                {{"id":2,"name":"s","x":0,"y":0,"w":50,"h":10,"kind":{{"ScaleBar":{bar}}}}}]}}"#
        )
    };
    let sizes = |l: &Layout| match (&l.elements[0].kind, &l.elements[1].kind) {
        (ElementKind::MapFrame { grid: Some(g), .. }, ElementKind::ScaleBar { text_size, .. }) => (g.label_size, *text_size),
        _ => panic!("a frame with a grid and a scale bar expected"),
    };
    let old: Layout = serde_json::from_str(&page(r#"{"step":10,"color":[0,0,0,255]}"#, r#"{"frame":1}"#)).unwrap();
    assert_eq!(sizes(&old), (GRID_LABEL_PT, SCALE_BAR_TEXT_PT));
    assert!(old.elements.iter().all(|e| !e.kind.has_custom_text_size()));
    let text = serde_json::to_string(&old).unwrap();
    assert!(!text.contains("text_size") && !text.contains("label_size"), "the defaults aren't written: {text}");
    assert_eq!(ElementKind::scale_bar(1), old.elements[1].kind, "a new bar is the default one");
    // Out of range, or not a number a reader can draw at, each is brought in and counted.
    for (bad, (grid, bar)) in [("1e9", (100.0, 100.0)), ("-3", (4.0, 4.0)), ("0.5", (4.0, 4.0))] {
        let mut l: Layout = serde_json::from_str(&page(
            &format!(r#"{{"step":10,"color":[0,0,0,255],"label_size":{bad}}}"#),
            &format!(r#"{{"frame":1,"text_size":{bad}}}"#),
        ))
        .unwrap();
        assert_eq!(l.clamp().elements_adjusted, 2, "{bad}");
        assert_eq!(sizes(&l), (grid, bar), "{bad}");
        assert!(l.elements.iter().all(|e| e.kind.has_custom_text_size()), "{bad}");
        assert!(l.clamp().is_empty(), "{bad}: a repaired layout needs nothing more");
    }
    let mut sized: Layout =
        serde_json::from_str(&page(r#"{"step":10,"color":[0,0,0,255],"label_size":14}"#, r#"{"frame":1,"text_size":16}"#)).unwrap();
    assert!(sized.clamp().is_empty(), "sizes in range are left as they are");
    assert_eq!(sizes(&sized), (14.0, 16.0));
    let saved = serde_json::to_string(&sized).unwrap();
    assert!(saved.contains(r#""label_size":14.0"#) && saved.contains(r#""text_size":16.0"#), "{saved}");
    assert_eq!(serde_json::from_str::<Layout>(&saved).unwrap(), sized);
    // A non-finite size, as a program may hand it, falls back to the default.
    assert_eq!(layout_text_in_range(f64::NAN, SCALE_BAR_TEXT_PT), SCALE_BAR_TEXT_PT);
    assert_eq!(layout_text_in_range(f64::INFINITY, GRID_LABEL_PT), GRID_LABEL_PT);
}

#[test]
fn hostile_layouts_are_repaired() {
    let text = r#"{"name":"x","width":1e300,"height":-5,"elements":[
            {"id":3,"name":"a","x":0,"y":0,"w":1e9,"h":10,"kind":{"MapFrame":{"map":"M","center":{"x":1e300,"y":200},"scale":-1,"shape":{"Polygon":[[0,0],[1,1]]},"extent_of":3,"extent_width":1e9,"border_width":1e9}}},
            {"id":3,"name":"b","x":0,"y":0,"w":10,"h":10,"kind":{"Text":{"text":"t","size":1e9,"color":[0,0,0,255],"letter_spacing":-1e9}}},
            {"id":9,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{"Legend":{"frame":42,"title":"L"}}}],
            "series":{"frame":77,"layer":"x"}}"#;
    let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
    let repairs = l.clamp();
    assert_eq!(
        repairs,
        LayoutRepairs { elements_adjusted: 2, elements_cut: 0, elements_removed: 1, page_adjusted: 1, series_removed: 1, mgrs_off: 0 },
        "what changed is counted"
    );
    assert!(l.clamp().is_empty(), "a repaired layout needs nothing more");
    if let ElementKind::MapFrame { bearing, .. } = &mut l.elements[0].kind {
        *bearing = 270.0;
    }
    assert!(l.clamp().is_empty(), "a bearing of 270 is -90 by another name, not a repair");
    if let ElementKind::MapFrame { pitch, .. } = &mut l.elements[0].kind {
        *pitch = 1e-7;
    }
    assert!(l.clamp().is_empty(), "a pitch too small to tilt is flat, not a repair");
    let ids: Vec<u32> = l.elements.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 2, "the legend of a missing frame goes");
    assert_ne!(ids[0], ids[1], "duplicate ids are renumbered");
    assert!(l.width <= 5000.0 && l.height >= 20.0 && l.series.is_none());
    match &l.elements[0].kind {
        ElementKind::MapFrame { scale, shape, extent_of, center, extent_width, border_width, .. } => {
            assert!(*scale == MIN_FRAME_SCALE && *shape == FrameShape::Rectangle && extent_of.is_none() && center.is_finite());
            assert_eq!(*extent_width, MAX_INDICATOR_WIDTH);
            assert_eq!(*border_width, MAX_FRAME_BORDER_WIDTH, "a border past the widest is brought into range");
        }
        _ => panic!("frame expected"),
    }
    assert!(
        matches!(&l.elements[1].kind, ElementKind::Text { size, letter_spacing, .. } if *size <= 400.0 && *letter_spacing == MIN_LETTER_SPACING)
    );
    let id = l.add("n", (0.0, 0.0, 1.0, 1.0), ElementKind::north_arrow(ids[0]));
    assert!(!ids.contains(&id));
    // An id at u32::MAX is renumbered, and adding after it doesn't overflow.
    let mut big: Layout = serde_json::from_str(r#"{"name":"b","width":100,"height":100,"elements":[{"id":4294967295,"name":"t","x":0,"y":0,"w":5,"h":5,"kind":{"Text":{"text":"t","size":10,"color":[0,0,0,255]}}}]}"#).unwrap_or_else(|e| panic!("{e}"));
    big.clamp();
    assert!(big.elements[0].id <= MAX_ID);
    let a = big.add(
        "a",
        (0.0, 0.0, 1.0, 1.0),
        ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 1.0, radius: 0.0, dash: None },
    );
    assert!(a != big.elements[0].id && a <= MAX_ID + 1);
}

#[test]
fn hairline_rectangles_keep_their_height() {
    let mut l = Layout::new("Layout", 210.0, 297.0);
    let rule = ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0, dash: None };
    let id = l.add("Rule", (14.0, 33.0, 182.0, 0.15), rule.clone());
    assert_eq!(l.element(id).map(|e| e.h), Some(0.15));
    // A zero height becomes the thinnest element, not a 1 mm box.
    let zero = l.add("Zero", (14.0, 40.0, 182.0, 0.0), rule);
    assert_eq!(l.element(zero).map(|e| e.h), Some(MIN_ELEMENT_MM));
    // So does a loaded one.
    let text = r#"{"name":"x","width":210,"height":297,"elements":[
            {"id":1,"name":"a","x":14,"y":33,"w":182,"h":0.15,"kind":{"Rectangle":{"fill":[0,0,0,255],"outline":[0,0,0,0],"width":0}}},
            {"id":2,"name":"b","x":14,"y":40,"w":182,"h":0,"kind":{"Rectangle":{"fill":[0,0,0,0],"outline":[0,0,0,255],"width":0.4}}}]}"#;
    let mut l: Layout = serde_json::from_str(text).unwrap();
    l.clamp();
    assert_eq!(l.element(1).map(|e| e.h), Some(0.15));
    assert_eq!(l.element(2).map(|e| e.h), Some(MIN_ELEMENT_MM));
}

#[test]
fn frame_camera_keeps_paper_scale() {
    let map = Map::new("Map");
    let cam = frame_camera(&map, Coord::new(0.0, 0.0), 1_000_000.0, 0.0, 0.0);
    // At the equator one logical point of paper (0.2646 mm) covers 264.6 m at 1:1,000,000.
    assert!((cam.res - 264.583).abs() < 0.5, "{}", cam.res);
    let (c, s) = fit_scale(&map, &rustgis_geom::Extent::new(-10.0, -10.0, 10.0, 10.0), 200.0, 200.0).unwrap_or_default();
    assert!(c.x.abs() < 1e-6 && s > 1e7 && s < 2e7, "{c:?} {s}");
    // A finds drawing at 2:1 on the equator, where a frame's camera is closest (#902).
    let cam = frame_camera(&map, Coord::new(0.0, 0.0), MIN_FRAME_SCALE, 0.0, 0.0);
    assert!((cam.res / (MIN_FRAME_SCALE * PAPER_M_PER_PT) - 1.0).abs() < 1e-9, "{}", cam.res);
    // A 2 cm find fitted into a 100 mm frame would print at about 4.5:1: held at 2:1.
    let find = rustgis_geom::Extent::new(0.0, 0.0, 0.02 / 111_320.0, 0.02 / 111_320.0);
    assert_eq!(fit_scale(&map, &find, 100.0, 100.0).map(|(_, s)| s), Some(MIN_FRAME_SCALE));
}

#[test]
fn automatic_scale_bars_count_in_what_the_plan_shows() {
    assert_eq!(ScaleUnits::Auto.resolve(5_000.0), (1000.0, "km"));
    assert_eq!(ScaleUnits::Auto.resolve(20.0), (1.0, "m"));
    assert_eq!(ScaleUnits::Auto.resolve(1.0), (1.0, "m"));
    assert_eq!(ScaleUnits::Auto.resolve(0.05), (0.01, "cm"));
    assert_eq!(ScaleUnits::Auto.resolve(0.005), (0.001, "mm"));
    assert_eq!(ScaleUnits::Meters.resolve(0.005), (1.0, "m"), "a chosen unit stays");
}

/// A frame centred on a pole keeps that centre, and is drawn centred there on a polar map;
/// Web Mercator, whose poles are at infinity, draws it at 85° as frames always were, and
/// [`frame_center_placed`] tells it can't place it (#2684).
#[test]
fn a_frame_centred_on_a_pole_is_drawn_there_on_a_polar_map() {
    let mut l = Layout::new("Polo", 150.0, 150.0);
    let frame: ElementKind =
        serde_json::from_value(serde_json::json!({"MapFrame":{"map":"Mapa", "center":{"x":0,"y":-90}, "scale":60000000.0}})).unwrap();
    let id = l.add("Mapa", (5.0, 5.0, 140.0, 140.0), frame);
    l.clamp();
    let center = |l: &Layout| match l.element(id).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { center, .. }) => *center,
        _ => panic!("no frame"),
    };
    assert_eq!(center(&l).y, -90.0, "the pole kept");
    // On a south polar stereographic map, the pole is the frame's centre.
    let mut polar = Map::new("Mapa");
    polar.set_projection(crate::Projection::from_code("EPSG:3031").unwrap());
    let south = Coord::new(0.0, -90.0);
    assert!(frame_center_placed(&polar, south));
    let cam = frame_camera(&polar, south, 60_000_000.0, 0.0, 0.0);
    let pole = polar.to_map(south).unwrap();
    assert!((cam.center.x - pole.x).abs() < 1e-3 && (cam.center.y - pole.y).abs() < 1e-3, "{:?} {pole:?}", cam.center);
    // On Web Mercator it can't be: drawn at 85° S.
    let merc = Map::new("Mapa");
    assert!(!frame_center_placed(&merc, south) && frame_center_placed(&merc, Coord::new(0.0, -85.0)));
    // A longitude past ±180° (the sanitizer keeps up to ±540°) is placed: only the latitude counts.
    assert!(frame_center_placed(&merc, Coord::new(300.0, 40.0)) && frame_center_placed(&merc, Coord::new(-500.0, -60.0)));
    // And drawn there: the same meridian a turn of the globe nearer, not the view's edge at 270°.
    for (lon, lat) in [(300.0, 40.0), (-500.0, -60.0)] {
        let at = merc.to_lonlat(frame_camera(&merc, Coord::new(lon, lat), 60_000_000.0, 0.0, 0.0).center).unwrap();
        let turns = (at.x - lon).rem_euclid(360.0);
        assert!((turns < 1e-6 || turns > 360.0 - 1e-6) && (at.y - lat).abs() < 1e-6, "({lon}, {lat}) drawn at {at:?}");
    }
    let cam = frame_camera(&merc, south, 60_000_000.0, 0.0, 0.0);
    let at = merc.to_lonlat(cam.center).unwrap();
    assert!((at.y + 85.0).abs() < 1e-6, "{at:?}");
}

#[test]
fn frame_camera_follows_the_frame_on_the_globe() {
    let mut map = Map::new("Map");
    map.projection = crate::Projection::Globe;
    let near = frame_camera(&map, Coord::new(10.0, 20.0), 60_000_000.0, 0.0, 0.0);
    let far = frame_camera(&map, Coord::new(10.0, 20.0), 300_000_000.0, 0.0, 0.0);
    assert!((far.res / near.res - 5.0).abs() < 1e-6, "{} {}", near.res, far.res);
    let c = map.to_lonlat(near.center).unwrap_or_default();
    assert!((c.x - 10.0).abs() < 1e-6 && (c.y - 20.0).abs() < 1e-6, "{c:?}");
    // The globe's ground scale at the centre is the frame's paper scale.
    let ground = near.res * 20f64.to_radians().cos();
    assert!((ground - 60_000_000.0 * PAPER_M_PER_PT).abs() < 1e-3, "{ground}");
}

#[test]
fn repeated_ids_keep_references_on_the_first_element() {
    let text = r#"{"name":"x","width":200,"height":100,"elements":[
            {"id":1,"name":"frame","x":0,"y":0,"w":100,"h":80,"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1e7}}},
            {"id":1,"name":"box","x":0,"y":0,"w":10,"h":10,"kind":{"Rectangle":{"fill":[0,0,0,255],"outline":[0,0,0,255],"width":1}}},
            {"id":2,"name":"arrow","x":110,"y":0,"w":10,"h":10,"kind":{"NorthArrow":{"frame":1}}}]}"#;
    let mut l: Layout = serde_json::from_str(text).unwrap();
    l.clamp();
    assert_eq!(l.element(1).map(|e| e.name.as_str()), Some("frame"));
    let ids: std::collections::HashSet<u32> = l.elements.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 3, "the box got a fresh id");
    assert!(matches!(l.element(2).map(|e| &e.kind), Some(ElementKind::NorthArrow { frame: 1, .. })));
}

#[test]
fn loaded_guides_are_limited() {
    let mut l = Layout::new("Layout", 297.0, 210.0);
    l.guides.x = (0..200).map(f64::from).collect();
    l.guides.y = vec![f64::NAN, 1e9, -20.0, f64::INFINITY, 50.0];
    l.clamp();
    assert_eq!(l.guides.x.len(), MAX_GUIDES);
    assert_eq!(l.guides.y, vec![-20.0, 50.0]);
}

/// A measured grid's labels: whole values, or the kilometres' last two digits; a graticule
/// saved before measured grids is written as it was; the UTM zone at a point.
#[test]
fn measured_grids_label_eastings_and_northings() {
    let g = Grid { kind: GridKind::Measured, ..Grid::default() };
    assert_eq!(g.measured_label(543_000.0, 1000.0), "543000");
    assert_eq!(g.measured_label(5_262_000.0, 1000.0), "5262000");
    assert_eq!(g.measured_label(2_500.5, 0.5), "2500.5");
    assert_eq!(g.measured_label(-1_000.0, 1000.0), "-1000");
    // Off the step by rounding error from the stepped loop: back on it.
    assert_eq!(g.measured_label(543_000.000_000_1, 1000.0), "543000");
    assert_eq!(g.measured_label(-0.000_000_1, 1000.0), "0");
    let short = Grid { short: true, ..g.clone() };
    assert_eq!(short.measured_label(543_000.0, 1000.0), "43");
    assert_eq!(short.measured_label(5_262_000.0, 1000.0), "62");
    assert_eq!(short.measured_label(5_200_000.0, 1000.0), "0");
    assert_eq!(short.measured_label(543_500.0, 500.0), "43.5");
    assert_eq!(short.measured_label(-1_000.0, 1000.0), "99");
    // The UTM zone (WGS 84) at a point, north and south of the Equator.
    assert_eq!(utm_epsg(8.73, 47.5), 32632);
    assert_eq!(utm_epsg(-180.0, 10.0), 32601);
    assert_eq!(utm_epsg(179.9, -10.0), 32760);
    assert_eq!(utm_epsg(180.0, -10.0), 32701, "the antimeridian is -180°, zone 1's west edge");
    assert_eq!(utm_epsg(151.2, -33.9), 32756);
    assert_eq!(utm_epsg(f64::NAN, 0.0), 32631);
    assert_eq!(utm_epsg(190.0, 10.0), 32602, "a world copy east of the antimeridian");
    assert_eq!(short.measured_label(500_000.0, 100_000.0), "500000", "too wide a step for short labels");
    // A graticule saved as it always was; a measured grid keeps its type, system and labels.
    let saved = serde_json::to_value(Grid::default()).unwrap();
    for k in ["kind", "crs", "short"] {
        assert!(saved.get(k).is_none(), "{k} written for a graticule");
    }
    let m = Grid { kind: GridKind::Measured, crs: Some("EPSG:25832".into()), short: true, step: 1000.0, ..Grid::default() };
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!((v["kind"].as_str(), v["crs"].as_str(), v["short"].as_bool()), (Some("measured"), Some("EPSG:25832"), Some(true)));
    assert_eq!(serde_json::from_value::<Grid>(v).unwrap(), m);
    assert_eq!(GridKind::from_name(" Measured "), Some(GridKind::Measured));
    assert_eq!(GridKind::from_name("utm"), None);
}

/// MGRS labels: the kilometres' last two digits on the lines, saved only when on; a WGS 84 UTM
/// zone is read from its EPSG code alone; a file with them on a graticule or naming another
/// system has them left off, counted on their own.
#[test]
fn mgrs_labels_need_a_wgs84_utm_zone() {
    let g = Grid { kind: GridKind::Measured, mgrs: true, ..Grid::default() };
    assert_eq!(g.measured_label(543_000.0, 1000.0), "43");
    assert_eq!(g.measured_label(5_262_500.0, 500.0), "62.5");
    assert_eq!(g.measured_label(500_000.0, 1000.0), "00", "two digits, as MGRS prints them");
    assert_eq!(g.measured_label(505_500.0, 500.0), "05.5");
    assert_eq!(Grid { mgrs: false, short: true, ..g.clone() }.measured_label(505_000.0, 1000.0), "5");
    assert_eq!(g.measured_label(500_000.0, 100_000.0), "500000");
    assert!(serde_json::to_value(Grid::default()).unwrap().get("mgrs").is_none());
    let v = serde_json::to_value(&g).unwrap();
    assert_eq!(v["mgrs"].as_bool(), Some(true));
    assert_eq!(serde_json::from_value::<Grid>(v).unwrap(), g);
    for (crs, zone) in [
        ("EPSG:32632", Some((32, true))),
        (" epsg:32760 ", Some((60, false))),
        ("32601", Some((1, true))),
        ("EPSG: 32733", Some((33, false))),
        ("epsg 32632", Some((32, true))),
        (" EPSG 32733 ", Some((33, false))),
        ("EPSG:32600", None),
        ("EPSG:32661", None),
        ("EPSG:32761", None),
        ("EPSG:25832", None),
        ("EPSG:2056", None),
        ("EPSG:326320", None),
        ("+proj=utm +zone=32 +datum=WGS84", None),
        ("", None),
    ] {
        assert_eq!(utm_zone_of(crs), zone, "{crs}");
    }
    let frame = |crs: Option<&str>, kind: GridKind| {
        let mut l = Layout::new("Layout", 297.0, 210.0);
        l.add(
            "Map Frame",
            (10.0, 10.0, 200.0, 150.0),
            ElementKind::MapFrame {
                map: "Map".into(),
                center: Coord::new(8.7, 47.5),
                scale: 25_000.0,
                shape: FrameShape::Rectangle,
                grid: Some(Grid { crs: crs.map(Into::into), kind, ..g.clone() }),
                extent_of: None,
                extent_color: INDICATOR_COLOR,
                extent_width: INDICATOR_WIDTH,
                border_color: FRAME_BORDER_COLOR,
                border_width: FRAME_BORDER_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        let r = l.clamp();
        let ElementKind::MapFrame { grid: Some(g), .. } = &l.elements[0].kind else { panic!() };
        (g.mgrs, r.mgrs_off, r.elements_adjusted)
    };
    assert_eq!(frame(None, GridKind::Measured), (true, 0, 0));
    assert_eq!(frame(Some("EPSG:32632"), GridKind::Measured), (true, 0, 0));
    assert_eq!(frame(Some(" "), GridKind::Measured), (true, 0, 0));
    assert_eq!(frame(Some("EPSG:2056"), GridKind::Measured), (false, 1, 0), "said on its own, not as a value out of range");
    assert_eq!(frame(None, GridKind::Graticule), (false, 1, 0));
}

/// A chart's maximum not above 0, which only a hand-edited file holds and the chart can't
/// draw to, is let go on read (the chart on its bars' own scale) and counted as a value out of
/// range, so the project says so (#2487 part 4); one above 0 is kept.
#[test]
fn a_charts_maximum_not_above_zero_is_repaired_and_counted() {
    for (max, kept, adjusted) in [(-5.0, None, 1), (0.0, None, 1), (40.0, Some(40.0), 0)] {
        let mut l: Layout = serde_json::from_value(serde_json::json!({
            "name": "L", "width": 120, "height": 90, "elements": [
                {"id": 1, "name": "Frame", "x": 10, "y": 10, "w": 60, "h": 70,
                 "kind": {"MapFrame": {"map": "Map", "center": {"x": 0, "y": 0}, "scale": 1000000}}},
                {"id": 2, "name": "Chart", "x": 75, "y": 10, "w": 35, "h": 40,
                 "kind": {"Chart": {"frame": 1, "layer": "Places", "field": "people", "max": max}}}
            ]
        }))
        .unwrap();
        let r = l.clamp();
        let ElementKind::Chart { max: m, .. } = &l.elements[1].kind else { panic!("a chart") };
        assert_eq!((*m, r.elements_adjusted), (kept, adjusted), "{max}");
    }
}

/// A bar outline's keys left out of a hand-edited file take those it is first given, as a
/// background's do, rather than leaving the layout unreadable (#2487 part 4b).
#[test]
fn a_charts_bar_outline_takes_its_defaults_for_keys_left_out() {
    let read = |outline: serde_json::Value| {
        let kind: ElementKind = serde_json::from_value(serde_json::json!(
            {"Chart": {"frame": 1, "layer": "Places", "field": "people", "bar_outline": outline}}
        ))
        .unwrap();
        let ElementKind::Chart { bar_outline, .. } = kind else { panic!("a chart") };
        bar_outline
    };
    let red = Rgba::rgb(0xD0, 0x10, 0x10);
    assert_eq!(read(serde_json::json!({"width": 2.0})), Some(ChartOutline { width: 2.0, ..CHART_OUTLINE }));
    assert_eq!(read(serde_json::json!({"color": [208, 16, 16, 255]})), Some(ChartOutline { color: red, ..CHART_OUTLINE }));
    assert_eq!(read(serde_json::json!({})), Some(CHART_OUTLINE));
}

/// A bar outline's width out of range, which only a hand-edited file holds, is repaired on
/// read and counted as a value out of range, so the project says so (#2487 part 4b): past 5
/// points it is held to 5, below 0 the outline is let go. A width of 0 draws none, as the
/// commands take it, so it is let go without a word; one in range is kept.
#[test]
fn a_charts_bar_outline_out_of_range_is_repaired_and_counted() {
    let ink = Rgba::rgb(0x22, 0x22, 0x22);
    for (width, kept, adjusted) in [(8.0, Some(5.0), 1), (-1.0, None, 1), (0.0, None, 0), (2.0, Some(2.0), 0)] {
        let mut l: Layout = serde_json::from_value(serde_json::json!({
            "name": "L", "width": 120, "height": 90, "elements": [
                {"id": 1, "name": "Frame", "x": 10, "y": 10, "w": 60, "h": 70,
                 "kind": {"MapFrame": {"map": "Map", "center": {"x": 0, "y": 0}, "scale": 1000000}}},
                {"id": 2, "name": "Chart", "x": 75, "y": 10, "w": 35, "h": 40,
                 "kind": {"Chart": {"frame": 1, "layer": "Places", "field": "people",
                                    "bar_outline": {"color": [34, 34, 34, 255], "width": width}}}}
            ]
        }))
        .unwrap();
        let r = l.clamp();
        let ElementKind::Chart { bar_outline, .. } = &l.elements[1].kind else { panic!("a chart") };
        let expected = kept.map(|w| ChartOutline { color: ink, width: w });
        assert_eq!((*bar_outline, r.elements_adjusted), (expected, adjusted), "{width}");
    }
}

/// A page colour with alpha, which only a hand-edited file holds, is read as it looks on white
/// paper (a page has nothing else under it), so a clear one is the white page, and every one
/// is counted as a page repaired (#2855). An opaque one is kept, and opaque white is the page's
/// own, so it is let go without a word.
#[test]
fn a_translucent_page_colour_is_made_opaque_and_counted() {
    for (paper, kept, adjusted) in [
        (serde_json::json!([11, 27, 43, 128]), Some(Rgba::rgb(133, 141, 149)), 1),
        (serde_json::json!([11, 27, 43, 0]), None, 1),
        (serde_json::json!([11, 27, 43, 255]), Some(Rgba::rgb(11, 27, 43)), 0),
        (serde_json::json!([255, 255, 255, 255]), None, 0),
        (serde_json::json!([255, 255, 255, 128]), None, 1),
    ] {
        let mut l: Layout =
            serde_json::from_value(serde_json::json!({"name": "L", "width": 120, "height": 90, "elements": [], "paper": paper})).unwrap();
        let r = l.clamp();
        assert_eq!((l.paper, r.page_adjusted), (kept, adjusted), "{paper}");
    }
}

#[test]
fn grid_labels_follow_the_format() {
    let g = Grid::default();
    // #210: the Equator and prime meridian are 0°, the antimeridian 180°, with no letter.
    assert_eq!(g.label(0.0, false, 10.0), "0°");
    assert_eq!(g.label(-0.0, false, 10.0), "0°");
    assert_eq!(g.label(0.0, true, 10.0), "0°");
    assert_eq!(g.label(180.0, true, 10.0), "180°");
    assert_eq!(g.label(-180.0, true, 10.0), "180°");
    assert_eq!(g.label(10.0, false, 10.0), "10°N");
    assert_eq!(g.label(-10.0, false, 10.0), "10°S");
    assert_eq!(g.label(20.0, true, 10.0), "20°E");
    assert_eq!(g.label(-20.0, true, 10.0), "20°W");
    // As many decimals as the step needs.
    assert_eq!(g.label(0.5, false, 0.25), "0.50°N");
    assert_eq!(g.label(-112.5, true, 0.5), "112.5°W");
    // Accumulated decimal steps miss 0 and 180 by rounding error.
    let mut v = -0.5;
    for _ in 0..5 {
        v += 0.1;
    }
    assert_ne!(v, 0.0);
    assert_eq!(g.label(v, false, 0.1), "0.0°");
    let mut v = 179.5;
    for _ in 0..5 {
        v += 0.1;
    }
    assert_eq!(g.label(v, true, 0.1), "180.0°");
    // Signed rather than lettered.
    let signed = Grid { hemisphere: false, ..Grid::default() };
    assert_eq!(signed.label(-112.5, true, 0.5), "-112.5°");
    assert_eq!(signed.label(45.0, false, 15.0), "45°");
    assert_eq!(signed.label(0.0, false, 15.0), "0°");
    // Degrees, minutes and seconds, leaving out what the step doesn't need.
    let dms = Grid { format: CoordFormat::Dms, ..Grid::default() };
    assert_eq!(dms.label(30.0, false, 5.0), "30°N");
    assert_eq!(dms.label(-112.5, true, 0.5), "112°30′W");
    assert_eq!(dms.label(-112.0, true, 0.5), "112°00′W");
    assert_eq!(dms.label(12.0 + 1.0 / 120.0, false, 1.0 / 120.0), "12°00′30″N");
    // Rounding up a second carries into the minutes and degrees.
    assert_eq!(dms.label(12.999_999_9, false, 0.25), "13°00′N");
}

#[test]
fn surrounds_saved_before_their_options_open_as_they_were() {
    let arrow: ElementKind = serde_json::from_str(r#"{"NorthArrow":{"frame":3}}"#).unwrap();
    assert_eq!(arrow, ElementKind::north_arrow(3));
    assert!(matches!(arrow, ElementKind::NorthArrow { style: ArrowStyle::Classic, .. }));
    let bar: ElementKind = serde_json::from_str(r#"{"ScaleBar":{"frame":3}}"#).unwrap();
    assert_eq!(bar, ElementKind::scale_bar(3));
    assert!(matches!(bar, ElementKind::ScaleBar { units: ScaleUnits::Auto, divisions: 4, style: ScaleBarStyle::Alternating, .. }));
    assert!(matches!(bar, ElementKind::ScaleBar { color, fill, .. } if color == ink() && fill == white()));
    let grid: Grid = serde_json::from_str(r#"{"step":10,"color":[64,72,85,150],"labels":true}"#).unwrap();
    assert_eq!(grid, Grid { step: 10.0, ..Grid::default() });
    assert_eq!(grid.edges, GridEdges { top: false, bottom: true, left: true, right: false });
    assert_eq!(grid.style, GridStyle::Lines);
}

/// Lines and the outlines of rectangles and ellipses keep a dash pattern through a save
/// (#1763); solid, nothing is written, so a layout saved before is written back byte for
/// byte; a pattern read from a file is brought into range as a line layer's is.
#[test]
fn dashes_round_trip_and_old_layouts_keep_their_bytes() {
    let black = Rgba::rgb(0, 0, 0);
    let mut l = Layout::new("L", 100.0, 100.0);
    let line = l.add(
        "Line",
        (10.0, 10.0, 60.0, 6.0),
        ElementKind::Line { color: black, width: 1.0, dir: LineDir::Horizontal, head: LineHead::End, dash: None },
    );
    let rect =
        l.add("Box", (10.0, 30.0, 60.0, 30.0), ElementKind::Rectangle { fill: black, outline: black, width: 1.0, radius: 0.0, dash: None });
    let oval = l.add("Oval", (10.0, 70.0, 30.0, 20.0), ElementKind::Ellipse { fill: black, outline: black, width: 1.0, dash: None });
    let old = serde_json::to_string(&l).unwrap();
    assert!(!old.contains("dash"), "solid writes no pattern");
    assert_eq!(serde_json::to_string(&serde_json::from_str::<Layout>(&old).unwrap()).unwrap(), old, "written back byte for byte");
    let dash_of = |l: &Layout, id: u32| match l.element(id).map(|e| &e.kind) {
        Some(ElementKind::Line { dash, .. } | ElementKind::Rectangle { dash, .. } | ElementKind::Ellipse { dash, .. }) => {
            dash.map(|d| d.steps().to_vec())
        }
        _ => panic!("a line or a shape"),
    };
    let pattern = Dash::new(&[4.0, 2.0, 1.0, 2.0]);
    for id in [line, rect, oval] {
        if let Some(ElementKind::Line { dash, .. } | ElementKind::Rectangle { dash, .. } | ElementKind::Ellipse { dash, .. }) =
            l.element_mut(id).map(|e| &mut e.kind)
        {
            *dash = pattern;
        }
    }
    let saved = serde_json::to_string(&l).unwrap();
    assert!(saved.contains(r#""dash":[4.0,2.0,1.0,2.0]"#), "{saved}");
    let back: Layout = serde_json::from_str(&saved).unwrap();
    for id in [line, rect, oval] {
        assert_eq!(dash_of(&back, id), Some(vec![4.0, 2.0, 1.0, 2.0]));
    }
    // From a file, as a line layer's pattern: steps clamped, all-zero read as solid.
    let file = saved.replacen("[4.0,2.0,1.0,2.0]", "[4,1e9,-2]", 1).replacen("[4.0,2.0,1.0,2.0]", "[0,0]", 1);
    let read: Layout = serde_json::from_str(&file).unwrap();
    assert_eq!(dash_of(&read, line), Some(vec![4.0, MAX_DASH_STEP, 0.0]));
    assert_eq!(dash_of(&read, rect), None);
    assert_eq!(dash_of(&read, oval), Some(vec![4.0, 2.0, 1.0, 2.0]));
}

/// A text turned 90° or -90° (#1763) keeps its turn through a save; one saved before reads
/// upright and is written back byte for byte; a turn read from a file is kept within -180 to
/// 180.
#[test]
fn text_rotation_round_trips_and_old_layouts_keep_their_bytes() {
    let turn_of = |l: &Layout, id: u32| l.element(id).map(Element::rotation);
    let mut l = Layout::new("L", 100.0, 100.0);
    let upright = l.add("Title", (10.0, 10.0, 60.0, 10.0), ElementKind::text("Brasil", 12.0, Rgba::rgb(0, 0, 0), false));
    let old = serde_json::to_string(&l).unwrap();
    assert!(!old.contains("rotation"), "an upright text writes no turn");
    let back: Layout = serde_json::from_str(&old).unwrap();
    assert_eq!(turn_of(&back, upright), Some(0.0));
    assert_eq!(serde_json::to_string(&back).unwrap(), old, "written back byte for byte");
    for turn in [90.0, -90.0] {
        let mut l = l.clone();
        if let Some(ElementKind::Text { rotation, .. }) = l.element_mut(upright).map(|e| &mut e.kind) {
            *rotation = turn;
        }
        let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(turn_of(&back, upright), Some(turn));
    }
    // Read from a file, a turn is kept within -180..=180.
    let file = old.replace(r#""bold":false"#, r#""bold":false,"rotation":450"#);
    let mut read: Layout = serde_json::from_str(&file).unwrap();
    read.clamp();
    assert_eq!(turn_of(&read, upright), Some(90.0));
    assert_eq!(
        [rotation(270.0), rotation(-270.0), rotation(180.0), rotation(-180.0), rotation(f64::NAN)],
        [-90.0, 90.0, 180.0, 180.0, 0.0]
    );
}

/// A turned text is hit, and takes up the page, where it is drawn: its box turned about its
/// centre (#1763).
#[test]
fn a_turned_text_is_hit_on_its_turned_box() {
    let mut l = Layout::new("L", 100.0, 100.0);
    let id = l.add("Source", (20.0, 45.0, 60.0, 10.0), ElementKind::text("Fonte: IBGE", 8.0, Rgba::rgb(0, 0, 0), false));
    // Upright: 60 wide, 10 high, centred on (50, 50).
    assert_eq!((l.hit(25.0, 50.0), l.hit(50.0, 25.0)), (Some(id), None));
    if let Some(ElementKind::Text { rotation, .. }) = l.element_mut(id).map(|e| &mut e.kind) {
        *rotation = 90.0;
    }
    // Turned: 10 wide, 60 high, about the same centre.
    assert_eq!((l.hit(25.0, 50.0), l.hit(50.0, 25.0)), (None, Some(id)));
    let (x, y, w, h) = l.element(id).unwrap().bounds();
    assert!((x - 45.0).abs() < 1e-9 && (y - 20.0).abs() < 1e-9 && (w - 10.0).abs() < 1e-9 && (h - 60.0).abs() < 1e-9);
    // The element keeps its unturned box.
    let e = l.element(id).unwrap();
    assert_eq!((e.x, e.y, e.w, e.h), (20.0, 45.0, 60.0, 10.0));
}

#[test]
fn loaded_grids_and_scale_bars_are_limited() {
    let text = r#"{"name":"x","width":200,"height":100,"elements":[
            {"id":1,"name":"frame","x":0,"y":0,"w":100,"h":80,"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1e7,
                "grid":{"step":10,"color":[0,0,0,255],"tick_length":1e9,"width":-5}}}},
            {"id":2,"name":"bar","x":110,"y":0,"w":50,"h":10,"kind":{"ScaleBar":{"frame":1,"divisions":100000}}}]}"#;
    let mut l: Layout = serde_json::from_str(text).unwrap();
    l.clamp();
    let Some(ElementKind::MapFrame { grid: Some(g), .. }) = l.element(1).map(|e| &e.kind) else { panic!() };
    assert_eq!((g.tick_length, g.width), (MAX_TICK_MM, 0.0));
    assert!(matches!(l.element(2).map(|e| &e.kind), Some(ElementKind::ScaleBar { divisions: MAX_SCALE_DIVISIONS, .. })));
    // A step in degrees for a graticule, in the system's units for a measured grid: 1 km
    // kept, and limited as `layout.setGrid` limits it.
    let step = |kind: &str, step: f64| {
        let text = text.replace(r#""step":10"#, &format!(r#""step":{step},"kind":"{kind}""#));
        let mut l: Layout = serde_json::from_str(&text).unwrap();
        l.clamp();
        let Some(ElementKind::MapFrame { grid: Some(g), .. }) = l.element(1).map(|e| &e.kind) else { panic!() };
        g.step
    };
    assert_eq!((step("graticule", 1000.0), step("graticule", -1.0)), (MAX_GRATICULE_STEP, 0.0));
    assert_eq!((step("measured", 1000.0), step("measured", 1e12), step("measured", 0.25)), (1000.0, MAX_MEASURED_STEP, MIN_MEASURED_STEP));
    assert_eq!(step("measured", 0.0), 0.0, "Auto");
}

/// A background (#2257) is left out of a saved element that has none, so older layouts read and
/// write as before; read back, it keeps its fill, outline, corners and padding, and a hand-edited
/// one is brought into range, a map frame's dropped.
#[test]
fn element_backgrounds_are_saved_read_back_and_repaired() {
    let mut l = Layout::new("L", 100.0, 100.0);
    let t = l.add("T", (40.0, 40.0, 20.0, 10.0), ElementKind::text("t", 12.0, Rgba::rgb(0, 0, 0), false));
    let saved = serde_json::to_value(&l).unwrap();
    assert!(saved["elements"][0].get("background").is_none(), "none, not written: {saved}");
    let b = Background { fill: Rgba::new(255, 255, 255, 204), outline: Rgba::rgb(0, 0, 0), width: 0.5, radius: 1.0, padding: 1.5 };
    l.element_mut(t).unwrap().background = Some(b);
    let back: Layout = serde_json::from_value(serde_json::to_value(&l).unwrap()).unwrap();
    assert_eq!(back.element(t).unwrap().background, Some(b));
    // Only a fill given: no outline, square corners, no padding.
    let fill: Background = serde_json::from_str(r#"{"fill":[1,2,3,255]}"#).unwrap();
    assert_eq!(fill, Background::filled(Rgba::rgb(1, 2, 3)));
    // An empty one is white, as `layout.update` makes `{"background": {}}`, not a file that fails to open.
    let empty: Background = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, Background::filled(Rgba::rgb(255, 255, 255)));

    let text = r#"{"name":"x","width":100,"height":100,"elements":[
            {"id":1,"name":"f","x":0,"y":0,"w":50,"h":50,"background":{"fill":[255,0,0,255]},"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1000000}}},
            {"id":2,"name":"t","x":0,"y":0,"w":10,"h":10,"background":{"fill":[255,0,0,255],"width":1e9,"radius":-3,"padding":1e300},"kind":{"Text":{"text":"t","size":12,"color":[0,0,0,255]}}},
            {"id":3,"name":"u","x":0,"y":0,"w":10,"h":10,"background":{"fill":[255,0,0,255],"padding":2},"kind":{"Text":{"text":"u","size":12,"color":[0,0,0,255]}}}]}"#;
    let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
    let repairs = l.clamp();
    assert_eq!(repairs.elements_adjusted, 2, "the frame's background and the text's numbers: {repairs:?}");
    assert_eq!(l.element(1).unwrap().background, None, "a map frame has none");
    let b = l.element(2).unwrap().background.unwrap();
    assert_eq!((b.width, b.radius, b.padding), (MAX_BACKGROUND_WIDTH, 0.0, MAX_BACKGROUND_PADDING));
    assert_eq!(l.element(3).unwrap().background.unwrap().padding, 2.0, "in range, kept");
    assert!(l.clamp().is_empty(), "a repaired layout needs nothing more");
}

/// A click on an element's background picks it, padding included; without one, its box only.
#[test]
fn a_background_is_part_of_what_a_click_picks() {
    let mut l = Layout::new("L", 100.0, 100.0);
    let t = l.add("T", (40.0, 40.0, 20.0, 10.0), ElementKind::text("t", 12.0, Rgba::rgb(0, 0, 0), false));
    assert_eq!(l.hit(38.0, 45.0), None);
    l.element_mut(t).unwrap().background = Some(Background { padding: 3.0, ..Background::filled(Rgba::rgb(255, 255, 255)) });
    assert_eq!(l.hit(38.0, 45.0), Some(t));
    assert_eq!(l.hit(36.0, 45.0), None, "past the padding");
    assert_eq!(l.element(t).unwrap().background_box(), (37.0, 37.0, 26.0, 16.0));
}

/// A legend's, colorbar's, table's or chart's ink (#2257): none in a layout saved before it,
/// not written when none, and read back when set. Other kinds have none to set.
#[test]
fn inks_are_saved_and_read_back() {
    let text = r#"{"name":"x","width":100,"height":100,"elements":[
            {"id":1,"name":"f","x":0,"y":0,"w":50,"h":50,"kind":{"MapFrame":{"map":"M","center":{"x":0,"y":0},"scale":1000000}}},
            {"id":2,"name":"l","x":0,"y":0,"w":10,"h":10,"kind":{"Legend":{"frame":1,"title":"L"}}},
            {"id":3,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{"Colorbar":{"frame":1}}},
            {"id":4,"name":"t","x":0,"y":0,"w":10,"h":10,"kind":{"Table":{"frame":1,"layer":"A"}}},
            {"id":5,"name":"g","x":0,"y":0,"w":10,"h":10,"kind":{"Chart":{"frame":1,"layer":"A","field":"n"}}}]}"#;
    let mut l: Layout = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}"));
    assert!(l.elements[1..].iter().all(|e| e.kind.takes_ink() && e.kind.ink().is_none()), "the default ink in an older layout");
    assert!(!l.elements[0].kind.takes_ink(), "a map frame has no ink");
    let saved = serde_json::to_string(&l).unwrap();
    assert!(!saved.contains("\"color\""), "none is not written: {saved}");
    let white = Rgba::new(255, 255, 255, 230);
    for e in &mut l.elements[1..] {
        *e.kind.ink_mut().unwrap() = Some(white);
    }
    let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
    assert!(back.elements[1..].iter().all(|e| e.kind.ink() == Some(white)), "{back:?}");
    assert_eq!(back, l);
    let mut text = ElementKind::text("t", 12.0, Rgba::rgb(0, 0, 0), false);
    assert!(!text.takes_ink() && text.ink_mut().is_none(), "a text's colour is its own option");
}

/// A text's block alignment, line spacing and case: left out of the file at their defaults, so a
/// text saved before saves byte for byte; written and read back when set; and a hand-edited value
/// out of range, of the wrong kind or not a name loads, brought into range and counted.
#[test]
fn text_block_settings_save_only_when_set_and_load_into_range() {
    use serde_json::json;
    for plain in [
        r#"{"Text":{"text":"a","size":10.0,"color":[0,0,0,255],"bold":false}}"#,
        r#"{"Text":{"text":"a\nb","size":12.0,"color":[31,41,55,255],"bold":true,"italic":true,"align":"Center","letter_spacing":2.0,"font":"Serif","font_name":"Georgia","rotation":90.0,"wrap":true}}"#,
    ] {
        let k: ElementKind = serde_json::from_str(plain).unwrap();
        assert_eq!(serde_json::to_string(&k).unwrap(), plain);
        assert!(!k.text_uses_block());
    }
    let mut set = ElementKind::text("a", 10.0, Rgba::rgb(0, 0, 0), false);
    if let ElementKind::Text { valign, line_spacing, case, .. } = &mut set {
        (*valign, *line_spacing, *case) = (TextVAlign::Bottom, 1.5, TextCase::Upper);
    }
    let s = serde_json::to_string(&set).unwrap();
    assert!(s.ends_with(r#""bold":false,"valign":"bottom","line_spacing":1.5,"case":"upper"}}"#), "{s}");
    assert_eq!(serde_json::from_str::<ElementKind>(&s).unwrap(), set);
    assert!(set.text_uses_block());
    // Unclamped, a value that is not a name or not a finite number saves as the default does:
    // nothing, never "top" or a null spacing.
    let plain = ElementKind::text("a", 10.0, Rgba::rgb(0, 0, 0), false);
    let plain_json = serde_json::to_string(&plain).unwrap();
    for (v, ls, c) in [
        (TextVAlign::Invalid, f64::NAN, TextCase::Invalid),
        (TextVAlign::Invalid, LINE_SPACING, TextCase::None),
        (TextVAlign::Top, f64::NAN, TextCase::None),
        (TextVAlign::Top, f64::INFINITY, TextCase::None),
        (TextVAlign::Top, f64::NEG_INFINITY, TextCase::Invalid),
    ] {
        let mut odd = plain.clone();
        if let ElementKind::Text { valign, line_spacing, case, .. } = &mut odd {
            (*valign, *line_spacing, *case) = (v, ls, c);
        }
        assert_eq!(serde_json::to_string(&odd).unwrap(), plain_json, "{v:?} {ls} {c:?}");
        assert!(!odd.text_uses_block(), "{v:?} {ls} {c:?}");
    }
    // One reader for the range: not finite is the usual spacing, else the nearer limit.
    for (ls, want) in [(f64::NAN, 1.0), (f64::INFINITY, 1.0), (f64::NEG_INFINITY, 1.0), (9.0, 3.0), (0.1, 0.5), (1.5, 1.5), (3.0, 3.0)] {
        assert_eq!(line_spacing_in_range(ls), want, "{ls}");
    }
    for (name, v) in [
        (" MIDDLE ", TextVAlign::Middle),
        ("centre", TextVAlign::Middle),
        ("Center", TextVAlign::Middle),
        ("Top", TextVAlign::Top),
        ("bottom", TextVAlign::Bottom),
    ] {
        assert_eq!(TextVAlign::from_name(name), Some(v), "{name}");
    }
    assert_eq!((TextVAlign::from_name("baseline"), TextCase::from_name("title")), (None, None));
    assert_eq!(TextCase::from_name(" Upper "), Some(TextCase::Upper));
    assert_eq!(TextCase::Upper.apply("Straße {9}".into()), "STRASSE {9}");
    assert_eq!(TextCase::Lower.apply("ÉTÉ".into()), "été");
    assert_eq!(TextCase::None.apply("As Typed".into()), "As Typed");
    // A hand-edited layout.
    let extras = [
        json!({"line_spacing": 9}),                                      // 2: to 3, said
        json!({"line_spacing": 0.1}),                                    // 3: to 0.5, said
        json!({"line_spacing": "1.5"}),                                  // 4: no number: 1, said
        json!({"line_spacing": [2]}),                                    // 5: 1, said
        json!({"valign": "sideways"}),                                   // 6: top, said
        json!({"valign": 7}),                                            // 7: top, said
        json!({"case": "title"}),                                        // 8: none, said
        json!({"case": {"x": 1}}),                                       // 9: none, said
        json!({"valign": "Middle", "case": "UPPER", "line_spacing": 2}), // 10: kept, not said
        json!({"valign": null, "case": null, "line_spacing": null}),     // 11: defaults, not said
        json!({"valign": "centre", "line_spacing": 3.0}),                // 12: kept, not said
    ];
    let elements: Vec<_> = extras
        .iter()
        .enumerate()
        .map(|(i, extra)| {
            let mut t = json!({"text": "a", "size": 10.0, "color": [0, 0, 0, 255], "bold": false});
            t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json!({"id": i + 2, "name": "t", "x": 0, "y": 0, "w": 10, "h": 10, "kind": {"Text": t}})
        })
        .collect();
    let mut l: Layout = serde_json::from_value(json!({"name": "x", "width": 100, "height": 100, "elements": elements})).unwrap();
    let repairs = l.clamp();
    let block = |id| match &l.element(id).unwrap().kind {
        ElementKind::Text { valign, line_spacing, case, .. } => (*valign, *line_spacing, *case),
        k => panic!("{k:?}"),
    };
    use {TextCase as C, TextVAlign as V};
    let want = [
        (2, (V::Top, 3.0, C::None)),
        (3, (V::Top, 0.5, C::None)),
        (4, (V::Top, 1.0, C::None)),
        (5, (V::Top, 1.0, C::None)),
        (6, (V::Top, 1.0, C::None)),
        (7, (V::Top, 1.0, C::None)),
        (8, (V::Top, 1.0, C::None)),
        (9, (V::Top, 1.0, C::None)),
        (10, (V::Middle, 2.0, C::Upper)),
        (11, (V::Top, 1.0, C::None)),
        (12, (V::Middle, 3.0, C::None)),
    ];
    for (id, b) in want {
        assert_eq!(block(id), b, "{id}");
    }
    assert_eq!((repairs.elements_adjusted, repairs.elements_cut), (8, 0));
    // Repaired to the defaults, they save nothing; no marker is ever written.
    let saved = serde_json::to_string(&l).unwrap();
    assert!(!saved.contains("sideways") && !saved.contains("title") && saved.matches("\"valign\"").count() == 2, "{saved}");
}

#[test]
fn scale_bars_without_details_save_as_before() {
    // As main writes a plain bar: colour and fill always, nothing new.
    let plain =
        r#"{"ScaleBar":{"frame":2,"units":"Auto","divisions":4,"style":"Alternating","color":[34,34,34,255],"fill":[255,255,255,255]}}"#;
    let bar: ElementKind = serde_json::from_str(plain).unwrap();
    assert_eq!(serde_json::to_string(&bar).unwrap(), plain);
    assert_eq!(bar, ElementKind::scale_bar(2));
    assert!(!bar.scale_bar_uses_details());
    let ElementKind::ScaleBar {
        subdivisions,
        divisions_before_zero,
        label_position,
        unit_label,
        unit_label_position,
        decimals,
        text_size,
        fit,
        ..
    } = &bar
    else {
        panic!("a scale bar")
    };
    assert_eq!((*subdivisions, *divisions_before_zero, *decimals, *text_size), (0, 0, None, SCALE_BAR_TEXT_PT));
    assert_eq!(
        (*label_position, unit_label.clone(), *unit_label_position, *fit),
        (ScaleLabelPosition::Above, None, UnitLabelPosition::AfterLabels, ScaleBarFit::AdjustDivisionValue)
    );
    // Every setting, saved under its own key and read back equal.
    let mut all = bar.clone();
    if let ElementKind::ScaleBar {
        subdivisions,
        divisions_before_zero,
        label_position,
        unit_label,
        unit_label_position,
        decimals,
        text_size,
        fit,
        ..
    } = &mut all
    {
        (*subdivisions, *divisions_before_zero, *label_position) = (4, 1, ScaleLabelPosition::Below);
        (*unit_label, *unit_label_position) = (Some("Kilometres".into()), UnitLabelPosition::AfterBar);
        (*decimals, *text_size, *fit) = (Some(2), 10.5, ScaleBarFit::AdjustWidth);
    }
    assert!(all.scale_bar_uses_details());
    // Its text size alone is no detail: the text size capability covers it.
    let mut sized = bar.clone();
    if let ElementKind::ScaleBar { text_size, .. } = &mut sized {
        *text_size = 16.0;
    }
    assert!(!sized.scale_bar_uses_details() && sized.has_custom_text_size());
    let s = serde_json::to_string(&all).unwrap();
    // The middle fit saves as the camelCase of its variant.
    let mut middle = all.clone();
    if let ElementKind::ScaleBar { fit, .. } = &mut middle {
        *fit = ScaleBarFit::AdjustDivision;
    }
    assert!(serde_json::to_string(&middle).unwrap().contains(r#""fit":"adjustDivision""#));
    assert!(
        s.starts_with(
            r#"{"ScaleBar":{"frame":2,"units":"Auto","divisions":4,"style":"Alternating","color":[34,34,34,255],"fill":[255,255,255,255],"#
        ),
        "{s}"
    );
    for part in [
        r#""subdivisions":4"#,
        r#""divisions_before_zero":1"#,
        r#""label_position":"below""#,
        r#""unit_label":"Kilometres""#,
        r#""unit_label_position":"afterBar""#,
        r#""decimals":2"#,
        r#""text_size":10.5"#,
        r#""fit":"adjustWidth""#,
    ] {
        assert!(s.contains(part), "{part} in {s}");
    }
    assert_eq!(serde_json::from_str::<ElementKind>(&s).unwrap(), all);
    // An empty unit label is a setting too: it prints none.
    let mut blank = bar.clone();
    if let ElementKind::ScaleBar { unit_label, .. } = &mut blank {
        *unit_label = Some(String::new());
    }
    assert!(blank.scale_bar_uses_details());
    assert!(serde_json::to_string(&blank).unwrap().contains(r#""unit_label":"""#));
    // Fixed at no decimals is a setting too, apart from auto: its thousands are set apart.
    let mut whole = bar.clone();
    if let ElementKind::ScaleBar { decimals, .. } = &mut whole {
        *decimals = Some(0);
    }
    assert!(whole.scale_bar_uses_details());
    assert!(serde_json::to_string(&whole).unwrap().contains(r#""decimals":0"#));
    // A loaded layout's out-of-range values are brought into range, and said.
    let text = format!(
        r#"{{"name":"x","width":200,"height":100,"elements":[
            {{"id":1,"name":"a","x":0,"y":0,"w":10,"h":10,"kind":{{"MapFrame":{{"map":"m","center":{{"x":0,"y":0}},"scale":1000}}}}}},
            {{"id":2,"name":"b","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"subdivisions":99,"divisions_before_zero":9,"decimals":9,"text_size":1e9,"unit_label":"{}"}}}}}},
            {{"id":3,"name":"c","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"text_size":-4,"unit_label":"a\r\nb\rc\nd"}}}}}},
            {{"id":4,"name":"d","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"subdivisions":1}}}}}},
            {{"id":5,"name":"e","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":300}}}}}},
            {{"id":6,"name":"f","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":-1}}}}}},
            {{"id":7,"name":"g","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":2.5}}}}}},
            {{"id":8,"name":"h","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":" Auto "}}}}}},
            {{"id":9,"name":"i","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":"two"}}}}}},
            {{"id":10,"name":"j","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"decimals":[2]}}}}}},
            {{"id":11,"name":"k","x":0,"y":0,"w":10,"h":10,"kind":{{"ScaleBar":{{"frame":1,"divisions":10,"divisions_before_zero":2}}}}}}]}}"#,
        "x".repeat(150)
    );
    let mut l: Layout = serde_json::from_str(&text).unwrap();
    let repairs = l.clamp();
    let got = |id| match l.element(id).map(|e| &e.kind) {
        Some(ElementKind::ScaleBar { subdivisions, divisions_before_zero, decimals, text_size, unit_label, .. }) => {
            (*subdivisions, *divisions_before_zero, *decimals, *text_size, unit_label.clone().unwrap_or_default())
        }
        _ => (0, 0, None, 0.0, String::new()),
    };
    assert_eq!(
        got(2),
        (
            MAX_SCALE_SUBDIVISIONS,
            MAX_SCALE_BEFORE_ZERO,
            Some(MAX_SCALE_DECIMALS as u8),
            *LAYOUT_TEXT_SIZE.end(),
            "x".repeat(MAX_UNIT_LABEL_CHARS)
        )
    );
    assert_eq!(got(3), (0, 0, None, *LAYOUT_TEXT_SIZE.start(), "a b c d".to_string()));
    // One subdivision is the division whole: it loads as none, quietly, and asks for no details.
    assert_eq!(got(4), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
    assert!(l.element(4).is_some_and(|e| !e.kind.scale_bar_uses_details()));
    // A count past 255 loads too (it would not fit a byte) and ends at the most, said.
    assert_eq!(got(5), (0, 0, Some(MAX_SCALE_DECIMALS as u8), SCALE_BAR_TEXT_PT, String::new()));
    // A negative or fractional count is no count: it goes back to automatic, and is said too.
    assert_eq!(got(6), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
    assert_eq!(got(7), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
    // Any JSON kind loads: the word "auto" is automatic and no repair; other words and lists
    // are no count, so they go back to automatic and are said too.
    for id in [8, 9, 10] {
        assert_eq!(got(id), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()), "{id}");
    }
    // Ten divisions with two before zero are more than the ten a bar draws: the ones before zero
    // give way, and it is said.
    assert_eq!(got(11), (0, 0, None, SCALE_BAR_TEXT_PT, String::new()));
    assert!(matches!(l.element(11).map(|e| &e.kind), Some(ElementKind::ScaleBar { divisions: 10, .. })));
    assert_eq!((repairs.elements_adjusted, repairs.elements_cut), (8, 1));
    for v in ScaleLabelPosition::ALL {
        assert_eq!(ScaleLabelPosition::from_name(&format!(" {} ", v.code().to_uppercase())), Some(v));
    }
    for v in UnitLabelPosition::ALL {
        assert_eq!(UnitLabelPosition::from_name(v.code()), Some(v));
    }
    for v in ScaleBarFit::ALL {
        assert_eq!(ScaleBarFit::from_name(v.code()), Some(v));
    }
    assert_eq!(ScaleBarFit::from_name("manual"), None);
}
