//! Never silent (AGENTS.md), as a property: a generated layer exported with `share.exportLayer`
//! and added back with `layer.addData` comes back the same, or the export says something. What a
//! format can't hold and says nothing about is listed in [`LIMITS`], with the reason. Layers lean
//! on edges: names formats treat specially, extreme and non-finite numbers, odd text, nulls,
//! multipoints, the antimeridian and the poles.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the smallest layer found that changes without a notice. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=2000 cargo test -p rustgis-engine --test never_silent_roundtrip`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_engine::{
    Project, Session,
    data::{Coord, Dataset, Date, Feature, Field, FieldType, Geometry, GeometryType, Value},
};
use serde_json::json;

const FORMATS: [&str; 8] = ["geojson", "csv", "fgb", "gpkg", "parquet", "shp", "kml", "gpx"];

/// Formats that hold only features with geometry: an export to one leaves the others out and says
/// so, and what comes back is compared with the layer without them.
const GEOMETRY_ONLY: [&str; 4] = ["fgb", "shp", "kml", "gpx"];

/// Changes a format makes without a notice, as (format, the start of a change from [`changes`],
/// why). Each is what the format can hold or how its reader works, not a loss of a value's
/// number or text, except trimming (Shapefile, KML, GPX), blanks read as null and (KML, GPX) a
/// field of nulls left out, which are kept here until they are said (#1907). CSV keeps the spaces
/// around a value (#2059).
const LIMITS: &[(&str, &str, &str)] = &[
    // CSV holds text: reading types each column from its values (`text_type`) and reads an empty
    // cell as null.
    ("csv", "blank text became null", "an empty cell is read as null"),
    ("csv", "field type changed", "a column's type comes from its values"),
    ("csv", "field of nulls type changed", "a column's type comes from its values"),
    ("csv", "boolean became text", "yes/no values are written as True/False, read back as text"),
    ("csv", "date became text", "dates are written as ISO 8601 text, and a CSV date stays text on read (#853)"),
    ("csv", "datetime became text", "dates are written as ISO 8601 text, and a CSV date stays text on read (#853)"),
    ("csv", "number became text", "a column with a value that isn't a number is text"),
    ("csv", "text became a number", "a column of numbers is a number column"),
    ("csv", "infinity or NaN became text", "inf and NaN aren't read as numbers"),
    ("csv", "not read back: nothing", "a layer without geometry or values is an empty file"),
    // GeoJSON types a property from its values.
    ("geojson", "field of nulls type changed", "a property that is null everywhere has no type"),
    ("geojson", "field type changed Date → Text", "a date is written as ISO 8601 text and stays text on read (#853)"),
    ("geojson", "field type changed DateTime → Text", "a date is written as ISO 8601 text and stays text on read (#853)"),
    ("geojson", "date became text", "a date is written as ISO 8601 text and stays text on read (#853)"),
    ("geojson", "datetime became text", "a date is written as ISO 8601 text and stays text on read (#853)"),
    ("geojson", "not read back: nothing", "a layer without geometry or values reads as nothing"),
    // dBase pads text with spaces and has no null text; reading makes a whole-number column an
    // integer one.
    ("shp", "text trimmed", "dBase pads text with spaces after it, and reading drops them, so spaces at its end go"),
    ("shp", "blank text became null", "dBase has no null text: blank text reads as null"),
    ("shp", "field of nulls type changed", "a number column's type comes from its values"),
    ("shp", "field type changed Double → Integer", "a column of whole numbers reads as integers"),
];

/// KML and GPX hold text, read with types from the values as for CSV (`type_text_columns`); the
/// name field is written as the feature's name and read back as `name`, and a field that is
/// null everywhere isn't in the file.
const TEXT_FORMAT_LIMITS: &[&str] = &[
    "text trimmed",
    "blank text became null",
    "field type changed",
    "field of nulls type changed",
    "field of nulls missing",
    "field renamed",
    "boolean became text",
    "date became text",
    "datetime became text",
    "number became text",
    "text became a number",
    "infinity or NaN became text",
];

