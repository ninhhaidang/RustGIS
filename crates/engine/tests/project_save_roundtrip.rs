//! Saving a project and opening it again gives back the same project, as a property: a project
//! built with commands from a few small layers (Project › Save As, then Project › Open) opens with
//! the same layers, symbology, labels, groups, layouts, bookmarks, time and map settings, or the
//! save or open says what changed. What a project file doesn't keep is listed in [`LIMITS`], with
//! the reason. Projects lean on edges: unicode, long and blank names, every renderer kind, nested
//! groups, an empty layer, and values that aren't finite numbers, as a reader may leave them.
//!
//! A second property: a saved file with fields this version doesn't know opens as it would
//! without them, or is refused; one that declares a newer format is refused, says to update, and
//! changes nothing.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. To run a seed again, or
//! deeper: `PROPERTY_SEED=7 PROPERTY_CASES=2000 cargo test -p rustgis-engine --test project_save_roundtrip`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Field, FieldType, GeometryType, Value as DataValue},
};
use serde_json::{Value, json};
use std::path::Path;

/// Parts of a project that saving and opening change by design, as (a path in the saved project,
/// `*` for any one step of it, and why).
const LIMITS: &[(&str, &str)] = &[
    ("/name", "an opened project takes its file's name (`name_from_file`)"),
    ("/maps/*/layers/*/geometry_rev", "a counter that tells caches the geometry changed, bumped as a layer is read"),
];

/// Whether the change `c` ("path: before → after") is at a path listed in [`LIMITS`].
fn declared(c: &str) -> bool {
    let path: Vec<&str> = c.split(": ").next().unwrap_or_default().split('/').collect();
    LIMITS.iter().any(|(limit, _)| {
        let limit: Vec<&str> = limit.split('/').collect();
        limit.len() <= path.len() && limit.iter().zip(&path).all(|(l, p)| *l == "*" || l == p)
    })
}

/// A number from the environment, else `default`.
fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Names leaning on edges: unicode, long, blank, spaces around, quotes, a duplicate.
fn name() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[A-Za-z][A-Za-z0-9 ]{0,10}",
        1 => prop::sample::select(vec![
            "Ünïcødé 名前 😀", "", " spaced ", "quote \"x\" 'y'", "line\nbreak", "Points", "a/b\\c", "<b>&amp;</b>", "  ",
        ])
        .prop_map(String::from),
        1 => (100usize..400).prop_map(|n| "é".repeat(n)),
    ]
}

/// A colour, sometimes transparent.
fn color() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => any::<[u8; 3]>().prop_map(|[r, g, b]| format!("#{r:02X}{g:02X}{b:02X}")),
        1 => any::<[u8; 4]>().prop_map(|[r, g, b, a]| format!("#{r:02X}{g:02X}{b:02X}{a:02X}")),
    ]
}

/// A number, sometimes on an edge.
fn number() -> impl Strategy<Value = f64> {
    prop_oneof![4 => 0.0..100.0f64, 1 => prop::sample::select(vec![0.0, -1.0, 1e-9, 1e12, 99.999, 100.0])]
}

