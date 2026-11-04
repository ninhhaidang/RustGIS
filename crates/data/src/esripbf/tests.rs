use super::*;
use crate::raster::Crs;

// Encoding, as `FeatureCollection.proto` lays answers out.

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn number(field: u64, v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, field << 3);
    varint(&mut out, v);
    out
}

fn message(field: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (field << 3) | 2);
    varint(&mut out, bytes.len() as u64);
    out.extend_from_slice(bytes);
    out
}

fn double(field: u64, v: f64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (field << 3) | 1);
    out.extend_from_slice(&v.to_le_bytes());
    out
}

fn float(field: u64, v: f32) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (field << 3) | 5);
    out.extend_from_slice(&v.to_le_bytes());
    out
}

fn packed(values: impl IntoIterator<Item = u64>) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        varint(&mut out, v);
    }
    out
}

/// An answer: a version, then a query result holding `result`'s fields as its feature result.
fn answer(result: &[Vec<u8>]) -> Vec<u8> {
    [message(1, b"10.0"), message(2, &message(1, &result.concat()))].concat()
}

/// A spatial reference of longitude/latitude (WKID 4326), as services send with `outSR=4326`.
fn lonlat() -> Vec<u8> {
    message(8, &number(1, 4326))
}

fn field(name: &str, ty: u64) -> Vec<u8> {
    message(13, &[message(1, name.as_bytes()), number(2, ty)].concat())
}

/// A transform: lower-left or upper-left origin, X and Y scale and translate.
fn transform(upper_left: bool, scale: f64, x0: f64, y0: f64) -> Vec<u8> {
    message(
        12,
        &[
            number(1, if upper_left { 0 } else { 1 }),
            message(2, &[double(1, scale), double(2, scale)].concat()),
            message(3, &[double(1, x0), double(2, y0)].concat()),
        ]
        .concat(),
    )
}

fn text(s: &str) -> Vec<u8> {
    message(1, &message(1, s.as_bytes()))
}
fn sint64(v: i64) -> Vec<u8> {
    message(1, &number(8, zz(v)))
}
fn uint(v: u64) -> Vec<u8> {
    message(1, &number(5, v))
}
fn uint64(v: u64) -> Vec<u8> {
    message(1, &number(7, v))
}
fn single(v: f32) -> Vec<u8> {
    message(1, &float(2, v))
}
fn real(v: f64) -> Vec<u8> {
    message(1, &double(3, v))
}
fn null() -> Vec<u8> {
    message(1, &number(10, 1))
}

/// A geometry of parts `lengths` (none: one part) and coordinates, X and Y as differences.
fn geometry(lengths: &[u64], coords: &[i64]) -> Vec<u8> {
    let mut g = Vec::new();
    if !lengths.is_empty() {
        g.extend(message(2, &packed(lengths.iter().copied())));
    }
    g.extend(message(3, &packed(coords.iter().map(|&c| zz(c)))));
    message(2, &g)
}

fn feature(parts: &[Vec<u8>]) -> Vec<u8> {
    message(15, &parts.concat())
}

fn c(x: f64, y: f64) -> Coord {
    Coord::new(x, y)
}

fn err(bytes: &[u8]) -> String {
    match read(bytes, "t") {
        Ok(_) => panic!("read an answer that should be refused"),
        Err(e) => e.to_string(),
    }
}

/// A page of a points layer, for paging tests: its object ids `ids` in a field named `oid`, each a
/// point at (id, 1), and whether the service says it holds more (`exceededTransferLimit`).
pub(crate) fn page(ids: &[u64], more: bool, oid: &str) -> Vec<u8> {
    let mut parts = vec![number(7, POINT), lonlat(), transform(false, 1.0, 0.0, 0.0), field(oid, OID)];
    if more {
        parts.push(number(9, 1));
    }
    parts.extend(ids.iter().map(|&id| feature(&[uint(id), geometry(&[], &[i64::try_from(id).unwrap_or(0), 1])])));
    answer(&parts)
}

/// Two points with every kind of field, as the services of the date test answered (2026-11-02):
/// dates as `sint64` milliseconds, object ids as `uint` or `uint64`, date-only values as text.
fn points() -> Vec<u8> {
    answer(&[
        number(7, POINT),
        lonlat(),
        transform(false, 0.5, 10.0, 20.0),
        field("OBJECTID", OID),
        field("NAME", 4),
        field("MAG", 2),
        field("DEPTH", 3),
        field("WHEN", 5),
        field("DAY", 14),
        field("BIG", 13),
        feature(&[
            uint64(7),
            text("Bogotá"),
            single(6.3),
            real(-2.5),
            sint64(1_785_903_246_000),
            text("1991-01-01"),
            sint64(10_000_000_000),
            geometry(&[], &[4, 6]),
        ]),
        feature(&[uint(8), null(), null(), null(), null(), null(), null(), geometry(&[], &[-2, 0])]),
    ])
}