/// Whether `change` is a limit of `ext` listed above.
fn declared(ext: &str, change: &str) -> bool {
    LIMITS.iter().any(|(f, start, _)| *f == ext && change.starts_with(start))
        || matches!(ext, "kml" | "gpx") && TEXT_FORMAT_LIMITS.iter().any(|start| change.starts_with(start))
}

/// A number from the environment, else `default`.
fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A longitude/latitude, sometimes on an edge: ±180°, the poles, past Web Mercator's 85.05°.
fn coord() -> impl Strategy<Value = Coord> {
    let lon = prop_oneof![4 => -180.0..180.0f64, 1 => prop::sample::select(vec![-180.0, 180.0, 0.0, 1e-9, 179.999_999_999])];
    let lat = prop_oneof![4 => -89.0..89.0f64, 1 => prop::sample::select(vec![-90.0, 90.0, 0.0, 85.06, -85.06])];
    (lon, lat).prop_map(|(x, y)| Coord::new(x, y))
}

/// A closed ring of three to six points around a centre.
fn ring() -> impl Strategy<Value = Vec<Coord>> {
    (coord(), 0.01..5.0f64, 3usize..7).prop_map(|(c, r, n)| {
        let at = |i: usize| {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            Coord::new((c.x * 0.9 + r * a.cos()).clamp(-180.0, 180.0), (c.y * 0.9 + r * a.sin()).clamp(-89.0, 89.0))
        };
        (0..=n).map(|i| at(i % n)).collect()
    })
}

/// A geometry of a layer of `kind`: a point layer's points and multipoints, lines of one or two
/// parts, polygons of one or two parts.
fn geometry(kind: GeometryType) -> BoxedStrategy<Geometry> {
    match kind {
        GeometryType::Point => prop_oneof![
            3 => coord().prop_map(Geometry::Point),
            1 => prop::collection::vec(coord(), 1..4).prop_map(Geometry::MultiPoint),
        ]
        .boxed(),
        GeometryType::Polyline => prop::collection::vec(prop::collection::vec(coord(), 2..5), 1..3).prop_map(Geometry::Line).boxed(),
        _ => prop::collection::vec(prop::collection::vec(ring(), 1..2), 1..3).prop_map(Geometry::Polygon).boxed(),
    }
}

/// Field names, leaning on ones that formats treat specially.
fn field_name() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-z][a-z0-9_]{0,7}",
        1 => prop::sample::select(vec![
            "id", "fid", "name", "NAME", "geometry", "a_very_long_field_name", "a_very_long_field_other", "Ünïcode", "名前",
            "with space", "x", "y", "lat", "lon", "longitude", "WKT", "1st",
        ])
        .prop_map(String::from),
    ]
}

/// Days and moments on the edges of the model: the ends of years 1 to 9999, the days around the
/// epoch, leap days, microseconds, no zone, `Z` and odd offsets.
const DATES: [&str; 12] = [
    "0001-01-01",
    "9999-12-31",
    "1969-12-31",
    "1970-01-01",
    "1900-02-28",
    "1900-03-01",
    "2000-02-29",
    "2024-02-29",
    "2026-03-05",
    "1582-10-15",
    "0001-12-31",
    "9999-01-01",
];
/// Wall-clock times with and without microseconds, at the ends of years 1 to 9999.
const LOCAL_TIMES: [&str; 12] = [
    "0001-01-01T00:00:00",
    "9999-12-31T23:59:59.999999",
    "1969-12-31T23:59:59.999999",
    "1970-01-01T00:00:00",
    "1970-01-01T00:00:00.000001",
    "2000-02-29T12:00:00.5",
    "2024-02-29T23:59:59.123456",
    "2026-03-05T14:30:00",
    "2026-03-05T00:00:00",
    "1900-02-28T08:15:00",
    "2026-03-05T09:00:00.100",
    "1900-03-01T23:59:59",
];
/// No zone, `Z` and odd offsets.
const ZONES: [&str; 7] = ["", "Z", "+05:45", "-09:30", "+14:00", "-12:00", "+00:30"];

