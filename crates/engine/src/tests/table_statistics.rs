//! `table.statistics`: a column's numbers, without an output table or an undo step.
#![expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
use super::*;
use rustgis_data::{Dataset, Date, Field, FieldType, Geometry, Value as V};

/// A layer "parcels" of five points: `area` (decimals, one null, one NaN), `n` (whole numbers),
/// `name` (text, one empty, one null) and `built` (dates).
fn parcels() -> Session {
    let mut s = Session::new(Project::default());
    let fields = vec![
        Field::new("area", FieldType::Double),
        Field::new("n", FieldType::Integer),
        Field::new("name", FieldType::Text),
        Field::new("built", FieldType::Date),
    ];
    let mut ds = Dataset::new("parcels", rustgis_data::GeometryType::Point, fields);
    let date = |t: &str| V::Date(Date::parse(t).unwrap());
    let rows = [
        (V::Float(2.0), V::Int(10), V::Text("b".into()), date("2020-05-01")),
        (V::Float(4.0), V::Int(20), V::Text("a".into()), date("1999-12-31")),
        (V::Float(6.0), V::Int(30), V::Text("b".into()), V::Null),
        (V::Null, V::Int(40), V::Text(String::new()), date("2026-01-01")),
        (V::Float(f64::NAN), V::Null, V::Null, V::Null),
    ];
    for (i, (a, n, t, d)) in rows.into_iter().enumerate() {
        ds.push(Some(Geometry::Point(rustgis_geom::Coord::new(i as f64, 0.0))), vec![a, n, t, d]);
    }
    crate::cmd::layer::add_dataset(&mut s, ds);
    s
}

fn stats(s: &mut Session, p: Value) -> Value {
    s.run("table.statistics", &p).unwrap()
}

#[test]
fn a_numeric_column_gives_its_numbers_over_all_rows() {
    let mut s = parcels();
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "parcels", "field": "area"}));
    assert_eq!(v["rows"], 5);
    assert_eq!(v["kind"], "number");
    assert_eq!((v["count"].clone(), v["nulls"].clone(), v["skipped"].clone()), (json!(3), json!(1), json!(1)));
    assert_eq!(v["sum"], 12.0);
    assert_eq!(v["mean"], 4.0);
    assert_eq!((v["min"].clone(), v["max"].clone()), (json!(2.0), json!(6.0)));
    // Population: sqrt(((2-4)^2 + 0 + (6-4)^2) / 3).
    assert!((v["stdDev"].as_f64().unwrap() - (8.0f64 / 3.0).sqrt()).abs() < 1e-12, "{v}");
    let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said, ["parcels: 1 value(s) in 'area' that aren't finite numbers (NaN, infinity or other text) left out of the statistics"]);
    assert!(s.notices_since(mark).iter().all(|n| n.loss() == Some(rustgis_data::loss::LossCode::DroppedValue)), "each with its code");
    // Without a layer, the active one (the added layer).
    s.active_layer = s.map().layer_by_name("parcels").map(|l| l.id);
    assert_eq!(stats(&mut s, json!({"field": "N"}))["sum"], 100.0);
}

#[test]
fn a_selection_limits_the_rows_and_a_clean_column_says_nothing() {
    let mut s = parcels();
    s.run("select.rows", &json!({"layer": "parcels", "rows": [0, 1, 1, 99], "mode": "new"})).unwrap();
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "parcels", "field": "n", "selected": true}));
    assert_eq!(
        (v["rows"].clone(), v["count"].clone(), v["sum"].clone(), v["mean"].clone()),
        (json!(2), json!(2), json!(30.0), json!(15.0))
    );
    assert_eq!(v["stdDev"], 5.0);
    assert_eq!(v["selected"], true);
    assert!(s.notices_since(mark).is_empty(), "a clean column gives no notice");
    // Nothing selected, selected only: no rows, no mean.
    s.run("select.clear", &Value::Null).unwrap();
    let v = stats(&mut s, json!({"layer": "parcels", "field": "n", "selected": true}));
    assert_eq!((v["rows"].clone(), v["count"].clone(), v["sum"].clone(), v["mean"].clone()), (json!(0), json!(0), json!(0.0), Value::Null));
}