#[test]
fn fields_keep_the_values_the_same_query_gives_as_geojson() {
    let page = read_page(&points(), "quakes").unwrap();
    let ds = &page.dataset;
    let types: Vec<(&str, FieldType)> = ds.fields.iter().map(|f| (f.name.as_str(), f.ty)).collect();
    assert_eq!(
        types,
        [
            ("OBJECTID", FieldType::Integer),
            ("NAME", FieldType::Text),
            ("MAG", FieldType::Double),
            ("DEPTH", FieldType::Double),
            ("WHEN", FieldType::Integer),
            ("DAY", FieldType::Text),
            ("BIG", FieldType::Integer),
        ]
    );
    let first = &ds.features[0];
    assert_eq!(
        first.attrs,
        [
            Value::Int(7),
            Value::Text("Bogotá".into()),
            // The single-precision value's shortest digits, not 6.300000190734863.
            Value::Float(6.3),
            Value::Float(-2.5),
            Value::Int(1_785_903_246_000),
            Value::Text("1991-01-01".into()),
            Value::Int(10_000_000_000),
        ]
    );
    assert_eq!(ds.features[1].attrs[0], Value::Int(8));
    assert!(ds.features[1].attrs[1..].iter().all(|v| *v == Value::Null));
    // Numbered by place, as GeoJSON is; the object id stays a field.
    assert_eq!(ds.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(page.object_id_field, Some(0));
    assert_eq!(ds.geometry_type, GeometryType::Point);
    assert_eq!(first.geometry, Some(Geometry::Point(c(12.0, 23.0))));
    assert_eq!(ds.features[1].geometry, Some(Geometry::Point(c(9.0, 20.0))));
    assert!(!page.exceeded_transfer_limit);
    assert!(page.report.messages.is_empty(), "{:?}", page.report.messages);
    assert!(!ds.zm_dropped && ds.crs.is_none());
}

#[test]
fn line_parts_start_over_and_an_upper_left_origin_counts_down() {
    let bytes = answer(&[
        number(7, POLYLINE),
        lonlat(),
        transform(true, 1.0, 0.0, 100.0),
        feature(&[geometry(&[2, 2], &[1, 1, 2, 0, 5, 5, 0, 1])]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.geometry_type, GeometryType::Polyline);
    assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![vec![c(1.0, 99.0), c(3.0, 99.0)], vec![c(5.0, 95.0), c(5.0, 94.0)]])));
    assert!(report.messages.is_empty());
}

