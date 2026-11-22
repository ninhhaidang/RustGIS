//! Join Field through the tool, on tables read by the real CSV and Shapefile readers: an all-digit
//! id that one file types as a number and the other as text still joins (#1178).
use rustgis_analysis::{Inputs, Output};
use rustgis_data::io::{read_csv, read_path, write_path};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Layers {
    data: HashMap<String, Dataset>,
    selected: HashMap<String, Vec<usize>>,
}
impl Inputs for Layers {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        self.data.get(name)
    }
    fn selection(&self, name: &str) -> &[usize] {
        self.selected.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}
impl Layers {
    fn with(mut self, ds: Dataset) -> Self {
        self.data.insert(ds.name.clone(), ds);
        self
    }
}

fn field_ty(ds: &Dataset, name: &str) -> FieldType {
    ds.fields.iter().find(|f| f.name == name).map(|f| f.ty).unwrap()
}
fn column<'a>(ds: &'a Dataset, name: &str) -> Vec<&'a Value> {
    let i = ds.fields.iter().position(|f| f.name == name).unwrap();
    ds.features.iter().map(|f| &f.attrs[i]).collect()
}

/// Census tracts as a TIGER/Line shapefile holds them (`GEOID` a character field), written and
/// read back through the Shapefile reader.
fn tracts() -> Dataset {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let mut ds =
        Dataset::new("tracts", GeometryType::Point, vec![Field::new("GEOID", FieldType::Text), Field::new("NAMELSAD", FieldType::Text)]);
    for (geoid, x) in [("47157000100", -90.05), ("47157000200", -90.04), ("47157000300", -90.03), ("47157980100", -90.02)] {
        ds.push_next(Some(Geometry::Point(Coord::new(x, 35.1))), vec![Value::Text(geoid.into()), Value::Text(format!("Tract {geoid}"))]);
    }
    let dir = std::env::temp_dir().join(format!("rustgis-join-field-{}-{}", std::process::id(), CALLS.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    write_path(&ds, &dir.join("tracts.shp")).unwrap();
    let (read, _) = read_path(&dir.join("tracts.shp")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(read.name, "tracts");
    assert_eq!(field_ty(&read, "GEOID"), FieldType::Text, "premise: the Shapefile reader keeps an all-digit character GEOID as text");
    read
}

/// The Census Gazetteer's tract table as a CSV: the reader types an all-digit `GEOID` as a number.
fn gazetteer() -> Dataset {
    let csv = "GEOID,ALAND,AWATER\n47157000300,3000,30\n47157000100,1000,10\n47157000200,2000,20\n47157999999,9,0\n";
    let (ds, _) = read_csv(csv, "gazetteer").unwrap();
    assert_eq!(field_ty(&ds, "GEOID"), FieldType::Integer, "premise: the CSV reader types GEOID as Integer");
    ds
}

#[test]
fn census_gazetteer_joins_tiger_tracts_by_geoid() {
    let layers = Layers::default().with(tracts()).with(gazetteer());
    let res =
        rustgis_analysis::run("joinField", &layers, &json!({"target":"tracts","targetField":"GEOID","joinTable":"gazetteer"})).unwrap();
    let Output::Layer(out) = res.output else { panic!("a layer target gives a layer") };
    assert_eq!(column(&out, "ALAND"), [&Value::Int(1000), &Value::Int(2000), &Value::Int(3000), &Value::Null]);
    assert_eq!(column(&out, "AWATER"), [&Value::Int(10), &Value::Int(20), &Value::Int(30), &Value::Null]);
    // The target's key, rows, ids and geometry are kept as they were.
    let before = tracts();
    assert_eq!(field_ty(&out, "GEOID"), FieldType::Text);
    assert_eq!(column(&out, "GEOID"), column(&before, "GEOID"));
    assert_eq!(out.features.iter().map(|f| f.id).collect::<Vec<_>>(), before.features.iter().map(|f| f.id).collect::<Vec<_>>());
    assert!(out.features.iter().zip(before.features.iter()).all(|(a, b)| a.geometry == b.geometry));
    assert!(res.messages.iter().any(|m| m.contains("3/4 records matched")), "{:?}", res.messages);
    assert!(!res.messages.iter().any(|m| m.contains("leading zeros")), "no zeros were lost: {:?}", res.messages);
}

#[test]
fn a_number_key_target_joins_a_text_key_source() {
    let layers = Layers::default().with(tracts()).with(gazetteer());
    let res = rustgis_analysis::run(
        "joinField",
        &layers,
        &json!({"target":"gazetteer","targetField":"GEOID","joinTable":"tracts","fields":["NAMELSAD"]}),
    )
    .unwrap();
    let Output::Table(out) = res.output else { panic!("a table target gives a table") };
    assert_eq!(field_ty(&out, "GEOID"), FieldType::Integer, "the target key keeps its type");
    assert_eq!(field_ty(&out, "NAMELSAD"), FieldType::Text);
    assert_eq!(
        column(&out, "NAMELSAD"),
        [
            &Value::Text("Tract 47157000300".into()),
            &Value::Text("Tract 47157000100".into()),
            &Value::Text("Tract 47157000200".into()),
            &Value::Null
        ]
    );
}

#[test]
fn codes_with_leading_zeros_only_match_as_written() {
    // The CSV reader keeps `01001` as text; a layer stores the same county as the number 1001.
    let (codes, _) = read_csv("FIPS,county\n01001,Autauga\n01003,Baldwin\n1005,Barbour\n", "codes").unwrap();
    assert_eq!(field_ty(&codes, "FIPS"), FieldType::Text, "premise: a leading zero keeps the column text");
    let (counties, _) = read_csv("FIPS,pop\n1001,58805\n1003,231767\n1005,25223\n", "counties").unwrap();
    assert_eq!(field_ty(&counties, "FIPS"), FieldType::Integer);
    let layers = Layers::default().with(codes).with(counties);
    let res = rustgis_analysis::run("joinField", &layers, &json!({"target":"counties","targetField":"FIPS","joinTable":"codes"})).unwrap();
    let Output::Table(out) = res.output else { panic!("a table target gives a table") };
    // No leading zeros are made up: only the code written without one matches.
    assert_eq!(column(&out, "county"), [&Value::Null, &Value::Null, &Value::Text("Barbour".into())]);
    assert!(res.messages.iter().any(|m| m.contains("1/3 records matched")), "{:?}", res.messages);
    // The two codes that lost their zero are named, not only counted.
    let notice = res.messages.iter().find(|m| m.contains("leading zeros")).unwrap_or_else(|| panic!("{:?}", res.messages));
    assert!(notice.contains("2 record(s) left unmatched"), "{notice}");
    assert!(notice.contains("target key 1001, source key \"01001\""), "{notice}");
}

#[test]
fn selections_are_honoured_with_mixed_key_types() {
    let mut layers = Layers::default().with(tracts()).with(gazetteer());
    layers.selected.insert("tracts".into(), vec![0, 1, 3]);
    layers.selected.insert("gazetteer".into(), vec![0, 1]);
    let res =
        rustgis_analysis::run("joinField", &layers, &json!({"target":"tracts","targetField":"GEOID","joinTable":"gazetteer"})).unwrap();
    let Output::Layer(out) = res.output else { panic!("a layer target gives a layer") };
    assert_eq!(
        column(&out, "GEOID"),
        [&Value::Text("47157000100".into()), &Value::Text("47157000200".into()), &Value::Text("47157980100".into())]
    );
    // 47157000100 matches a selected record; 47157000200 is in the gazetteer but not among its
    // selected records.
    assert_eq!(column(&out, "ALAND"), [&Value::Int(1000), &Value::Null, &Value::Null]);
}

#[test]
fn duplicate_strategies_apply_to_mixed_key_types() {
    let (dups, _) = read_csv("GEOID,ALAND\n47157000100,1\n47157000100,2\n", "dups").unwrap();
    let layers = Layers::default().with(tracts()).with(dups);
    let run = |duplicates: &str| {
        rustgis_analysis::run(
            "joinField",
            &layers,
            &json!({"target":"tracts","targetField":"GEOID","joinTable":"dups","duplicates":duplicates}),
        )
    };
    let Output::Layer(first) = run("First").unwrap().output else { panic!() };
    assert_eq!(column(&first, "ALAND")[0], &Value::Int(1));
    let Output::Layer(last) = run("Last").unwrap().output else { panic!() };
    assert_eq!(column(&last, "ALAND")[0], &Value::Int(2));
    let err = run("Error").err().unwrap();
    assert!(err.contains("duplicate key \"47157000100\""), "{err}");
}

#[test]
fn other_key_type_pairs_are_refused_with_the_way_out() {
    let (decimal, _) = read_csv("GEOID,v\n47157000100.5,1\n", "decimal").unwrap();
    assert_eq!(field_ty(&decimal, "GEOID"), FieldType::Double);
    let layers = Layers::default().with(tracts()).with(decimal);
    let err =
        rustgis_analysis::run("joinField", &layers, &json!({"target":"tracts","targetField":"GEOID","joinTable":"decimal"})).err().unwrap();
    assert!(err.contains("target field 'GEOID' is Text, but source field 'GEOID' is Double"), "{err}");
    assert!(err.contains("Add Field and Calculate Field"), "{err}");
}