#[test]
fn text_and_date_columns_count_distinct_and_order() {
    let mut s = parcels();
    let v = stats(&mut s, json!({"layer": "parcels", "field": "name"}));
    assert_eq!(v["kind"], "text");
    assert_eq!((v["count"].clone(), v["nulls"].clone(), v["distinct"].clone()), (json!(3), json!(2), json!(2)));
    assert_eq!((v["min"].clone(), v["max"].clone()), (json!("a"), json!("b")));
    assert!(v.get("sum").is_none() && v.get("mean").is_none());
    let v = stats(&mut s, json!({"layer": "parcels", "field": "built"}));
    assert_eq!(v["kind"], "date");
    assert_eq!((v["count"].clone(), v["nulls"].clone(), v["distinct"].clone()), (json!(3), json!(2), json!(3)));
    assert_eq!((v["min"].clone(), v["max"].clone()), (json!("1999-12-31"), json!("2026-01-01")));
}

#[test]
fn bad_requests_are_errors_and_nothing_is_recorded() {
    let mut s = parcels();
    let err = s.run("table.statistics", &json!({"layer": "parcels", "field": "nope"})).unwrap_err();
    assert!(err.contains("'nope'"), "{err}");
    assert!(s.run("table.statistics", &json!({"layer": "parcels"})).is_err());
    assert!(s.run("table.statistics", &json!({"layer": "missing", "field": "area"})).is_err());
    let (depth, dirty) = (s.undo_depth(), s.dirty());
    stats(&mut s, json!({"layer": "parcels", "field": "area"}));
    stats(&mut s, json!({"layer": "parcels", "field": "name", "selected": true}));
    assert_eq!((s.undo_depth(), s.dirty()), (depth, dirty), "read only: no undo step, project not marked changed");
}

/// A layer "w" with one column per kind, from `rows` of (whole, decimal-or-whole, text).
fn layer_of(name: &str, fields: Vec<Field>, rows: Vec<Vec<V>>) -> Session {
    let mut s = Session::new(Project::default());
    let mut ds = Dataset::new(name, rustgis_data::GeometryType::Point, fields);
    for (i, r) in rows.into_iter().enumerate() {
        ds.push(Some(Geometry::Point(rustgis_geom::Coord::new(i as f64, 0.0))), r);
    }
    crate::cmd::layer::add_dataset(&mut s, ds);
    s
}

#[test]
fn whole_numbers_past_2_53_are_added_up_exactly_and_say_nothing() {
    let big = (1_i64 << 53) + 1;
    let mut s = layer_of("w", vec![Field::new("n", FieldType::Integer)], vec![vec![V::Int(big)], vec![V::Int(big + 2)], vec![V::Null]]);
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "w", "field": "n"}));
    assert_eq!(v["sum"], 2 * big + 2, "{v}");
    assert_eq!((v["min"].clone(), v["max"].clone()), (json!(big), json!(big + 2)));
    // Population standard deviation of two values 2 apart is 1, however large they are.
    assert_eq!(v["stdDev"], 1.0);
    assert!((v["mean"].as_f64().unwrap() - (big + 1) as f64).abs() <= 2.0);
    assert!(s.notices_since(mark).is_empty(), "exact: nothing approximated, nothing said");
    // A sum past i64 is its digits, not a rounded number.
    let mut s = layer_of("w", vec![Field::new("n", FieldType::Integer)], vec![vec![V::Int(i64::MAX)], vec![V::Int(i64::MAX)]]);
    let v = stats(&mut s, json!({"layer": "w", "field": "n"}));
    assert_eq!(v["sum"], "18446744073709551614");
    assert_eq!(v["max"], i64::MAX);
    // Within 2^53 the answer stays a plain number.
    let mut s = layer_of("w", vec![Field::new("n", FieldType::Integer)], vec![vec![V::Int(3)], vec![V::Int(4)]]);
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": "n"}))["sum"], 7.0);
}