/// One edit, as a command and its parameters; "@L<i>" stands for the id of the i-th layer.
fn step() -> impl Strategy<Value = (&'static str, Value)> {
    let layer = (0usize..4).prop_map(|i| format!("@L{i}"));
    prop_oneof![
        (layer.clone(), color()).prop_map(|(l, c)| ("layer.symbology", json!({"layer": l, "type": "single", "color": c}))),
        (layer.clone(), prop::sample::select(vec!["name", "flag", "pop", "day", "seen"]))
            .prop_map(|(l, f)| ("layer.symbology", json!({"layer": l, "type": "unique", "field": f}))),
        layer.clone().prop_map(|l| ("layer.symbology", json!({"layer": l, "type": "unique", "expression": "name || '!'"}))),
        (layer.clone(), 1u32..13, prop::sample::select(vec!["equal", "quantile", "natural", "manual"]))
            .prop_map(|(l, n, m)| ("layer.symbology", json!({"layer": l, "type": "graduated", "field": "pop", "classes": n, "method": m}))),
        layer.clone().prop_map(|l| ("layer.symbology", json!({"layer": l, "type": "proportional", "field": "pop"}))),
        (layer.clone(), 0u32..4, color(), name())
            .prop_map(|(l, i, c, n)| ("@classStyle", json!({"layer": l, "index": i, "color": c, "label": n}))),
        (layer.clone(), prop::sample::select(vec!["Ember", "Viridis"])).prop_map(|(l, r)| ("layer.ramp", json!({"layer": l, "ramp": r}))),
        (layer.clone(), number()).prop_map(|(l, v)| ("layer.transparency", json!({"layer": l, "value": v}))),
        (layer.clone(), any::<bool>()).prop_map(|(l, v)| ("layer.visibility", json!({"layer": l, "visible": v}))),
        (layer.clone(), number(), number()).prop_map(|(l, a, b)| ("layer.scaleRange", json!({"layer": l, "min": a * 1e5, "max": b * 1e3}))),
        (layer.clone(), name()).prop_map(|(l, n)| ("layer.rename", json!({"layer": l, "name": n}))),
        (layer.clone(), color(), number()).prop_map(|(l, c, w)| ("layer.outline", json!({"layer": l, "color": c, "width": w / 10.0}))),
        (layer.clone(), prop::sample::select(vec!["A", "★", "😀"]), number())
            .prop_map(|(l, c, n)| ("layer.marker", json!({"layer": l, "char": c, "size": n}))),
        (layer.clone(), prop::sample::select(vec!["pop > 10", "name = 'é'", "", "day >= '2000-01-01'", "seen < '2026-03-05T08:00Z'"]))
            .prop_map(|(l, w)| ("layer.definitionQuery", json!({"layer": l, "where": w}))),
        (layer.clone(), prop::sample::select(vec!["multiply", "screen", "normal"]))
            .prop_map(|(l, m)| ("layer.blend", json!({"layer": l, "mode": m}))),
        (layer.clone(), name()).prop_map(|(l, n)| (
            "layer.displayFilters",
            json!({"layer": l, "filters": [{"name": n, "minScale": 1e8, "maxScale": 1e3, "where": "pop > 1"}]})
        )),
        (layer.clone(), color())
            .prop_map(|(l, c)| ("layer.fillLayers", json!({"layer": l, "fills": [{"color": c}, {"pattern": "hatch", "angle": 135}]}))),
        layer.clone().prop_map(|l| ("layer.markerRotation", json!({"layer": l, "field": "pop", "counterclockwise": true}))),
        (layer.clone(), prop::sample::select(vec!["area", "radius"]))
            .prop_map(|(l, k)| ("layer.sizeBy", json!({"layer": l, "field": "pop", "minSize": 2, "maxSize": 30, "scaling": k}))),
        (layer.clone(), -5i32..6).prop_map(|(l, v)| ("layer.priority", json!({"layer": l, "value": v}))),
        (layer.clone(), number()).prop_map(|(l, v)| ("layer.referenceScale", json!({"layer": l, "scale": v * 1e4}))),
        layer.clone().prop_map(|l| ("layer.symbolBy", json!({"layer": l, "size": "pop / 10", "fill": "if(pop > 5, '#ff0000', NULL)"}))),
        layer.clone().prop_map(|l| ("layer.noFill", json!({"layer": l}))),
        (layer.clone(), any::<bool>()).prop_map(|(l, v)| ("labels.enable", json!({"layer": l, "on": v}))),
        (layer.clone(), prop::sample::select(vec!["name || ' ' || pop", "'Ünï 😀'", "pop"]))
            .prop_map(|(l, t)| ("labels.by", json!({"layer": l, "text": t}))),
        (layer.clone(), number(), any::<bool>(), any::<bool>())
            .prop_map(|(l, n, b, i)| ("labels.style", json!({"layer": l, "size": n / 2.0 + 1.0, "bold": b, "italic": i}))),
        (layer.clone(), 0u32..100).prop_map(|(l, c)| ("labels.placement", json!({"layer": l, "callout": c}))),
        (layer.clone(), number()).prop_map(|(l, m)| ("labels.scaleRange", json!({"layer": l, "min": m * 1e5}))),
        (name(), layer.clone(), layer.clone()).prop_map(|(n, a, b)| ("group.add", json!({"name": n, "layers": [a, b]}))),
        (layer.clone(), layer.clone()).prop_map(|(a, b)| ("@nest", json!([a, b]))),
        name().prop_map(|n| ("bookmark.add", json!({"name": n}))),
        (layer.clone(), prop::sample::select(vec!["when", "day", "seen"]))
            .prop_map(|(l, f)| ("time.bind", json!({"layer": l, "field": f}))),
        (prop::sample::select(vec!["2015-01", "2020-02-29", "1900-01-01"]), 1u32..5, prop::sample::select(vec!["cumulative", "wide"]))
            .prop_map(|(d, i, w)| ("time.set", json!({"start": "1900", "end": "2030", "date": d, "interval": i, "show": w}))),
        name().prop_map(|n| ("layout.new", json!({"name": n}))),
        (prop::sample::select(vec!["legend", "scaleBar", "northArrow", "text", "rectangle", "grid", "colorbar"]), name())
            .prop_map(|(k, t)| ("layout.addElement", json!({"kind": k, "text": t}))),
        prop::sample::select(vec!["paper", "midnight", "ocean", "atlas", "blank"]).prop_map(|n| ("map.basemap", json!({"name": n}))),
        prop::sample::select(vec!["Albers Equal Area Conic +lat_1=29.5 +lat_2=45.5", "EPSG:3857", "EPSG:4326"])
            .prop_map(|t| ("map.projection", json!({"type": t}))),
        (-180.0..180.0f64).prop_map(|b| ("map.setBearing", json!({"bearing": b}))),
        prop::sample::select(vec!["degrees", "measured", "utm"]).prop_map(|t| ("map.graticule", json!({"on": true, "type": t}))),
        (-180.0..180.0f64, -85.0..85.0f64, number())
            .prop_map(|(x, y, s)| ("map.camera", json!({"lon": x, "lat": y, "scale": s * 1e5 + 1.0}))),
        name().prop_map(|n| ("map.new", json!({"name": n}))),
        (layer.clone(), name()).prop_map(|(l, n)| ("style.new", json!({"layer": l, "name": n}))),
        (layer, 0u8..4, prop::sample::select(vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0]))
            .prop_map(|(l, row, v)| ("@poison", json!([l, row, v.to_string()]))),
    ]
}