/// Values of a field's type, leaning on edges: extremes, NaN, long and odd text, nulls.
fn value(ty: FieldType) -> BoxedStrategy<Value> {
    let v = match ty {
        FieldType::Integer => prop_oneof![
            4 => (-1000i64..1000).prop_map(Value::Int),
            1 => prop::sample::select(vec![i64::MAX, i64::MIN, 2i64.pow(53) + 1, 2_147_483_648]).prop_map(Value::Int),
        ]
        .boxed(),
        FieldType::Double => prop_oneof![
            4 => (-1e6..1e6f64).prop_map(Value::Float),
            1 => prop::sample::select(vec![0.1, -0.0, 1e300, 5e-324, 0.1234567891, f64::NAN, f64::INFINITY]).prop_map(Value::Float),
        ]
        .boxed(),
        FieldType::Date => prop_oneof![
            3 => (-719_162i64..=2_932_896).prop_map(|n| Date::from_days(n).map_or(Value::Null, Value::Date)),
            3 => prop::sample::select(&DATES).prop_map(|t| Value::parse(t, FieldType::Date)),
        ]
        .boxed(),
        FieldType::DateTime => prop_oneof![
            2 => prop::sample::select(&LOCAL_TIMES).prop_map(|t| Value::parse(t, FieldType::DateTime)),
            4 => (prop::sample::select(&LOCAL_TIMES), prop::sample::select(&ZONES))
                .prop_map(|(t, z)| Value::parse(&format!("{t}{z}"), FieldType::DateTime)),
        ]
        .boxed(),
        FieldType::Text => prop_oneof![
            4 => "[a-zA-Z0-9 ]{0,12}".prop_map(Value::Text),
            1 => prop::sample::select(vec![
                "", " lead", "comma, here", "quote \"x\"", "line\nbreak", "Ünïcødé 名前 😀", "NULL", "123", "true", "=1+1", "<b>&amp;</b>",
            ])
            .prop_map(|s| Value::Text(s.to_string())),
            1 => (1usize..400).prop_map(|n| Value::Text("é".repeat(n))),
            // Digits a Double doesn't hold: identifiers past i64, and decimals past 17 digits.
            1 => prop::sample::select(vec![
                "12345678901234567891", "12345678901234567000", "9007199254740993", "0.12345678901234567891", "-99999999999999999999",
            ])
            .prop_map(|s| Value::Text(s.to_string())),
        ]
        .boxed(),
        _ => any::<bool>().prop_map(Value::Bool).boxed(),
    };
    prop_oneof![6 => v, 1 => Just(Value::Null)].boxed()
}

/// A layer: a geometry kind, up to four fields of any type, up to four features (a few without
/// geometry).
fn layer() -> impl Strategy<Value = Dataset> {
    let kind = prop::sample::select(vec![GeometryType::Point, GeometryType::Polyline, GeometryType::Polygon]);
    let types = prop::sample::select(vec![
        FieldType::Integer,
        FieldType::Double,
        FieldType::Text,
        FieldType::Boolean,
        FieldType::Date,
        FieldType::DateTime,
    ]);
    let fields = prop::collection::vec((field_name(), types), 0..5).prop_map(|fs| {
        let mut seen = std::collections::HashSet::new();
        fs.into_iter().filter(|(n, _)| seen.insert(n.clone())).map(|(n, t)| Field::new(n, t)).collect::<Vec<_>>()
    });
    (kind, fields).prop_flat_map(|(kind, fields)| {
        let attrs: Vec<_> = fields.iter().map(|f| value(f.ty)).collect();
        let feature = (prop::option::weighted(0.9, geometry(kind)), attrs);
        prop::collection::vec(feature, 1..5).prop_map(move |features| {
            let mut ds = Dataset::new("layer", kind, fields.clone());
            for (g, attrs) in features {
                ds.push(g, attrs);
            }
            ds
        })
    })
}

/// What a trip says: the export, besides naming the file, and reading the file back.
struct Said {
    export: Vec<String>,
    read: Vec<String>,
}