#[test]
fn whole_numbers_added_with_decimals_past_2_53_say_how_many_are_rounded() {
    let big = (1_i64 << 53) + 1;
    let fields = vec![Field::new("x", FieldType::Double)];
    let mut s = layer_of("w", fields.clone(), vec![vec![V::Int(big)], vec![V::Float(0.5)], vec![V::Int(-big)]]);
    let mark = s.notice_mark();
    stats(&mut s, json!({"layer": "w", "field": "x"}));
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert_eq!(said, ["w: 2 whole number(s) in 'x' past 2^53 are rounded in the statistics, because the column also holds decimals"]);
    assert!(s.notices_since(mark).iter().all(|n| n.loss() == Some(rustgis_data::loss::LossCode::ChangedRounded)), "each with its code");
    // Past 2^53 but held exactly (2^54): a sum with decimals that comes out rounded is said
    // (2^54 + 5.5 is 2^54 + 4 as a decimal); one that comes out exact isn't.
    let mut s = layer_of("w", fields.clone(), vec![vec![V::Int(5)], vec![V::Float(0.5)], vec![V::Int(1 << 54)]]);
    let mark = s.notice_mark();
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": "x"}))["sum"], ((1_i64 << 54) + 4) as f64);
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| (n.text.contains("are rounded"), n.loss())).collect();
    assert_eq!(said, [(true, Some(rustgis_data::loss::LossCode::ChangedRounded))]);
    let mut s = layer_of("w", fields.clone(), vec![vec![V::Float(0.0)], vec![V::Int(1 << 54)]]);
    let mark = s.notice_mark();
    stats(&mut s, json!({"layer": "w", "field": "x"}));
    assert!(s.notices_since(mark).is_empty(), "an exact sum says nothing");
    // Decimals alone round a sum only as decimals do, which isn't said: the big ones cancel.
    let cancel = vec![vec![V::Int(1 << 54)], vec![V::Int(-(1 << 54))], vec![V::Float(0.1)], vec![V::Float(0.2)]];
    let mut s = layer_of("w", fields.clone(), cancel);
    let mark = s.notice_mark();
    stats(&mut s, json!({"layer": "w", "field": "x"}));
    assert!(s.notices_since(mark).is_empty(), "{:?}", s.notices_since(mark));
    // Decimals alone past 2^53 round it the same way, and say so.
    let mut s = layer_of("w", fields.clone(), vec![vec![V::Float((1_i64 << 54) as f64)], vec![V::Float(0.5)]]);
    let mark = s.notice_mark();
    stats(&mut s, json!({"layer": "w", "field": "x"}));
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.loss()).collect();
    assert_eq!(said, [Some(rustgis_data::loss::LossCode::ChangedRounded)]);
    // So is the exact sum given where the running one overflows.
    let huge = || vec![V::Float(f64::MAX), V::Float(f64::MAX), V::Float(-f64::MAX), V::Float(-f64::MAX), V::Int(1 << 54)];
    let mut rows: Vec<Vec<V>> = huge().into_iter().map(|v| vec![v]).collect();
    rows.push(vec![V::Float(0.5)]);
    let mut s = layer_of("w", fields.clone(), rows);
    let mark = s.notice_mark();
    stats(&mut s, json!({"layer": "w", "field": "x"}));
    assert!(
        s.notices_since(mark).iter().any(|n| n.loss() == Some(rustgis_data::loss::LossCode::ChangedRounded)),
        "{:?}",
        s.notices_since(mark)
    );
    // The same column with values a decimal holds: nothing said.
    let mut s = layer_of("w", fields, vec![vec![V::Int(5)], vec![V::Float(0.5)]]);
    let mark = s.notice_mark();
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": "x"}))["sum"], 5.5);
    assert!(s.notices_since(mark).is_empty());
}