/// Dates and moments of the fixture's rows (rows past them hold null), at the ends of years 1 to
/// 9999, around the epoch, on leap days, with microseconds, no zone, `Z` and odd offsets.
const DAYS: [&str; 4] = ["0001-01-01", "1969-12-31", "2024-02-29", "9999-12-31"];
const MOMENTS: [&str; 4] =
    ["9999-12-31T23:59:59.999999", "2026-03-05T14:30:00.123456+05:45", "0001-01-01T00:00:00-12:00", "1969-12-31T23:59:59-09:30"];

/// The fixture: points, lines and polygons with text, numbers, dates and yes/no values, and an
/// empty layer, each under a generated name.
fn layers(s: &mut Session, names: &[String]) -> Vec<u32> {
    const TEXT: [&str; 4] = ["Ünï 😀", "", "a, b", "x"];
    let props = |i: usize| json!({"name": TEXT[i % 4], "pop": i as f64 * 3.5, "when": format!("201{i}-0{}-15", i + 1), "flag": i.is_multiple_of(2)});
    let points = (0..4).map(|i| json!({"type": "Feature", "properties": props(i), "geometry": {"type": "Point", "coordinates": [i as f64 * 40.0 - 179.9, 89.9 - i as f64 * 50.0]}}));
    let lines = (0..3).map(|i| json!({"type": "Feature", "properties": props(i), "geometry": {"type": "LineString", "coordinates": [[-10.0, i as f64], [10.0, i as f64 + 1.0]]}}));
    let polygons = (0..3).map(|i| {
        let x = i as f64 * 5.0;
        json!({"type": "Feature", "properties": props(i), "geometry": {"type": "Polygon", "coordinates": [[[x, 0.0], [x + 4.0, 0.0], [x + 4.0, 4.0], [x, 0.0]]]}})
    });
    let sets: [Vec<Value>; 3] = [points.collect(), lines.collect(), polygons.collect()];
    let mut ids = Vec::new();
    for (features, name) in sets.iter().zip(names) {
        let text = json!({"type": "FeatureCollection", "features": features}).to_string();
        let before: Vec<u32> = s.map().layers.iter().map(|l| l.id).collect();
        s.run("layer.addDataText", &json!({"fileName": "layer.geojson", "text": text, "name": name})).unwrap();
        ids.extend(s.map().layers.iter().map(|l| l.id).filter(|id| !before.contains(id)));
    }
    // GeoJSON dates stay text, so the date fields are added as a reader of a typed format leaves
    // them: days and moments on the edges of the model, with offsets, and nulls.
    for id in &ids {
        let Some(l) = s.map_mut().layer_mut(*id) else { continue };
        let (day, seen) = (l.dataset.ensure_field("day", FieldType::Date), l.dataset.ensure_field("seen", FieldType::DateTime));
        for (i, f) in l.dataset.features.iter_mut().enumerate() {
            let cell = |t: Option<&&str>, ty| t.map_or(DataValue::Null, |t| DataValue::parse(t, ty));
            if let Some(c) = f.attrs.get_mut(day) {
                *c = cell(DAYS.get(i), FieldType::Date);
            }
            if let Some(c) = f.attrs.get_mut(seen) {
                *c = cell(MOMENTS.get(i), FieldType::DateTime);
            }
        }
    }
    // A file without features isn't read as a layer, so the empty one is added as a reader leaves it.
    let fields = ["name", "when"].map(|f| Field::new(f, FieldType::Text)).into_iter().chain([
        Field::new("pop", FieldType::Double),
        Field::new("day", FieldType::Date),
        Field::new("seen", FieldType::DateTime),
    ]);
    let empty = Dataset::new(names.get(3).map_or("", String::as_str), GeometryType::Point, fields.collect());
    ids.push(s.map_mut().add_dataset(empty));
    ids
}