/// Exports `ds` as `ext` and adds the file back: what was said, and the layer read back, or why
/// there is none. `Err` with why the export refused the layer, which isn't silent.
fn trip(ds: &Dataset, ext: &str, dir: &std::path::Path) -> Result<(Said, Result<Dataset, String>), String> {
    let path = dir.join(format!("layer.{ext}"));
    let mut s = Session::new(Project::default());
    s.map_mut().add_dataset(ds.clone());
    let mark = s.notice_mark();
    s.run("share.exportLayer", &json!({"layer": "layer", "path": path}))?;
    let export: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| !t.starts_with("Exported ")).collect();
    let before: Vec<u32> = s.map().layers.iter().map(|l| l.id).collect();
    let mark = s.notice_mark();
    let added = s.run("layer.addData", &json!({"path": path}));
    let said = Said { export, read: s.notices_since(mark).iter().map(|n| n.text.clone()).collect() };
    if let Err(e) = added {
        return Ok((said, Err(e)));
    }
    // A format read as several layers (GPX: tracks and their points) gives back the one of the
    // layer's kind.
    let added: Vec<&Dataset> = s.map().layers.iter().filter(|l| !before.contains(&l.id)).map(|l| &l.dataset).collect();
    let back = added.iter().find(|d| d.geometry_type == ds.geometry_type).or(added.first()).map(|d| (*d).clone());
    Ok((said, back.ok_or_else(|| "nothing".to_string())))
}

/// The value's type, to name a change.
fn tag(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Int(_) => "integer",
        Value::Float(_) => "number",
        Value::Text(_) => "text",
        Value::Date(_) => "date",
        Value::DateTime(_) => "datetime",
        _ => "boolean",
    }
}

/// Whether the number `v` keeps every digit of the text `t`: a whole number exactly, a decimal at
/// the precision of its digits, as the readers type text (`text_type`).
fn keeps_digits(t: &str, v: &Value) -> bool {
    let t = t.trim();
    let Value::Float(f) = v else { return true };
    let digits = |s: &str| {
        let m = s.split(['e', 'E']).next().unwrap_or_default();
        m.chars().filter(char::is_ascii_digit).collect::<String>().trim_start_matches('0').trim_end_matches('0').to_string()
    };
    let whole = t.strip_prefix(['-', '+']).unwrap_or(t);
    if whole.bytes().all(|b| b.is_ascii_digit()) {
        let d = whole.trim_start_matches('0');
        return format!("{:.0}", f.abs()) == if d.is_empty() { "0" } else { d };
    }
    let want = digits(t);
    want.is_empty() || digits(&format!("{f:.*e}", want.len() - 1)) == want
}