#[test]
fn a_sum_too_large_for_a_decimal_is_left_out_with_a_notice_and_the_mean_stays_right() {
    let fields = vec![Field::new("x", FieldType::Double)];
    let mut s = layer_of("w", fields.clone(), vec![vec![V::Float(1e308)], vec![V::Float(1e308)]]);
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "w", "field": "x"}));
    assert_eq!(v["sum"], Value::Null);
    assert_eq!(v["mean"], 1e308, "the running mean doesn't overflow");
    assert_eq!(v["count"], 2);
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert_eq!(said, ["w: the sum of 'x' is too large to hold as a decimal number (past about 1.8e308) and is left out"]);
    assert!(
        s.notices_since(mark).iter().all(|n| n.loss() == Some(rustgis_data::loss::LossCode::DroppedCalculationResult)),
        "each with its code"
    );
    // Opposite extremes have a representable deviation, despite their squared spread.
    let mut s = layer_of("w", fields, vec![vec![V::Float(1.7e308)], vec![V::Float(-1.7e308)]]);
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "w", "field": "x"}));
    assert_eq!(v["sum"], 0.0);
    assert_eq!(v["mean"], 0.0, "the running mean of opposite extremes is zero, not overflowed: {v}");
    assert_eq!(v["stdDev"], 1.7e308);
    assert!(s.notices_since(mark).is_empty(), "{v}");
}

#[test]
fn representable_sums_and_deviations_survive_intermediate_overflow_and_underflow() {
    for (values, sum, deviation) in [
        (vec![0.0, 1e200], 1e200, 5e199),
        (vec![0.0, 1e-200], 1e-200, 5e-201),
        (vec![0.0, 5e-162], 5e-162, 2.5e-162),
        (vec![0.0, 1e-160], 1e-160, 5e-161),
        (vec![1e308, 1e308, -1e308], 1e308, (1e308 / 3.0) * 2.0 * 2.0f64.sqrt()),
    ] {
        let mut s = layer_of("w", vec![Field::new("x", FieldType::Double)], values.into_iter().map(|x| vec![V::Float(x)]).collect());
        let mark = s.notice_mark();
        let v = stats(&mut s, json!({"layer":"w","field":"x"}));
        assert_eq!(v["sum"], sum, "{v}");
        let got = v["stdDev"].as_f64().unwrap();
        assert!((got / deviation - 1.0).abs() < 1e-14, "{v}, expected {deviation}");
        assert!(s.notices_since(mark).is_empty(), "{v}");
    }
    // Small terms survive cancellation of overflowing prefixes, even at the subnormal limit.
    for small in [1e-200, f64::from_bits(1), -1e-200] {
        let mut s = layer_of(
            "w",
            vec![Field::new("x", FieldType::Double)],
            [1e308, 1e308, small, -1e308, -1e308].into_iter().map(|x| vec![V::Float(x)]).collect(),
        );
        assert_eq!(stats(&mut s, json!({"layer":"w","field":"x"}))["sum"], small);
    }
}

#[test]
fn distinct_text_bytes_are_bounded_and_mixed_cells_keep_their_full_text() {
    let chunk = "x".repeat(crate::cmd::table_stats::MAX_DISTINCT_BYTES / 2);
    let mut s = layer_of(
        "w",
        vec![Field::new("text", FieldType::Text)],
        vec![vec![V::Text(chunk.clone())], vec![V::Text(format!("y{chunk}"))], vec![V::Text("z".into())]],
    );
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer":"w","field":"text"}));
    assert_eq!(v["count"], 3);
    assert_eq!(v["distinct"], 1);
    assert_eq!(v["distinctCapped"], true);
    assert_eq!(v["max"], "z");
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("8 MiB") && said[0].contains("more than 1"), "{said:?}");
    let mut s = layer_of("w", vec![Field::new("text", FieldType::Text)], vec![vec![V::Float(1e-7)], vec![V::Float(2e-7)]]);
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer":"w","field":"text"}));
    assert_eq!(v["distinct"], 2);
    assert_eq!(v["min"], "0.0000001");
    assert_eq!(v["max"], "0.0000002");
    assert!(s.notices_since(mark).is_empty());
}

