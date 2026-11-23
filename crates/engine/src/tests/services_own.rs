//! Services' features asked for in their own coordinate system (#2032 C2f-3b): a WFS type in its
//! default system and an ArcGIS layer in its service's, held in it as they come, with the
//! longitude/latitude request kept where the system isn't one RustGIS reads.

use super::ogc_server::{Route, Server, arcgis_layer};
use crate::{Project, Session};
use rustgis_data::raster::Crs;
use serde_json::json;

fn first_point(s: &Session, name: &str) -> (Crs, f64, f64) {
    let l = s.map().layer_by_name(name).unwrap();
    let Some(rustgis_data::Geometry::Point(c)) = l.dataset.features.iter().next().unwrap().geometry.clone() else { panic!("not a point") };
    (l.dataset.coord_crs(), c.x, c.y)
}

fn notices(s: &Session) -> Vec<String> {
    s.notices.iter().map(|n| n.text.clone()).collect()
}

/// A WFS 1.1 type in LV95 is asked for in it and held in it, whether its answer names the system
/// or not, and turned round when written north first; a server answering in degrees anyway is
/// read so, and said; a type in a system RustGIS doesn't read is asked in CRS84 as before.
#[test]
fn a_wfs_type_is_asked_for_in_its_own_system() {
    let point = |x: f64, y: f64, member: &str| {
        format!(
            r#"{{"type":"FeatureCollection",{member}"features":[{{"type":"Feature","properties":{{"n":1}},"geometry":{{"type":"Point","coordinates":[{x},{y}]}}}}]}}"#
        )
    };
    let named = r#""crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::2056"}},"#;
    let server = Server::new(vec![
        Route::new("/plain", &point(2_600_000.0, 1_200_000.0, "")),
        Route::new("/turned", &point(1_200_000.0, 2_600_000.0, named)),
        Route::new("/lonlat", &point(7.44, 46.95, "")),
    ]);
    let caps = |path: &str, system: &str| {
        format!(
            r#"<WFS_Capabilities version="1.1.0"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get href="{}{path}?"/></HTTP></DCP></Operation></OperationsMetadata>
<FeatureTypeList><FeatureType><Name>towns</Name><Title>Towns</Title><DefaultSRS>{system}</DefaultSRS><OtherSRS>urn:ogc:def:crs:OGC:1.3:CRS84</OtherSRS>
<WGS84BoundingBox><LowerCorner>7.0 46.5</LowerCorner><UpperCorner>8.0 47.2</UpperCorner></WGS84BoundingBox></FeatureType></FeatureTypeList></WFS_Capabilities>"#,
            server.base
        )
    };
    let lv95 = Crs::from_epsg(2056).unwrap();
    let mut s = Session::new(Project::default());
    for (path, name) in [("/plain", "Plain"), ("/turned", "Turned")] {
        let text = caps(path, "urn:ogc:def:crs:EPSG::2056");
        s.run("service.add", &json!({"url": format!("{}{path}", server.base), "layer": "towns", "text": text, "name": name})).unwrap();
        assert_eq!(first_point(&s, name), (lv95, 2_600_000.0, 1_200_000.0), "{path}");
    }
    assert!(notices(&s).is_empty(), "{:?}", notices(&s));
    // A server that answers in longitude/latitude whatever it is asked: read as such, and said.
    let text = caps("/lonlat", "urn:ogc:def:crs:EPSG::2056");
    s.run("service.add", &json!({"url": format!("{}/lonlat", server.base), "layer": "towns", "text": text, "name": "Ignored"})).unwrap();
    assert_eq!(first_point(&s, "Ignored"), (Crs::LonLat, 7.44, 46.95));
    let said = notices(&s);
    assert!(
        said.len() == 1 && said[0].starts_with("1 service answer(s) named no coordinate system, and their positions are in degrees"),
        "{said:?}"
    );
    // A system RustGIS doesn't read: in CRS84, as before.
    let text = caps("/lonlat", "urn:ogc:def:crs:EPSG::999999");
    s.run("service.add", &json!({"url": format!("{}/lonlat", server.base), "layer": "towns", "text": text, "name": "Unread"})).unwrap();
    assert_eq!(first_point(&s, "Unread"), (Crs::LonLat, 7.44, 46.95));
    let srs: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.query.iter().any(|(k, v)| k == "REQUEST" && v == "GetFeature") && !r.query.iter().any(|(k, _)| k == "RESULTTYPE"))
        .filter_map(|r| r.query.iter().find(|(k, _)| k == "SRSNAME").map(|(_, v)| v.clone()))
        .collect();
    assert_eq!(srs, ["EPSG:2056", "EPSG:2056", "EPSG:2056", "urn:ogc:def:crs:OGC:1.3:CRS84"]);
}