#[test]
fn clockwise_rings_start_polygons_and_counterclockwise_ones_are_their_holes() {
    #[rustfmt::skip]
    let coords = [
        // Outer, clockwise: (0 0) (0 10) (10 10) (10 0) (0 0).
        0, 0, 0, 10, 10, 0, 0, -10, -10, 0,
        // Hole, counterclockwise: (2 2) (4 2) (4 4) (2 4) (2 2).
        2, 2, 2, 0, 0, 2, -2, 0, 0, -2,
        // A second outer ring: (20 0) (20 5) (25 5) (25 0) (20 0).
        20, 0, 0, 5, 5, 0, 0, -5, -5, 0,
    ];
    let bytes = answer(&[number(7, POLYGON), lonlat(), transform(false, 1.0, 0.0, 0.0), feature(&[geometry(&[5, 5, 5], &coords)])]);
    let (ds, report) = read(&bytes, "t").unwrap();
    let Some(Geometry::Polygon(polygons)) = &ds.features[0].geometry else { panic!("not a polygon") };
    assert_eq!(polygons.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(polygons[0][1][0], c(2.0, 2.0));
    assert_eq!(polygons[1][0][0], c(20.0, 0.0));
    assert!(report.messages.is_empty());
}

#[test]
fn a_geometry_without_coordinates_is_no_geometry() {
    // As `Water_Network/FeatureServer/3` sends a line without vertices: its type alone.
    let empty = message(2, &number(1, POLYLINE));
    let bytes = answer(&[number(7, POLYLINE), transform(false, 1.0, 0.0, 0.0), feature(&[empty])]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.features[0].geometry, None);
    assert_eq!(ds.geometry_type, GeometryType::Polyline);
    assert!(report.messages.is_empty());
}

#[test]
fn a_ring_without_area_is_left_out_and_said() {
    // A clockwise square, then three points on a line.
    let coords = [0, 0, 0, 10, 10, 0, 0, -10, -10, 0, 0, 0, 5, 0, 5, 0];
    let bytes = answer(&[number(7, POLYGON), lonlat(), transform(false, 1.0, 0.0, 0.0), feature(&[geometry(&[5, 3], &coords)])]);
    let (ds, report) = read(&bytes, "t").unwrap();
    let Some(Geometry::Polygon(polygons)) = &ds.features[0].geometry else { panic!("not a polygon") };
    assert_eq!(polygons.len(), 1);
    assert_eq!(report.messages, ["1 polygon rings have no area and are left out"]);
}

#[test]
fn z_and_m_are_dropped_and_said_even_when_they_are_all_alike() {
    let bytes = answer(&[
        number(10, 1),
        number(11, 1),
        lonlat(),
        transform(false, 1.0, 0.0, 0.0),
        feature(&[geometry(&[], &[4, 6, 100, 3])]),
        feature(&[geometry(&[], &[1, 1, 100, 3])]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.features[0].geometry, Some(Geometry::Point(c(4.0, 6.0))));
    assert!(ds.zm_dropped);
    assert_eq!(report.messages, [crate::zm_note(Some(2))]);
}

#[test]
fn web_mercator_answers_are_turned_to_longitude_latitude() {
    let bytes = answer(&[
        message(8, &[number(1, 102_100), number(2, 3857)].concat()),
        transform(false, 1.0, 0.0, 0.0),
        feature(&[geometry(&[], &[1_113_195, 0])]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    let Some(Geometry::Point(p)) = ds.features[0].geometry else { panic!("not a point") };
    assert!((p.x - 10.0).abs() < 1e-4 && p.y.abs() < 1e-9, "{p:?}");
    assert_eq!(ds.crs, Some(Crs::WebMercator));
    assert!(report.messages.is_empty());
}

#[test]
fn an_unknown_coordinate_system_is_said() {
    let bytes = answer(&[message(8, &number(1, 999_999)), transform(false, 1.0, 0.0, 0.0), feature(&[geometry(&[], &[1, 2])])]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert!(ds.crs.is_none());
    assert_eq!(report.messages, ["coordinate system 999999 can't be used; coordinates are read as longitude/latitude"]);
}

#[test]
fn an_answer_without_a_coordinate_system_is_said_to_be_read_as_longitude_latitude() {
    let bytes = answer(&[transform(false, 1.0, 0.0, 0.0), feature(&[geometry(&[], &[1, 2])])]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.features[0].geometry, Some(Geometry::Point(c(1.0, 2.0))));
    assert_eq!(report.messages, ["the answer names no coordinate system; coordinates are read as longitude/latitude"]);
}

#[test]
fn coordinates_past_longitude_latitude_lose_their_geometry_and_the_layer_says_so() {
    // Web Mercator: 1,113,195 m is 10°; 1,000,000,000 m comes out near 8,983°, beyond the world.
    let bytes = answer(&[
        message(8, &number(1, 3857)),
        transform(false, 1.0, 0.0, 0.0),
        feature(&[geometry(&[], &[1_113_195, 0])]),
        feature(&[geometry(&[], &[1_000_000_000, 0])]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert!(ds.features[0].geometry.is_some());
    assert_eq!(ds.features[1].geometry, None);
    // Defining the projection again can't bring it back.
    assert!(ds.crs_incomplete);
    assert_eq!(report.messages, ["1 features have coordinates outside their coordinate system: they are kept without geometry"]);
}

#[test]
fn text_that_isnt_utf8_is_read_with_replacements_and_said() {
    let bytes = answer(&[
        message(13, &[message(1, b"N\xffAME"), number(2, 4)].concat()),
        feature(&[message(1, &message(1, b"a\xffb"))]),
        feature(&[text("ok")]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.fields[0].name, "N\u{FFFD}AME");
    assert_eq!(ds.features[0].attrs, [Value::Text("a\u{FFFD}b".into())]);
    assert_eq!(ds.features[1].attrs, [Value::Text("ok".into())]);
    assert_eq!(report.messages, ["2 values or field names aren't valid UTF-8; the bytes that can't be read are replaced with U+FFFD"]);
}

#[test]
fn parts_without_vertices_add_nothing() {
    let line = |lengths: &[u64], coords: &[i64]| {
        answer(&[number(7, POLYLINE), lonlat(), transform(false, 1.0, 0.0, 0.0), feature(&[geometry(lengths, coords)])])
    };
    let (ds, report) = read(&line(&[0, 2, 0], &[1, 1, 2, 0]), "t").unwrap();
    assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![vec![c(1.0, 1.0), c(3.0, 1.0)]])));
    assert!(report.messages.is_empty(), "{:?}", report.messages);
    // Lengths that are all zero still count the coordinates: none of them belongs to a part.
    assert!(err(&line(&[0], &[1, 1])).contains("more coordinates"));
}

#[test]
fn repeated_names_and_unread_field_types_are_left_out_and_said() {
    let bytes = answer(&[
        field("NAME", 4),
        field("name", 4),
        field("PHOTO", 8),
        field("COUNT", 1),
        feature(&[text("a"), text("b"), text("blob"), text("many")]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["NAME", "COUNT"]);
    assert_eq!(ds.features[0].attrs, [Value::Text("a".into()), Value::Null]);
    assert_eq!(
        report.messages,
        [
            "1 fields repeat an earlier field's name and are left out",
            "1 fields hold geometries, blobs or rasters, which aren't read, and are left out",
            "1 values don't fit their field's type and are left empty",
        ]
    );
    // A table: no geometry type, no geometry.
    assert_eq!(ds.geometry_type, GeometryType::None);
}

#[test]
fn a_page_cut_at_the_record_limit_says_so() {
    let bytes = answer(&[number(9, 1), field("OBJECTID", OID), feature(&[uint(1)])]);
    assert!(read_page(&bytes, "t").unwrap().exceeded_transfer_limit);
    let (_, report) = read(&bytes, "t").unwrap();
    assert_eq!(report.messages, ["the service sent 1 features only (its record limit); the rest aren't in this answer"]);
}

#[test]
fn refused_answers_say_why() {
    assert!(err(b"").contains("f=json"));
    let e = err(br#"{"error":{"message":"Invalid query","code":400,"details":[]}}"#);
    assert!(e.contains("(400): Invalid query"), "{e}");
    assert!(err(br#"{"type":"FeatureCollection","features":[]}"#).contains("JSON, not a protocol buffer"));
    // A count (`returnCountOnly`).
    assert!(err(&[message(1, b"10.0"), message(2, &message(2, &number(1, 5)))].concat()).contains("count"));
    assert!(err(&[message(1, b"10.0")].concat()).contains("no features"));
    let with = |part: Vec<u8>| answer(&[transform(false, 1.0, 0.0, 0.0), feature(&[part])]);
    assert!(err(&with(message(7, &[]))).contains("curve"));
    assert!(err(&with(message(3, &[]))).contains("shape-buffer"));
    assert!(err(&with(geometry(&[], &[1, 1, 1, 1]))).contains("more than one vertex"));
    assert!(err(&with(geometry(&[3], &[1, 1]))).contains("ends inside"));
    assert!(err(&with(geometry(&[1], &[1, 1, 1, 1]))).contains("more coordinates"));
    let multipatch = answer(&[number(7, MULTIPATCH), transform(false, 1.0, 0.0, 0.0), feature(&[geometry(&[], &[1, 1])])]);
    assert!(err(&multipatch).contains("multipatch"));
    // Declared, without features: refused rather than read as an empty layer.
    assert!(err(&answer(&[number(7, MULTIPATCH)])).contains("multipatch"));
    // So is an envelope layer, which RustGIS doesn't read either.
    assert!(err(&answer(&[number(7, ENVELOPE)])).contains("envelope"));
    let untransformed = answer(&[feature(&[geometry(&[], &[1, 1])])]);
    assert!(err(&untransformed).contains("no transform"));
    let e = err(&answer(&[field("A", 4), feature(&[text("x"), text("y")])]));
    assert!(e.contains("feature 1: it has more values"), "{e}");
}

#[test]
fn many_fields_times_many_empty_features_are_refused_before_they_take_memory() {
    // A few tens of kB that would ask for over 512 MB of rows, whatever the answer's size. The
    // first feature past the bound comes from it (the fields count once, a row per feature), so
    // the test doesn't hang on the size of a value.
    let row = row_bytes(1, 4000) - row_bytes(0, 4000);
    let features = (MAX_ROW_BYTES - row_bytes(0, 4000)) / row + 1;
    let mut parts: Vec<Vec<u8>> = (0..4000).map(|i| field(&format!("f{i}"), 4)).collect();
    parts.extend((0..features).map(|_| feature(&[])));
    let e = err(&answer(&parts));
    assert!(e.contains(&format!("{features} features of 4000 fields would take more than 512 MB")), "{e}");
    // Fields after the features count too, refused as they come, before the last one.
    let mut late: Vec<Vec<u8>> = (0..2 * features).map(|_| feature(&[])).collect();
    late.extend((0..4000).map(|i| field(&format!("f{i}"), 4)));
    let e = err(&answer(&late));
    assert!(e.contains(&format!("{} features of ", 2 * features)) && e.contains(" fields would take more than 512 MB"), "{e}");
    assert!(!e.contains("of 4000 fields"), "{e}");
    // Fields alone, without a feature: two bytes each (an empty field message).
    let fields = MAX_ROW_BYTES / row_bytes(0, 1) + 1;
    let e = err(&answer(&[[0x6a, 0x00].repeat(fields)]));
    assert!(e.contains(&format!("0 features of {fields} fields would take more than 512 MB")), "{e}");
}

/// A polyline geometry's fields: `lengths` packed, then X and Y as differences.
fn line_parts(lengths: &[u64], coords: &[i64]) -> Vec<u8> {
    [message(2, &packed(lengths.iter().copied())), message(3, &packed(coords.iter().map(|&c| zz(c))))].concat()
}

#[test]
fn geometries_take_their_memory_from_one_room_for_the_whole_answer() {
    let transform = Transform { given: true, ..Transform::default() };
    let shape = Shape { kind: POLYLINE, transform: &transform, has_z: false, has_m: false };
    // Room for ten one-vertex parts, the answer's geometries together.
    let each = PART_BYTES + std::mem::size_of::<Coord>();
    let mut tally = Tally { geometry_room: 10 * each, ..Tally::default() };
    let six = line_parts(&[1; 6], &[1, 1].repeat(6));
    let Some(Geometry::Line(parts)) = read_geometry(&six, &shape, &mut tally).unwrap() else { panic!("not a line") };
    assert_eq!(parts.len(), 6);
    let e = read_geometry(&six, &shape, &mut tally).unwrap_err().to_string();
    assert!(e.contains("the geometries would take more than 1024 MB"), "{e}");
}

#[test]
fn a_point_with_a_second_part_is_refused_before_the_rest_is_read() {
    let transform = Transform { given: true, ..Transform::default() };
    let shape = Shape { kind: POINT, transform: &transform, has_z: false, has_m: false };
    // The second part's coordinates aren't even there: the refusal comes first.
    let e = read_geometry(&line_parts(&[1, 1], &[1, 1]), &shape, &mut Tally::default()).unwrap_err().to_string();
    assert!(e.contains("a point has more than one vertex"), "{e}");
}

#[test]
fn text_values_take_their_memory_from_one_room_for_the_whole_answer() {
    let transform = Transform::default();
    let shape = Shape { kind: POINT, transform: &transform, has_z: false, has_m: false };
    let columns = [Some((0, Kind::Text))];
    // 1e300 is 11 bytes of the answer and 301 digits as text.
    let huge = real(1e300);
    let mut tally = Tally { text_room: 500, ..Tally::default() };
    let (values, _) = super::feature(&huge, &columns, 1, &shape, &mut tally).unwrap();
    assert_eq!(values[0], Value::Text(format!("1{}", "0".repeat(300))));
    let e = super::feature(&huge, &columns, 1, &shape, &mut tally).unwrap_err().to_string();
    assert!(e.contains("the text values would take more than 512 MB"), "{e}");
}

#[test]
fn lengths_in_several_fields_keep_their_order() {
    // Packed first, then one number after the coordinates.
    let bytes = answer(&[
        number(7, POLYLINE),
        lonlat(),
        transform(false, 1.0, 0.0, 0.0),
        feature(&[message(2, &[message(2, &packed([2])), message(3, &packed([1, 1, 1, 0, 5, 5].map(zz))), number(2, 1)].concat())]),
    ]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![vec![c(1.0, 1.0), c(2.0, 1.0)], vec![c(5.0, 5.0)]])));
    assert!(report.messages.is_empty(), "{:?}", report.messages);
}

#[test]
fn a_wkt_that_isnt_utf8_is_said() {
    let wkt = b"GEOGCS[\"GCS_WGS_1984\xff\",DATUM[\"D_WGS_1984\",SPHEROID[\"WGS_1984\",6378137.0,298.257223563]],PRIMEM[\"Greenwich\",0.0],UNIT[\"Degree\",0.0174532925199433]]";
    let bytes = answer(&[message(8, &message(5, wkt)), transform(false, 1.0, 0.0, 0.0), feature(&[geometry(&[], &[1, 2])])]);
    let (_, report) = read(&bytes, "t").unwrap();
    assert_eq!(
        report.messages.first().map(String::as_str),
        Some("the coordinate system's WKT isn't valid UTF-8; the bytes that can't be read are replaced with U+FFFD")
    );
    // Unused when a code RustGIS knows names the system: nothing to say.
    let with_code = answer(&[
        message(8, &[number(1, 4326), message(5, wkt)].concat()),
        transform(false, 1.0, 0.0, 0.0),
        feature(&[geometry(&[], &[1, 2])]),
    ]);
    let (_, report) = read(&with_code, "t").unwrap();
    assert!(report.messages.is_empty(), "{:?}", report.messages);
}

#[test]
fn cut_or_corrupted_answers_never_panic() {
    let whole = points();
    for n in 0..whole.len() {
        let _ = read(&whole[..n], "cut");
        let _ = is_esripbf(&whole[..n]);
    }
    for i in 0..whole.len() {
        for byte in [0x00, 0x01, 0x7f, 0x80, 0xff] {
            let mut bytes = whole.clone();
            bytes[i] = byte;
            let _ = read(&bytes, "corrupt");
        }
    }
    // A length past every byte there is, and a number of eleven bytes.
    let mut huge = vec![0x0a];
    varint(&mut huge, u64::MAX);
    assert!(read(&huge, "t").is_err());
    assert!(read(&[0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01], "t").is_err());
}

#[test]
fn a_dropped_or_fetched_answer_is_read_by_its_content() {
    // A file saved as `.pbf`, and an address without an extension (`…/query?…&f=pbf`), which
    // `sniff_extension` names `.pbf`.
    assert_eq!(crate::io::sniff_extension(&points()), Some("pbf"));
    let (ds, report) = crate::io::read_bytes("quakes.pbf", &points()).unwrap();
    assert_eq!((ds.name.as_str(), ds.features.len(), ds.geometry_type), ("quakes", 2, GeometryType::Point));
    assert_eq!(ds.source.as_deref(), Some("quakes.pbf"));
    assert!(report.messages.is_empty(), "{:?}", report.messages);
    let layers = crate::io::read_layers("quakes.pbf", &points()).unwrap();
    assert_eq!(layers.len(), 1);
    // OpenStreetMap stays OpenStreetMap's, and another `.pbf` says what it isn't.
    let osm = crate::io::read_layers("city.osm.pbf", &crate::osm::tests::sample()).unwrap();
    assert!(osm.iter().any(|l| l.dataset.name.ends_with("points")), "{:?}", osm.iter().map(|l| &l.dataset.name).collect::<Vec<_>>());
    let e = crate::io::read_bytes("tile.pbf", &message(3, &message(1, b"roads"))).unwrap_err().to_string();
    assert!(e.contains("isn't OpenStreetMap data or an ArcGIS feature service's answer"), "{e}");
    // A refusal names the format, not OpenStreetMap.
    let e = crate::io::read_bytes("x.pbf", &answer(&[feature(&[message(7, &[])])])).unwrap_err().to_string();
    assert!(e.contains("ArcGIS protocol buffer") && e.contains("curve"), "{e}");
    // So does a count, saved or fetched (`returnCountOnly`).
    let count = [message(1, b"10.0"), message(2, &message(2, &number(1, 5)))].concat();
    assert_eq!(crate::io::sniff_extension(&count), Some("pbf"));
    let e = crate::io::read_bytes("n.pbf", &count).unwrap_err().to_string();
    assert!(e.contains("ArcGIS protocol buffer") && e.contains("a count, ids or an extent"), "{e}");
}

#[test]
fn answers_are_told_from_other_protocol_buffers_and_json() {
    assert!(is_esripbf(&points()));
    // Without its version too.
    assert!(is_esripbf(&message(2, &message(1, &number(7, POINT)))));
    // A count (of zero too, which sends nothing), object ids and an extent, which the reader
    // refuses by name.
    let query = |result: Vec<u8>| [message(1, b"10.0"), message(2, &result)].concat();
    assert!(is_esripbf(&query(message(2, &number(1, 5)))));
    assert!(is_esripbf(&query(message(2, &[]))));
    assert!(is_esripbf(&query(message(3, &message(1, b"OBJECTID")))));
    assert!(is_esripbf(&query(message(4, &message(1, &double(1, 1.0))))));
    // A result the format doesn't define, or a field its result doesn't have.
    assert!(!is_esripbf(&query(message(5, &number(1, 5)))));
    assert!(!is_esripbf(&query(message(2, &message(1, b"x")))));
    assert!(!is_esripbf(&query(message(3, &[]))));
    // OpenStreetMap: a big-endian header length, then its `OSMHeader` blob header.
    assert!(!is_esripbf(b"\x00\x00\x00\x0d\x0a\x09OSMHeader\x18\x40"));
    // A vector tile: a layer (field 3).
    assert!(!is_esripbf(&message(3, &message(1, b"roads"))));
    assert!(!is_esripbf(br#"{"features":[]}"#));
    assert!(!is_esripbf(b""));
}

/// Pages of real services read as `f=pbf`, against the same pages read as GeoJSON by RustGIS's
/// GeoJSON reader: the same features, values and vertices, coordinates within the services'
/// quantization. Set `RUSTGIS_ESRIPBF_DIR` to a folder of `<name>.pbf` and `<name>.geojson`
/// pairs, fetched with `outSR=4326`, for instance:
///
/// ```sh
/// q='query?where=1%3D1&outFields=*&returnGeometry=true&outSR=4326&resultRecordCount=500&orderByFields=OBJECTID'
/// page() { for f in pbf geojson; do curl -sf "$2/$q&f=$f" -o "$1.$f"; done; }
/// page quakes https://services9.arcgis.com/RHVPKKiFTONKtxq3/arcgis/rest/services/USGS_Seismic_Data_v1/FeatureServer/0
/// page counties https://services.arcgis.com/P3ePLMYs2RVChkJx/arcgis/rest/services/USA_Counties_Generalized_Boundaries/FeatureServer/0
/// page newtypes https://services5.arcgis.com/N82JbI5EYtAkuUKU/ArcGIS/rest/services/AllNewFieldTypes_withDateOnlyContingentValues/FeatureServer/0
/// page mlines https://sampleserver6.arcgisonline.com/arcgis/rest/services/Water_Network/FeatureServer/3
/// ```
#[test]
fn real_pages_match_the_same_pages_as_geojson() {
    let Ok(dir) = std::env::var("RUSTGIS_ESRIPBF_DIR") else { return };
    let mut pairs = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("pbf") {
            continue;
        }
        let name = path.display().to_string();
        let (pbf, _) = read(&std::fs::read(&path).unwrap(), "pbf").unwrap_or_else(|e| panic!("{name}: {e}"));
        let text = std::fs::read_to_string(path.with_extension("geojson")).unwrap();
        let (json, _) = crate::io::read_geojson(&text, "geojson").unwrap();
        assert_eq!(pbf.features.len(), json.features.len(), "{name}: features");
        for (row, (a, b)) in pbf.features.iter().zip(json.features.iter()).enumerate() {
            for (i, f) in pbf.fields.iter().enumerate() {
                let Some(j) = json.field_index(&f.name) else { continue };
                let (x, y) = (&a.attrs[i], &b.attrs[j]);
                let same = match (x, y) {
                    // GeoJSON prints single precision rounded to seven digits.
                    (Value::Float(p), Value::Float(q)) => (p - q).abs() <= 1e-6 * p.abs().max(q.abs()).max(1.0),
                    (Value::Float(p), Value::Int(q)) | (Value::Int(q), Value::Float(p)) => *p == *q as f64,
                    _ => x == y,
                };
                assert!(same, "{name}: row {row}, {}: {x:?} against {y:?}", f.name);
            }
            // A geometry without vertices is none: the protocol buffer sends it without
            // coordinates, GeoJSON as an empty `LineString`, which its reader keeps.
            let shape = |g: &Option<Geometry>| {
                g.clone().filter(|g| {
                    let mut any = false;
                    g.for_each_coord(|_| any = true);
                    any
                })
            };
            match (&shape(&a.geometry), &shape(&b.geometry)) {
                (Some(p), Some(q)) => {
                    assert_eq!(p.kind(), q.kind(), "{name}: row {row}");
                    let (mut n, mut m) = (0, 0);
                    p.for_each_coord(|_| n += 1);
                    q.for_each_coord(|_| m += 1);
                    assert_eq!(n, m, "{name}: row {row}, vertices");
                    let (e, f) = (p.extent(), q.extent());
                    for (u, v) in [(e.min_x, f.min_x), (e.min_y, f.min_y), (e.max_x, f.max_x), (e.max_y, f.max_y)] {
                        assert!((u - v).abs() < 1e-7, "{name}: row {row}, extent {e:?} against {f:?}");
                    }
                }
                (p, q) => assert_eq!(p.is_some(), q.is_some(), "{name}: row {row}, geometry"),
            }
        }
        pairs += 1;
    }
    assert!(pairs > 0, "{dir}: no .pbf and .geojson pairs");
}

#[test]
fn a_field_type_rustgis_doesnt_know_is_read_as_text_and_said() {
    let bytes = answer(&[field("LATER", 17), field("NAME", 4), feature(&[uint(5), text("a")])]);
    let (ds, report) = read(&bytes, "t").unwrap();
    assert_eq!(ds.features[0].attrs, [Value::Text("5".into()), Value::Text("a".into())]);
    assert_eq!(report.messages, ["1 fields have a type RustGIS doesn't know; their values are read as text"]);
    // Every type the format defines today says nothing.
    let known: Vec<Vec<u8>> = (0..=LAST_FIELD_TYPE).filter(|t| !(7..=9).contains(t)).map(|t| field(&format!("F{t}"), t)).collect();
    let (_, report) = read(&answer(&known), "t").unwrap();
    assert!(report.messages.is_empty(), "{:?}", report.messages);
}

#[test]
fn text_is_taken_from_the_room_at_its_size_once_read_before_it_is_made() {
    // Each byte that isn't UTF-8 becomes U+FFFD, three bytes.
    for bytes in [&b"plain"[..], b"a\xffb", b"\xff\xff\xff", b"\xe2\x82", "Bogotá".as_bytes()] {
        assert_eq!(lossy_len(bytes), String::from_utf8_lossy(bytes).len(), "{bytes:?}");
    }
    let transform = Transform::default();
    let shape = Shape { kind: POINT, transform: &transform, has_z: false, has_m: false };
    let columns = [Some((0, Kind::Text))];
    // Ten bytes sent, thirty once read: room for the bytes sent isn't enough.
    let cell = message(1, &message(1, &[0xff; 10]));
    let mut tally = Tally { text_room: 20, ..Tally::default() };
    let e = super::feature(&cell, &columns, 1, &shape, &mut tally).unwrap_err().to_string();
    assert!(e.contains("the text values would take more than"), "{e}");
    // A field's name counts too, twice, as it's kept lowercase as well.
    let name = [message(1, &[0xff; 10]), number(2, 4)].concat();
    assert!(super::field(&name, &mut Tally { text_room: 59, ..Tally::default() }).is_err());
    assert!(super::field(&name, &mut Tally { text_room: 60, ..Tally::default() }).is_ok());
}

#[test]
fn a_multipoint_of_one_part_keeps_its_points() {
    let transform = Transform { given: true, upper_left: false, ..Transform::default() };
    let shape = Shape { kind: MULTIPOINT, transform: &transform, has_z: false, has_m: false };
    let one = line_parts(&[2], &[1, 1, 1, 1]);
    let two = line_parts(&[1, 1], &[1, 1, 2, 2]);
    let points = |bytes: &[u8]| read_geometry(bytes, &shape, &mut Tally::default()).unwrap();
    assert_eq!(points(&one), Some(Geometry::MultiPoint(vec![c(1.0, 1.0), c(2.0, 2.0)])));
    assert_eq!(points(&two), Some(Geometry::MultiPoint(vec![c(1.0, 1.0), c(2.0, 2.0)])));
}

#[test]
fn a_multipoint_of_several_parts_takes_its_joined_points_from_the_room() {
    let transform = Transform { given: true, upper_left: false, ..Transform::default() };
    let shape = Shape { kind: MULTIPOINT, transform: &transform, has_z: false, has_m: false };
    let two = line_parts(&[1, 1], &[1, 1, 2, 2]);
    let vertex = std::mem::size_of::<Coord>();
    // The two parts fit; the joined list of their two points doesn't.
    let parts = 2 * (PART_BYTES + vertex);
    assert!(read_geometry(&two, &shape, &mut Tally { geometry_room: parts + 2 * vertex - 1, ..Tally::default() }).is_err());
    assert!(read_geometry(&two, &shape, &mut Tally { geometry_room: parts + 2 * vertex, ..Tally::default() }).is_ok());
}

#[test]
fn a_wkt_is_taken_from_the_text_room_at_its_size_once_read() {
    // 10 bytes, 2 of them not UTF-8: 8 + 2 × 3 = 14 once read.
    let reference = message(5, b"GEOGCS\xff\xfe[]");
    assert!(spatial_reference(&reference, &mut Tally { text_room: 13, ..Tally::default() }).is_err());
    assert!(spatial_reference(&reference, &mut Tally { text_room: 14, ..Tally::default() }).is_ok());
}