#[test]
fn non_date_cells_in_a_date_field_are_counted_and_said() {
    let mut s = layer_of(
        "w",
        vec![Field::new("date", FieldType::Date)],
        vec![vec![V::Date(Date::parse("2026-01-01").unwrap())], vec![V::Text("not a date".into())]],
    );
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer":"w","field":"date"}));
    assert_eq!(v["count"], 2);
    assert_eq!(v["min"], "2026-01-01");
    assert_eq!(v["max"], "2026-01-01");
    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("1 value(s)") && said[0].contains("aren't dates"), "{said:?}");
    assert!(s.notices_since(mark).iter().all(|n| n.loss() == Some(rustgis_data::loss::LossCode::DroppedValue)), "each with its code");
    let mut clean = layer_of("w", vec![Field::new("date", FieldType::Date)], vec![vec![V::Date(Date::parse("2026-01-01").unwrap())]]);
    let mark = clean.notice_mark();
    assert_eq!(stats(&mut clean, json!({"layer":"w","field":"date"}))["min"], "2026-01-01");
    assert!(clean.notices_since(mark).is_empty());
}

#[test]
fn distinct_texts_are_counted_to_a_bound_and_past_it_the_answer_says_so() {
    let fields = vec![Field::new("id", FieldType::Text)];
    let rows = |n: usize| (0..n).map(|i| vec![V::Text(format!("id{i}"))]).collect::<Vec<_>>();
    let mut s = layer_of("w", fields.clone(), rows(crate::cmd::table_stats::MAX_DISTINCT + 5));
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "w", "field": "id"}));
    assert_eq!(v["distinct"], crate::cmd::table_stats::MAX_DISTINCT);
    assert_eq!(v["distinctCapped"], true);
    assert_eq!(v["count"], crate::cmd::table_stats::MAX_DISTINCT + 5, "everything else is still counted");
    assert_eq!(v["max"], "id99999");
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert_eq!(said, ["w: 'id' has more than 100000 distinct values; the distinct count stops at 100000"]);
    assert!(s.notices_since(mark).iter().all(|n| n.loss() == Some(rustgis_data::loss::LossCode::DroppedOverLimit)), "each with its code");
    // Exactly the bound: counted whole, nothing said.
    let mut s = layer_of("w", fields, rows(crate::cmd::table_stats::MAX_DISTINCT));
    let mark = s.notice_mark();
    let v = stats(&mut s, json!({"layer": "w", "field": "id"}));
    assert_eq!(v["distinct"], crate::cmd::table_stats::MAX_DISTINCT);
    assert!(v.get("distinctCapped").is_none() && s.notices_since(mark).is_empty());
}

#[test]
fn the_name_is_looked_up_as_given_before_its_trimmed_form() {
    let fields = vec![Field::new(" area ", FieldType::Double), Field::new("area", FieldType::Double)];
    let mut s = layer_of("w", fields, vec![vec![V::Float(1.0), V::Float(10.0)]]);
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": " area "}))["sum"], 1.0, "as given");
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": "area"}))["sum"], 10.0);
    // Stray spaces on a name with none: the trimmed form is the fallback.
    let mut s = layer_of("w", vec![Field::new("area", FieldType::Double)], vec![vec![V::Float(2.0)]]);
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": "  AREA "}))["sum"], 2.0);
    // Blank is no name.
    assert!(s.run("table.statistics", &json!({"layer": "w", "field": "   "})).is_err());
}