/// How a value changed, if it did; the same number in another type isn't a change. The kinds
/// name what a format that holds text does to a value, so [`LIMITS`] can allow just that.
fn value_change(a: &Value, b: &Value) -> Option<String> {
    let same = match (a, b) {
        (Value::Float(x), Value::Float(y)) => x == y || x.is_nan() && y.is_nan(),
        (Value::Int(x), Value::Int(y)) => x == y,
        // A whole number in the other type, exactly.
        (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => {
            (-(2f64.powi(63))..2f64.powi(63)).contains(f) && f.fract() == 0.0 && *f as i64 == *i
        }
        _ => a == b,
    };
    if same {
        return None;
    }
    // Text that reads as the number exactly.
    let reads_as = |t: &str, v: &Value| match v {
        Value::Int(i) => t.trim().parse::<i64>() == Ok(*i),
        Value::Float(f) => t.trim().parse::<f64>().is_ok_and(|m| m == *f),
        _ => false,
    };
    Some(match (a, b) {
        (Value::Text(x), Value::Null) if x.trim().is_empty() => "blank text became null".into(),
        (Value::Text(x), Value::Text(y)) if x.trim() == y.trim() => "text trimmed".into(),
        (Value::Text(x), _) if reads_as(x, b) && keeps_digits(x, b) => "text became a number".into(),
        (Value::Float(x), Value::Text(y))
            if !x.is_finite() && y.trim().parse::<f64>().is_ok_and(|m| m == *x || m.is_nan() && x.is_nan()) =>
        {
            "infinity or NaN became text".into()
        }
        (_, Value::Text(y)) if reads_as(y, a) => "number became text".into(),
        (Value::Bool(x), Value::Text(y)) if y == if *x { "True" } else { "False" } => "boolean became text".into(),
        // A date as its ISO 8601 text, which is how it is shown; any other text is another date.
        (Value::Date(_) | Value::DateTime(_), Value::Text(y)) if *y == a.to_string() => format!("{} became text", tag(a)),
        // Another value, which no limit allows.
        _ if tag(a) == tag(b) => format!("{} changed", tag(a)),
        _ => format!("{} changed to {}", tag(a), tag(b)),
    })
}

/// A geometry's parts, with exact coordinates, each ring from its least point in one direction, a
/// polygon's holes and a multipart polygon's parts in order: what a format may rearrange without
/// changing the shape.
fn shape(g: &Geometry) -> Vec<Vec<Vec<(u64, u64)>>> {
    let q = |c: &Coord| (c.x.to_bits(), c.y.to_bits());
    let ring = |r: &[Coord]| {
        let mut r: Vec<_> = r.iter().map(q).collect();
        if r.len() > 1 && r.first() == r.last() {
            r.pop();
        }
        let start = |r: &[(u64, u64)]| {
            let mut r = r.to_vec();
            let i = r.iter().enumerate().min_by_key(|(_, p)| **p).map_or(0, |(i, _)| i);
            r.rotate_left(i);
            r
        };
        let forward = start(&r);
        r.reverse();
        forward.min(start(&r))
    };
    match g {
        Geometry::Point(c) => vec![vec![vec![q(c)]]],
        Geometry::MultiPoint(cs) => cs.iter().map(|c| vec![vec![q(c)]]).collect(),
        Geometry::Line(ls) => ls.iter().map(|l| vec![l.iter().map(q).collect()]).collect(),
        Geometry::Polygon(ps) => {
            let mut parts: Vec<Vec<_>> = ps
                .iter()
                .map(|p| {
                    let mut rings: Vec<_> = p.iter().map(|r| ring(r)).collect();
                    if let Some(holes) = rings.get_mut(1..) {
                        holes.sort();
                    }
                    rings
                })
                .collect();
            parts.sort();
            parts
        }
    }
}

/// A change, and the field it is in, if any: a notice of it names that field.
type Change = (String, Option<String>);

/// What changed in one feature: its geometry and the values of the fields in `map`.
fn feature_changes(x: &Feature, y: &Feature, map: &[(usize, Option<usize>)], fields: &[Field]) -> Vec<Change> {
    let mut out = Vec::new();
    match (&x.geometry, &y.geometry) {
        (Some(g), Some(h)) if shape(g) != shape(h) => out.push(("geometry changed".to_string(), None)),
        (Some(_), None) => out.push(("geometry dropped".into(), None)),
        (None, Some(_)) => out.push(("geometry made up".into(), None)),
        _ => {}
    }
    for &(i, j) in map {
        let (Some(j), Some(f)) = (j, fields.get(i)) else { continue };
        let (a, b) = (x.attrs.get(i).unwrap_or(&Value::Null), y.attrs.get(j).unwrap_or(&Value::Null));
        if let Some(why) = value_change(a, b) {
            out.push((format!("{why} ({}: {a:?} → {b:?})", f.name), Some(f.name.clone())));
        }
    }
    out
}

/// The renames notices say, written `'old' → 'new'`.
fn renames(notices: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for n in notices {
        for (i, _) in n.match_indices("' → '") {
            let old = n[..i].rsplit('\'').next().unwrap_or_default();
            let new = n[i + "' → '".len()..].split('\'').next().unwrap_or_default();
            out.push((old.to_string(), new.to_string()));
        }
    }
    out
}

/// Whether `change`, in `field` if any, is said: by a notice naming the field, the export's or the
/// reading's, or by any export notice for a change in no field. A notice that renames a field
/// (`'a' → 'a_1'`) says the rename, not a change in the values of either name.
fn is_said(change: &str, field: Option<&str>, said: &Said) -> bool {
    let Some(f) = field else { return !said.export.is_empty() };
    said.export.iter().chain(&said.read).any(|n| {
        let mut n = n.clone();
        if !change.starts_with("field renamed") {
            for (old, new) in renames(std::slice::from_ref(&n)) {
                n = n.replace(&format!("'{old}' → '{new}'"), "");
            }
        }
        n.contains(&format!("'{f}'"))
    })
}

/// What changed between a layer and the layer read back. Features are matched by content, since
/// a format may reorder them (FlatGeobuf sorts them along its spatial index); fields by name, and
/// a format may add its own. `renamed` pairs a field with the column it became.
fn changes(a: &Dataset, b: &Dataset, renamed: &[(String, String)]) -> Vec<Change> {
    let mut out = Vec::new();
    let mut map = Vec::new();
    // Each column read back stands for one field at most: the names notices said first, then
    // exact names, then names that differ in case.
    let mut taken = vec![false; b.fields.len()];
    let mut exact: Vec<Option<usize>> = vec![None; a.fields.len()];
    for (i, f) in a.fields.iter().enumerate() {
        let new = renamed.iter().find(|(old, _)| *old == f.name).map(|(_, new)| new);
        let j = new.and_then(|new| (0..b.fields.len()).find(|&j| !taken[j] && b.fields[j].name == *new));
        if let Some(j) = j {
            taken[j] = true;
            exact[i] = Some(j);
        }
    }
    for (i, f) in a.fields.iter().enumerate() {
        if exact[i].is_some() {
            continue;
        }
        let j = (0..b.fields.len()).find(|&j| !taken[j] && b.fields[j].name == f.name);
        if let Some(j) = j {
            taken[j] = true;
        }
        exact[i] = j;
    }
    for (i, f) in a.fields.iter().enumerate() {
        // A field holding only nulls gives a reader that types fields from their values nothing
        // to go on.
        let nulls = if a.features.iter().all(|x| x.attrs.get(i).is_none_or(|v| *v == Value::Null)) { " of nulls" } else { "" };
        let exact = exact.get(i).copied().flatten();
        let j = exact.or_else(|| (0..b.fields.len()).find(|&j| !taken[j] && b.fields[j].name.eq_ignore_ascii_case(&f.name)));
        if let Some(j) = j {
            taken[j] = true;
        }
        // A name and a type change each count.
        let changed = match j.and_then(|j| b.fields.get(j)) {
            None => vec![format!("field{nulls} missing ({})", f.name)],
            Some(g) => [
                (g.name != f.name).then(|| format!("field renamed ({} → {})", f.name, g.name)),
                (g.ty != f.ty).then(|| format!("field{nulls} type changed {:?} → {:?} ({})", f.ty, g.ty, f.name)),
            ]
            .into_iter()
            .flatten()
            .collect(),
        };
        out.extend(changed.into_iter().map(|c| (c, Some(f.name.clone()))));
        map.push((i, j));
    }
    let (fa, fb) = (a.features.iter().collect::<Vec<_>>(), b.features.iter().collect::<Vec<_>>());
    if fa.len() != fb.len() {
        out.push((format!("feature count changed ({} → {})", fa.len(), fb.len()), None));
        return out;
    }
    let mut used = vec![false; fb.len()];
    for x in fa {
        let best =
            (0..fb.len()).filter(|&j| !used[j]).map(|j| (feature_changes(x, fb[j], &map, &a.fields), j)).min_by_key(|(c, _)| c.len());
        if let Some((c, j)) = best {
            used[j] = true;
            out.extend(c);
        }
    }
    out
}

#[test]
fn an_export_that_changes_a_layer_says_so() {
    let dir = std::env::temp_dir().join(format!("rustgis-never-silent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    // About 6 ms a case.
    let cases = u32::try_from(env("PROPERTY_CASES", 256)).unwrap_or(256);
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    // Layers each format wrote: one that refuses every layer checks nothing.
    let written = std::cell::RefCell::new(std::collections::HashMap::<&str, u32>::new());
    let result = runner.run(&layer(), |ds| {
        for ext in FORMATS {
            let sub = dir.join(ext);
            let _ = std::fs::remove_dir_all(&sub);
            std::fs::create_dir_all(&sub).unwrap();
            // A refusal says why, but a crash caught by `Session::run`, writing or reading, is a bug.
            let crashed = |e: &str| e.contains("failed unexpectedly") || e.contains("stopped unexpectedly");
            let (said, back) = match trip(&ds, ext, &sub) {
                Ok((_, Err(e))) | Err(e) if crashed(&e) => return Err(TestCaseError::fail(format!("{ext}: {e}"))),
                Ok(trip) => trip,
                Err(_) => continue,
            };
            *written.borrow_mut().entry(ext).or_default() += 1;
            let mut held = ds.clone();
            if GEOMETRY_ONLY.contains(&ext) {
                held.features.retain(|f| f.geometry.is_some());
                if held.features.len() < ds.features.len() && !said.export.iter().any(|n| n.contains("left out")) {
                    return Err(TestCaseError::fail(format!(
                        "{ext}: features without geometry left out without a notice; said: {:?}",
                        said.export
                    )));
                }
            }
            // An export that says it wrote NaN and infinity as null holds them as null.
            if said.export.iter().any(|n| n.contains("aren't finite numbers")) {
                for v in held.features.iter_mut().flat_map(|f| f.attrs.iter_mut()) {
                    if matches!(v, Value::Float(x) if !x.is_finite()) {
                        *v = Value::Null;
                    }
                }
            }
            let found = back.map_or_else(
                |e| vec![(format!("not read back: {e}"), None)],
                |back| {
                    let notices: Vec<String> = said.export.iter().chain(&said.read).cloned().collect();
                    let mut renamed = renames(&notices);
                    // KML and GPX write the first field named like `name` as each feature's
                    // name, which reads back as `name`.
                    if matches!(ext, "kml" | "gpx")
                        && let Some(f) = held.fields.iter().find(|f| f.name.eq_ignore_ascii_case("name"))
                    {
                        renamed.insert(0, (f.name.clone(), "name".to_string()));
                    }
                    changes(&held, &back, &renamed)
                },
            );
            // A change is said by a notice naming its field, the export's or the reading's, or by
            // any export notice for one in no field (geometry, the feature count): a notice of
            // geometry names no field to look for, so this half is looser.
            let silent: Vec<String> =
                found.into_iter().filter(|(c, field)| !declared(ext, c) && !is_said(c, field.as_deref(), &said)).map(|(c, _)| c).collect();
            if !silent.is_empty() {
                return Err(TestCaseError::fail(format!(
                    "{ext}: changed without a notice: {silent:?}; said: {:?}, {:?}",
                    said.export, said.read
                )));
            }
        }
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
    let written = written.into_inner();
    for ext in FORMATS {
        assert!(written.get(ext).is_some_and(|n| *n > 0), "PROPERTY_SEED={seed}: {ext} wrote none of the layers");
    }
}

#[test]
fn a_rename_notice_says_the_rename_not_a_change_in_values() {
    let said = |n: &str| Said { export: vec![n.to_string()], read: Vec::new() };
    let renamed = said("l.gpkg: 2 field names were changed to fit a GeoPackage: 'name' → 'name_1', 'name_1' → 'name_1_1'");
    assert!(is_said("field renamed (name_1 → name_1_1)", Some("name_1"), &renamed));
    assert!(!is_said("number changed (name_1: Int(1) → Int(2))", Some("name_1"), &renamed));
    let values = said("l.shp: 1 value of 'name_1' was cut to fit");
    assert!(is_said("number changed (name_1: Int(1) → Int(2))", Some("name_1"), &values));
    assert!(!is_said("number changed (other: Int(1) → Int(2))", Some("other"), &values));
}