/// `params` with each "@L<i>" replaced by that layer's id.
fn resolve(params: &Value, ids: &[u32]) -> Value {
    match params {
        Value::String(t) => {
            t.strip_prefix("@L").and_then(|i| i.parse::<usize>().ok()).and_then(|i| ids.get(i)).map_or(params.clone(), |id| json!(id))
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| resolve(v, ids)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), resolve(v, ids))).collect()),
        _ => params.clone(),
    }
}

/// Runs one edit, true when it changed something. `@nest` puts the second layer's group inside
/// the first's; `@classStyle` styles a class of Unique Values; `@poison` leaves a number that isn't
/// finite in a row's values, as a reader may (a NaN REAL in a GeoPackage). The commands and the
/// readers refuse one in a style, so a style isn't given one.
fn apply(s: &mut Session, cmd: &str, params: &Value, ids: &[u32]) -> bool {
    let p = resolve(params, ids);
    // A layer's command works on the layer selected in the Layers pane.
    if let Some(id) = p.get("layer").and_then(Value::as_u64) {
        s.active_layer = u32::try_from(id).ok();
    }
    match cmd {
        "@nest" => {
            let groups = s.run("group.add", &json!({"name": "Outer", "layers": [p[0]]})).ok().and_then(|v| v["group"].as_u64());
            let inner = s.run("group.add", &json!({"name": "Inner", "layers": [p[1]]})).ok().and_then(|v| v["group"].as_u64());
            groups.zip(inner).is_some_and(|(g, i)| s.run("group.setParent", &json!({"group": i, "parent": g, "before": null})).is_ok())
        }
        "@classStyle" => {
            s.run("layer.symbology", &json!({"layer": p["layer"], "type": "unique", "field": "name"})).is_ok()
                && s.run("layer.classStyle", &p).is_ok()
        }
        "@poison" => {
            let (Some(id), Some(row), Some(v)) = (p[0].as_u64(), p[1].as_u64(), p[2].as_str().and_then(|v| v.parse::<f64>().ok())) else {
                return false;
            };
            let Some(l) = s.map_mut().layer_mut(id as u32) else { return false };
            let pop = l.dataset.fields.iter().position(|f| f.name == "pop");
            let cell = l.dataset.features.get_mut(row as usize).zip(pop).and_then(|(f, i)| f.attrs.get_mut(i));
            cell.map(|c| *c = rustgis_engine::data::Value::Float(v)).is_some()
        }
        _ => s.run(cmd, &p).is_ok(),
    }
}