/// A server that lists a type's own system but refuses it (an exception report where features
/// were expected) is asked once more in EPSG:4326, as before, and that is said.
#[test]
fn a_wfs_type_whose_system_is_refused_is_asked_in_degrees() {
    let refusal =
        r#"<?xml version="1.0"?><ows:ExceptionReport><ows:Exception exceptionCode="InvalidParameterValue"/></ows:ExceptionReport>"#;
    let caps = |base: &str| {
        format!(
            r#"<WFS_Capabilities version="1.1.0"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get href="{base}/refuses?"/></HTTP></DCP></Operation></OperationsMetadata>
<FeatureTypeList><FeatureType><Name>towns</Name><Title>Towns</Title><DefaultSRS>urn:ogc:def:crs:EPSG::2056</DefaultSRS>
<WGS84BoundingBox><LowerCorner>7.0 46.5</LowerCorner><UpperCorner>8.0 47.2</UpperCorner></WGS84BoundingBox></FeatureType></FeatureTypeList></WFS_Capabilities>"#
        )
    };
    let server = Server::new(vec![Route::new("/refuses", refusal)]);
    let text = caps(&server.base);
    // The address the type is asked at, and the one in degrees, answered with its features.
    let listed = rustgis_data::ogc::parse(&format!("{}/refuses", server.base), text.as_bytes()).unwrap();
    let rustgis_data::ogc::Offer::Features { url } = &listed.layers[0].offer else { panic!("{:?}", listed.layers[0].offer) };
    let lonlat = rustgis_data::ogc::wfs_lonlat_fallback(url).unwrap();
    let target = lonlat.strip_prefix(&server.base).unwrap();
    server.set(
        target,
        r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"n":1},"geometry":{"type":"Point","coordinates":[7.44,46.95]}}]}"#,
    );
    let mut s = Session::new(Project::default());
    s.run("service.add", &json!({"url": format!("{}/refuses", server.base), "layer": "towns", "text": text, "name": "Towns"})).unwrap();
    assert_eq!(first_point(&s, "Towns"), (Crs::LonLat, 7.44, 46.95));
    let said = notices(&s);
    assert!(
        said.iter()
            .any(|n| n.starts_with("Towns: the server refused its own system (EPSG:2056") && n.ends_with("asked for in EPSG:4326 instead")),
        "{said:?}"
    );
}

/// An ArcGIS layer of a service in LV95 is asked for in it (`outSR=2056`) and held in it, its
/// GeoJSON answer naming no system; a service in Web Mercator by 3857; one in longitude/latitude
/// as before.
#[test]
fn an_arcgis_layer_is_asked_for_in_its_services_system() {
    let page = |x: f64, y: f64| {
        format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","id":1,"properties":{{"OBJECTID":1}},"geometry":{{"type":"Point","coordinates":[{x},{y}]}}}}]}}"#
        )
    };
    let server = Server::new(vec![
        Route::new("/arcgis/rest/services/LV/FeatureServer/0/query", &page(2_600_000.0, 1_200_000.0)),
        Route::new("/arcgis/rest/services/LV/FeatureServer/0", &arcgis_layer(1000, false)),
        Route::new("/arcgis/rest/services/WM/FeatureServer/0/query", &page(828_000.0, 5_934_000.0)),
        Route::new("/arcgis/rest/services/WM/FeatureServer/0", &arcgis_layer(1000, false)),
        Route::new("/arcgis/rest/services/LL/FeatureServer/0/query", &page(7.44, 46.95)),
        Route::new("/arcgis/rest/services/LL/FeatureServer/0", &arcgis_layer(1000, false)),
    ]);
    // Its full extent, in its own system, around the town.
    let service = |sr: &str, x: f64, y: f64, d: f64| {
        format!(
            r#"{{"currentVersion": 11.5, "maxRecordCount": 1000, "spatialReference": {sr}, "fullExtent": {{"xmin": {}, "ymin": {}, "xmax": {}, "ymax": {}, "spatialReference": {sr}}}, "layers": [{{"id": 0, "name": "Towns"}}]}}"#,
            x - d,
            y - d,
            x + d,
            y + d
        )
    };
    let mut s = Session::new(Project::default());
    for (folder, sr, crs, x, y, d) in [
        ("LV", r#"{"wkid": 2056, "latestWkid": 2056}"#, Crs::from_epsg(2056).unwrap(), 2_600_000.0, 1_200_000.0, 50_000.0),
        ("WM", r#"{"wkid": 102100, "latestWkid": 3857}"#, Crs::WebMercator, 828_000.0, 5_934_000.0, 50_000.0),
        ("LL", r#"{"wkid": 4326}"#, Crs::LonLat, 7.44, 46.95, 0.5),
    ] {
        let url = format!("{}/arcgis/rest/services/{folder}/FeatureServer", server.base);
        s.run("service.add", &json!({"url": url, "layer": "Towns", "text": service(sr, x, y, d), "name": folder})).unwrap();
        assert_eq!(first_point(&s, folder), (crs, x, y), "{folder}");
    }
    let out: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/query"))
        .filter_map(|r| r.query.iter().find(|(k, _)| k == "outSR").map(|(_, v)| v.clone()))
        .collect();
    assert_eq!(out, ["2056", "3857", "4326"]);
    assert!(notices(&s).is_empty(), "{:?}", notices(&s));
    // A service without a full extent: its answer, naming no system, can't be checked against
    // bounds. It is read in the system asked for, and that is said, once.
    let bare =
        r#"{"currentVersion": 11.5, "maxRecordCount": 1000, "spatialReference": {"wkid": 2056}, "layers": [{"id": 0, "name": "Towns"}]}"#;
    let url = format!("{}/arcgis/rest/services/LV/FeatureServer", server.base);
    s.run("service.add", &json!({"url": url, "layer": "Towns", "text": bare, "name": "Unbounded"})).unwrap();
    assert_eq!(first_point(&s, "Unbounded"), (Crs::from_epsg(2056).unwrap(), 2_600_000.0, 1_200_000.0));
    let said = notices(&s);
    assert!(said.len() == 1 && said[0].contains("1 service answer(s) named no coordinate system"), "{said:?}");
}