#[test]
fn a_column_is_told_apart_by_its_place_when_names_repeat() {
    let fields = vec![Field::new("v", FieldType::Double), Field::new("v", FieldType::Double)];
    let mut s = layer_of("w", fields, vec![vec![V::Float(1.0), V::Float(100.0)]]);
    assert_eq!(stats(&mut s, json!({"layer": "w", "field": "v"}))["sum"], 1.0, "by name: the first");
    let second = stats(&mut s, json!({"layer": "w", "field": "v", "index": 1}));
    assert_eq!((second["sum"].clone(), second["index"].clone()), (json!(100.0), json!(1)));
    let mark = s.notice_mark();
    assert_eq!(stats(&mut s, json!({"layer": "w", "index": 1}))["sum"], 100.0, "no name needed");
    assert!(s.notices_since(mark).is_empty(), "index is a recognized parameter");
    assert!(s.run("table.statistics", &json!({"layer": "w", "index": 2})).unwrap_err().contains("no column 2"));
    assert!(s.run("table.statistics", &json!({"layer": "w", "index": "x"})).is_err());
}

#[test]
fn the_desktops_count_ahead_is_what_the_command_answers_and_a_changed_layer_is_refused() {
    use crate::cmd::reads::{read_ahead, read_land};
    use rustgis_analysis::control::Control;
    let mut s = parcels();
    let p = json!({"layer": "parcels", "field": "area"});
    let here = stats(&mut s, p.clone());
    let ahead = read_ahead(&mut s, "table.statistics", &p).unwrap();
    assert!(ahead.reads());
    let read = ahead.run(&Control::stoppable()).unwrap();
    assert_eq!(read_land(&mut s, read).unwrap(), here);
    assert!(s.read_ahead.is_none(), "the count was taken");
    // Edited while it was counted: said, not counted again here.
    let read = read_ahead(&mut s, "table.statistics", &p).unwrap().run(&Control::stoppable()).unwrap();
    s.run("edit.setValue", &json!({"layer": "parcels", "row": 0, "field": "area", "value": "9"})).unwrap();
    let e = read_land(&mut s, read).unwrap_err();
    assert!(e.contains("changed while the statistics were counted"), "{e}");
    // A request that is wrong makes no count ahead: the command says why as it lands.
    let ahead = read_ahead(&mut s, "table.statistics", &json!({"layer": "parcels", "field": "nope"})).unwrap();
    assert!(!ahead.reads());
    let read = ahead.run(&Control::stoppable()).unwrap();
    assert!(read_land(&mut s, read).unwrap_err().contains("'nope'"));
    // Cancelled as it starts, the count looks and stops.
    let control = Control::cancelled_once_started();
    let e = read_ahead(&mut s, "table.statistics", &p).unwrap().run(&control).err().unwrap();
    assert!(e.to_lowercase().contains("cancel") && control.looked() > 0, "{e}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_unprepared_window_thread_refuses_statistics_but_a_prepared_read_lands() {
    use crate::cmd::reads::{read_ahead, read_land};
    use rustgis_analysis::control::Control;
    let mut s = parcels();
    let p = json!({"layer":"parcels","field":"area"});
    let expected = stats(&mut s, p.clone());
    s.statistics_need_read_ahead(true);
    let error = s.run("table.statistics", &p).unwrap_err();
    assert!(error.contains("background read queue"), "{error}");
    let ahead = read_ahead(&mut s, "table.statistics", &p).unwrap();
    let read = std::thread::spawn(move || ahead.run(&Control::stoppable())).join().unwrap().unwrap();
    assert_eq!(read_land(&mut s, read).unwrap(), expected);
    s.statistics_need_read_ahead(false);
    assert_eq!(stats(&mut s, p), expected, "headless synchronous adapters still work");
}

/// The count made ahead reports every row as done, the last block of fewer than 1,024 included,
/// and a layer under one block too.
#[test]
fn the_count_ahead_reports_progress_over_every_row() {
    use crate::cmd::reads::read_ahead;
    use rustgis_analysis::control::Control;
    for n in [5usize, 1025] {
        let rows: Vec<Vec<V>> = (0..n).map(|i| vec![V::Int(i as i64)]).collect();
        let mut s = layer_of("w", vec![Field::new("n", FieldType::Integer)], rows);
        let control = Control::stoppable();
        read_ahead(&mut s, "table.statistics", &json!({"layer": "w", "field": "n"})).unwrap().run(&control).unwrap();
        assert_eq!(control.progress(), Some((n as u64, n as u64)), "{n} rows");
    }
}
