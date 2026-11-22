//! A GeoJSON FeatureCollection's legacy `crs` member (issue #930) is honoured when it is added,
//! whole or read in place, and what is written is still WGS 84 with no `crs`.
use rustgis_engine::{
    Project, Session,
    data::{io, raster::Crs},
    geom::Extent,
};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Arnhem's municipal outline's box and a point, in RD New (EPSG:28992).
fn rd(crs: &Value) -> String {
    json!({
        "type": "FeatureCollection",
        "name": "aoi_gemeente_arnhem",
        "crs": crs,
        "features": [
            {"type": "Feature", "properties": {"naam": "Arnhem"}, "geometry": {"type": "Polygon", "coordinates":
                [[[183548, 438459], [196442, 438459], [196442, 454549], [183548, 454549], [183548, 438459]]]}},
            {"type": "Feature", "properties": {"naam": "punt"}, "geometry": {"type": "Point", "coordinates": [188000, 445000]}},
        ]
    })
    .to_string()
}

fn declared(name: &str) -> Value {
    json!({"type": "name", "properties": {"name": name}})
}

fn file(name: &str, text: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-geojson-crs-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn add(s: &mut Session, p: &PathBuf, stream: bool) -> Result<u32, String> {
    let r = s.run("layer.addData", &json!({"path": p, "stream": stream}))?;
    r["layer"].as_u64().map(|id| id as u32).ok_or_else(|| format!("no layer in {r}"))
}

fn in_arnhem(e: &Extent) -> bool {
    (5.8..6.0).contains(&e.min_x) && (5.8..6.0).contains(&e.max_x) && (51.9..52.1).contains(&e.min_y) && (51.9..52.1).contains(&e.max_y)
}

#[test]
fn layer_lands_in_arnhem_by_the_declared_crs_both_ways() {
    let p = file("arnhem.geojson", &rd(&declared("urn:ogc:def:crs:EPSG::28992")));
    for stream in [false, true] {
        let mut s = Session::new(Project::default());
        let id = add(&mut s, &p, stream).unwrap();
        let layer = s.map().layer(id).unwrap();
        assert_eq!(layer.is_streamed(), stream);
        assert!(in_arnhem(&layer.extent()), "stream {stream}: {:?}", layer.extent());
        if !stream {
            assert_eq!(layer.dataset.crs, Some(Crs::Epsg(28992)));
        }
    }
}

#[test]
fn export_is_wgs84_without_crs_and_returns_to_the_native_numbers() {
    let p = file("arnhem-export.geojson", &rd(&declared("EPSG:28992")));
    let mut s = Session::new(Project::default());
    let id = add(&mut s, &p, false).unwrap();
    let out = p.with_file_name("out.geojson");
    s.run("share.exportLayer", &json!({"layer": id, "path": out})).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(!text.contains("\"crs\""), "{text}");
    let (back, _) = io::read_geojson(&text, "x").unwrap();
    assert!(in_arnhem(&back.extent()), "{:?}", back.extent());
    // In its own system again, the layer is where the file put it.
    let native = io::project(&s.map().layer(id).unwrap().dataset, Crs::Epsg(28992)).unwrap();
    let e = native.extent();
    assert!((e.min_x - 183_548.0).abs() < 0.01 && (e.max_x - 196_442.0).abs() < 0.01, "{e:?}");
    assert!((e.min_y - 438_459.0).abs() < 0.01 && (e.max_y - 454_549.0).abs() < 0.01, "{e:?}");
}

#[test]
fn define_projection_still_works_on_a_layer_with_a_declared_crs() {
    let mut s = Session::new(Project::default());
    // Declared one way; the user knows better: the native numbers are reinterpreted.
    let p = file("arnhem-redefine.geojson", &rd(&declared("EPSG:32631")));
    let id = add(&mut s, &p, false).unwrap();
    assert!(!in_arnhem(&s.map().layer(id).unwrap().extent()));
    s.run("layer.defineProjection", &json!({"layer": id, "crs": "EPSG:28992"})).unwrap();
    let layer = s.map().layer(id).unwrap();
    assert!(in_arnhem(&layer.extent()), "{:?}", layer.extent());
    assert_eq!(layer.dataset.crs, Some(Crs::Epsg(28992)));
    // And a file that says nothing, then Define Projection, as before the fix.
    let p = file("arnhem-silent.geojson", &rd(&Value::Null));
    let id = add(&mut s, &p, false).unwrap();
    assert!(!in_arnhem(&s.map().layer(id).unwrap().extent()));
    s.run("layer.defineProjection", &json!({"layer": id, "crs": "EPSG:28992"})).unwrap();
    assert!(in_arnhem(&s.map().layer(id).unwrap().extent()));
}

#[test]
fn a_crs_of_any_shape_never_panics() {
    let long = "9".repeat(200_000);
    let mut nested = json!(1);
    for _ in 0..100 {
        nested = json!([nested]);
    }
    let crs = [
        Value::Null,
        json!(1),
        json!(1e308),
        json!(true),
        json!("EPSG:28992"),
        json!(long),
        json!([]),
        json!({}),
        nested.clone(),
        json!({"type": "name"}),
        json!({"type": "name", "properties": null}),
        json!({"type": "name", "properties": []}),
        json!({"type": "name", "properties": {"name": null}}),
        json!({"type": "name", "properties": {"name": [1]}}),
        json!({"type": "name", "properties": {"name": nested}}),
        json!({"type": "name", "properties": {"name": long}}),
        json!({"type": "name", "properties": {"name": format!("EPSG:{long}")}}),
        json!({"type": "name", "properties": {"name": "EPSG:-1"}}),
        json!({"type": "name", "properties": {"name": "EPSG:65535"}}),
        json!({"type": "name", "properties": {"name": "\u{0}EPSG:\u{202e}28992\u{1f}"}}),
        json!({"type": "name", "properties": {"name": format!("ÉPSG:é{}", "€".repeat(60))}}),
        json!({"type": "link", "properties": {"href": "http://127.0.0.1:9/crs", "type": "ogcwkt"}}),
        json!({"type": ["name"], "properties": {"name": "EPSG:28992"}}),
    ];
    for (n, crs) in crs.iter().enumerate() {
        let p = file(&format!("junk{n}.geojson"), &rd(crs));
        for stream in [false, true] {
            let mut s = Session::new(Project::default());
            if let Ok(id) = add(&mut s, &p, stream) {
                // Added, drawn and described.
                let _ = s.map().layer(id).map(|l| l.extent());
                let _ = s.run("info.project", &Value::Null);
            }
        }
    }
    // The same on features and geometries (not read), and a crs after the features.
    for text in [
        r#"{"type":"FeatureCollection","features":[{"type":"Feature","crs":7,"properties":{},"geometry":{"type":"Point","coordinates":[1,2],"crs":[]}}]}"#,
        r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}],"crs":{"type":"name","properties":{"name":"EPSG:32631"}}}"#,
        r#"{"type":"Feature","crs":{"type":"name","properties":{"name":"EPSG:28992"}},"properties":{},"geometry":null}"#,
    ] {
        let p = file("odd.geojson", text);
        for stream in [false, true] {
            let mut s = Session::new(Project::default());
            let _ = add(&mut s, &p, stream);
        }
    }
}

#[test]
fn a_crs_after_the_features_is_honoured() {
    let text = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[500000,4649776]}}],"crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG:6.6:32617"}}}"#;
    let p = file("late.geojson", text);
    for stream in [false, true] {
        let mut s = Session::new(Project::default());
        let id = add(&mut s, &p, stream).unwrap();
        let e = s.map().layer(id).unwrap().extent();
        assert!((e.min_x + 81.0).abs() < 0.01 && (e.min_y - 42.0).abs() < 0.01, "stream {stream}: {e:?}");
    }
}