/// The paths at which two saved projects differ, with both values.
fn diff(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                diff(&format!("{path}/{k}"), x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), out);
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                diff(&format!("{path}/{i}"), u, v, out);
            }
        }
        _ if a != b => {
            let short = |v: &Value| v.to_string().chars().take(80).collect::<String>();
            out.push(format!("{path}: {} → {}", short(a), short(b)));
        }
        _ => {}
    }
}

/// What the user sees of a project: everything it saves.
fn seen(s: &Session) -> Value {
    serde_json::to_value(&s.project).unwrap()
}

/// Each map's layers' date and date-time fields (name, type) and their values, as they are held. Other
/// fields are left out: a NaN is written as null and said so, and compares unequal to itself.
fn held_values(s: &Session) -> Vec<Vec<Vec<(String, FieldType, Vec<DataValue>)>>> {
    s.project
        .maps
        .iter()
        .map(|m| {
            m.layers
                .iter()
                .map(|l| {
                    let d = &l.dataset;
                    let dates = d.fields.iter().enumerate().filter(|(_, f)| matches!(f.ty, FieldType::Date | FieldType::DateTime));
                    dates
                        .map(|(i, f)| {
                            (f.name.clone(), f.ty, d.features.iter().map(|x| x.attrs.get(i).cloned().unwrap_or(DataValue::Null)).collect())
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// What notices since `mark` said, besides that a file was saved or opened.
fn said(s: &Session, mark: u64) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| !t.starts_with("Saved ") && !t.starts_with("Opened ")).collect()
}

/// A project: names for the four fixture layers and up to twelve edits.
fn project() -> impl Strategy<Value = (Vec<String>, Vec<(&'static str, Value)>)> {
    (prop::collection::vec(name(), 4), prop::collection::vec(step(), 0..12))
}

/// Builds the project in a new session, with how many edits changed something.
fn build(names: &[String], steps: &[(&'static str, Value)]) -> (Session, usize) {
    let mut s = Session::new(Project::new("Project"));
    let ids = layers(&mut s, names);
    s.run("layout.new", &json!({"name": "Page"})).unwrap();
    let applied = steps.iter().filter(|(cmd, p)| apply(&mut s, cmd, p, &ids)).count();
    (s, applied)
}

/// A seeded runner, as the convention says.
fn runner(default_cases: u32) -> (u64, TestRunner) {
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let cases = u32::try_from(env("PROPERTY_CASES", u64::from(default_cases))).unwrap_or(default_cases);
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    (seed, TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes)))
}

/// A private scratch folder for one test.
fn scratch(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustgis-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Saves `s` as `path`, and opens it in a new session: what each said, and the opened session.
fn save_and_open(s: &mut Session, path: &Path) -> Result<(Vec<String>, Session), String> {
    let mark = s.notice_mark();
    s.run("project.saveAs", &json!({"path": path}))?;
    let mut said_all = said(s, mark);
    let mut opened = Session::new(Project::new("Other"));
    let mark = opened.notice_mark();
    opened.run("project.open", &json!({"path": path}))?;
    said_all.extend(said(&opened, mark));
    Ok((said_all, opened))
}

#[test]
fn a_saved_project_opens_as_it_was_saved() {
    let dir = scratch("project-save");
    let path = dir.join("project.rgis");
    let (seed, mut runner) = runner(256);
    let (cases, edits, compared) = (std::cell::Cell::new(0), std::cell::Cell::new(0), std::cell::Cell::new(0));
    let result = runner.run(&project(), |(names, steps)| {
        let (mut s, applied) = build(&names, &steps);
        cases.set(cases.get() + usize::from(applied > 0));
        edits.set(edits.get() + applied);
        let before = seen(&s);
        let held = held_values(&s);
        let (notices, opened) = save_and_open(&mut s, &path).map_err(|e| TestCaseError::fail(format!("didn't save and open: {e}")))?;
        let mut changed = Vec::new();
        diff("", &before, &seen(&opened), &mut changed);
        compared.set(compared.get() + usize::from(notices.is_empty()));
        // Values that aren't finite numbers, which saving writes as null and says so, are null in
        // what the user sees before saving too (`Value`'s `Serialize`): nothing is excused.
        changed.retain(|c| !declared(c));
        // Dates and moments come back exactly: type, value and offset (`Value`'s `==` compares what
        // is stored).
        if held_values(&opened) != held {
            changed.push(format!("field types or values: {held:?} → {:?}", held_values(&opened)));
        }
        if !changed.is_empty() {
            return Err(TestCaseError::fail(format!("changed, saying {notices:?}: {changed:#?}")));
        }
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{} cases had edits, {} edits in all, {} compared without a notice", cases.get(), edits.get(), compared.get());
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}

/// Puts `key` in the `n`-th object of `v` (counting depth first), if it has that many.
fn insert_at(v: &mut Value, n: &mut usize, key: &str) -> bool {
    match v {
        Value::Object(o) => {
            if *n == 0 {
                o.insert(key.to_string(), json!({"from": "a newer RustGIS", "values": [1, 2]}));
                return true;
            }
            *n -= 1;
            o.values_mut().any(|v| insert_at(v, n, key))
        }
        Value::Array(a) => a.iter_mut().any(|v| insert_at(v, n, key)),
        _ => false,
    }
}

#[test]
fn a_project_from_a_newer_rustgis_opens_only_when_this_version_can_keep_it() {
    let dir = scratch("project-newer");
    let (clean, newer) = (dir.join("clean.rgis"), dir.join("newer.rgis"));
    let (seed, mut runner) = runner(128);
    let places = prop::collection::vec(0usize..400, 1..4);
    let raised = prop::option::of(prop::sample::select(vec![1u64, 2, 1000, u64::from(u32::MAX)]));
    let (opened, refused) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let result = runner.run(&(project(), places, raised), |((names, steps), places, raised)| {
        let (mut s, _) = build(&names, &steps);
        s.run("project.saveAs", &json!({"path": clean})).map_err(TestCaseError::fail)?;
        let mut file: Value = serde_json::from_str(&std::fs::read_to_string(&clean).unwrap()).unwrap();
        for (i, mut place) in places.into_iter().enumerate() {
            insert_at(&mut file, &mut place, &format!("futureField{i}"));
        }
        if let Some(k) = raised {
            file["format"] = json!(u64::from(rustgis_engine::project::READ_FORMAT) + k);
        }
        std::fs::write(&newer, file.to_string()).unwrap();
        let mut a = Session::new(Project::new("Other"));
        a.run("project.open", &json!({"path": clean})).map_err(TestCaseError::fail)?;
        // Refused, it leaves the project open as it was, layers, groups and layouts too.
        let (mut b, _) = build(&names, &steps);
        let before = seen(&b);
        let open = b.run("project.open", &json!({"path": newer}));
        let count = if open.is_ok() { &opened } else { &refused };
        count.set(count.get() + 1);
        match (open, raised) {
            (Ok(_), Some(k)) => Err(TestCaseError::fail(format!("a project of format +{k} opened"))),
            (Err(e), Some(_)) if !e.contains("Update RustGIS") || seen(&b) != before => {
                Err(TestCaseError::fail(format!("refused without saying to update, or changed the open project: {e}")))
            }
            // A field put where a newer RustGIS couldn't write one (beside an enum's variant, in a
            // list of values) makes a file this version can't read: refused, it changes nothing.
            (Err(e), None) if seen(&b) != before => Err(TestCaseError::fail(format!("a refused file changed the open project: {e}"))),
            (Ok(_), None) => {
                // Fields this version doesn't know are left out, as a newer RustGIS that writes
                // something an older one would lose raises the format, or lists it in `requires`
                // (docs/maintaining.md).
                let mut changed = Vec::new();
                diff("", &seen(&a), &seen(&b), &mut changed);
                changed.retain(|c| !declared(c));
                if changed.is_empty() {
                    Ok(())
                } else {
                    Err(TestCaseError::fail(format!("unknown fields changed the project: {changed:#?}")))
                }
            }
            _ => Ok(()),
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("{} opened, {} refused", opened.get(), refused.get());
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}
