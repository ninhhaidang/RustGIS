//! Services asked for in their own coordinate system (#2032 C2f-3b, C2f-3c): a WFS type in its
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
    // Read in a system other than the one asked for: an assumption, coded.
    assert_eq!(s.notices.iter().map(|n| n.loss()).collect::<Vec<_>>(), [Some(rustgis_data::loss::LossCode::AssumedCrs)]);
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
    // Read in the system asked for: nothing assumed past the request.
    assert_eq!(s.notices.iter().map(|n| n.loss()).collect::<Vec<_>>(), [None]);
}

/// A WCS coverage in LV95 is asked for in it, with no conversion by the server, and comes in as a
/// raster in LV95 for the map to resample once; a geographic one is asked in EPSG:4326 as before.
#[test]
fn a_wcs_coverage_is_asked_for_in_its_own_system() {
    let lv95 = Crs::from_epsg(2056).unwrap();
    let r = rustgis_data::raster::Raster::new(
        "dem",
        10,
        10,
        vec![vec![500.0; 100]],
        rustgis_data::raster::GeoTransform { origin_x: 2_600_000.0, origin_y: 1_200_100.0, pixel_width: 10.0, pixel_height: -10.0 },
        lv95,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let (base, _) = super::serve_file(rustgis_data::raster::geotiff::write(&r, 0).unwrap(), false);
    let caps = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
        <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>dem</wcs:CoverageId><ows:Title>Terrain</ows:Title>
          <ows:WGS84BoundingBox><ows:LowerCorner>7.43 46.95</ows:LowerCorner><ows:UpperCorner>7.45 46.96</ows:UpperCorner></ows:WGS84BoundingBox>
        </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
    let describe = r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/2056" axisLabels="E N">
        <lowerCorner>2600000 1200000</lowerCorner><upperCorner>2600100 1200100</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
    let url = format!("{base}/wcs");
    let mut s = Session::new(Project::default());
    let p = json!({"url": url, "layer": "dem", "text": caps, "describe": describe, "bbox": "full"});
    let c = s.run("service.coverage", &p).unwrap();
    let get = c["url"].as_str().unwrap();
    assert!(
        get.contains("&OUTPUTCRS=http://www.opengis.net/def/crs/EPSG/0/2056&SUBSET=E(2600000,2600100)&SUBSET=N(1200000,1200100)&SCALESIZE=E(2048),N(2048)"),
        "{get}"
    );
    let added = s.run("service.add", &p).unwrap();
    let l = s.map().layer(added["layer"].as_u64().unwrap() as u32).unwrap();
    assert_eq!(l.raster.as_ref().unwrap().data.as_ref().unwrap().crs, lv95);
    // In EPSG:4326: asked so, as before.
    let geographic = describe
        .replace("EPSG/0/2056", "EPSG/0/4326")
        .replace(r#"axisLabels="E N""#, r#"axisLabels="Lat Long""#)
        .replace("2600000 1200000", "46.95 7.43")
        .replace("2600100 1200100", "46.96 7.45");
    let c = s.run("service.coverage", &json!({"url": url, "layer": "dem", "text": caps, "describe": geographic, "bbox": "full"})).unwrap();
    assert!(c["url"].as_str().unwrap().contains("OUTPUTCRS=http://www.opengis.net/def/crs/EPSG/0/4326&SUBSET=Long(7.43,7.45)"), "{c}");
}

/// A 2 × 2 byte GeoTIFF placed at `corner`, `step` a pixel, that doesn't say its coordinate system
/// (no GeoKey directory), written by hand: little-endian, one strip.
fn untagged_tiff(corner: (f64, f64), step: f64) -> Vec<u8> {
    let entries: [(u16, u16, u32, u32); 11] = [
        (256, 3, 1, 2),
        (257, 3, 1, 2),
        (258, 3, 1, 8),
        (259, 3, 1, 1),
        (262, 3, 1, 1),
        (273, 4, 1, 218),
        (277, 3, 1, 1),
        (278, 3, 1, 2),
        (279, 4, 1, 4),
        (33550, 12, 3, 146),
        (33922, 12, 6, 170),
    ];
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(out.len(), 146);
    for v in [step, step, 0.0, 0.0, 0.0, 0.0, corner.0, corner.1, 0.0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(out.len(), 218);
    out.extend_from_slice(&[1, 2, 3, 4]);
    out
}

/// A coverage asked for in LV95 whose answer doesn't say its system is read in LV95, and says so,
/// whether `service.add` downloads it or the app does (`service.coverage` gives the system, and
/// `layer.addDownload` takes it as `coverageCrs`).
#[test]
fn a_coverage_answer_without_its_system_is_read_in_the_one_asked_for() {
    let lv95 = Crs::from_epsg(2056).unwrap();
    let tiff = untagged_tiff((2_600_000.0, 1_200_020.0), 10.0);
    let (base, _) = super::serve_file(tiff.clone(), false);
    let caps = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
        <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>dem</wcs:CoverageId><ows:Title>Terrain</ows:Title></wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
    let describe = r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/2056" axisLabels="E N">
        <lowerCorner>2600000 1200000</lowerCorner><upperCorner>2600020 1200020</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
    let url = format!("{base}/wcs");
    let p = json!({"url": url, "layer": "dem", "text": caps, "describe": describe, "bbox": "full"});
    let mut s = Session::new(Project::default());
    let c = s.run("service.coverage", &p).unwrap();
    assert_eq!(c["coverageCrs"], "EPSG:2056", "{c}");
    let crs_of = |s: &Session, id: &serde_json::Value| {
        s.map().layer(id.as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().crs
    };
    let added = s.run("service.add", &p).unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), lv95);
    assert!(notices(&s).iter().any(|n| n.contains("the system it was asked for in")), "{:?}", notices(&s));
    // As the app adds it.
    let get = c["url"].as_str().unwrap();
    s.downloaded.push((7, tiff.clone()));
    let added =
        s.run("layer.addDownload", &json!({"id": 7, "url": get, "name": "Terrain", "coverage": true, "coverageCrs": "EPSG:2056"})).unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), lv95);
    // The address says the system too: without coverageCrs, as a script adds it.
    s.downloaded.push((13, tiff.clone()));
    let added = s.run("layer.addDownload", &json!({"id": 13, "url": get, "name": "Terrain", "coverage": true})).unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), lv95);
    // An address that doesn't say it, without the system (or with one that isn't an EPSG code of
    // a projected system): its metres can't be placed, refused as before.
    let unsaid =
        get.replace("&SUBSETTINGCRS=http://www.opengis.net/def/crs/EPSG/0/2056&OUTPUTCRS=http://www.opengis.net/def/crs/EPSG/0/2056", "");
    assert_ne!(unsaid, get);
    for (id, crs) in [(8, None), (9, Some("EPSG:4326")), (10, Some("2056")), (11, Some("EPSG:x")), (12, Some("+proj=somerc"))] {
        s.downloaded.push((id, tiff.clone()));
        let mut q = json!({"id": id, "url": unsaid, "name": "Terrain", "coverage": true});
        if let Some(c) = crs {
            q["coverageCrs"] = json!(c);
        }
        assert!(s.run("layer.addDownload", &q).is_err(), "{crs:?}");
    }
    // A coverageCrs that isn't usable leaves it to the address; a usable one comes first.
    s.downloaded.push((14, tiff.clone()));
    let added = s
        .run("layer.addDownload", &json!({"id": 14, "url": get, "name": "Terrain", "coverage": true, "coverageCrs": "EPSG:4326"}))
        .unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), lv95);
    s.downloaded.push((15, tiff.clone()));
    let added = s
        .run("layer.addDownload", &json!({"id": 15, "url": get, "name": "Terrain", "coverage": true, "coverageCrs": "EPSG:21781"}))
        .unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), Crs::from_epsg(21781).unwrap());
    // Added by address, as the command line adds service.coverage's address: the address says
    // the system, or coverageCrs does.
    let added = s.run("layer.addUrl", &json!({"url": get, "name": "By address", "coverage": true})).unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), lv95);
    let added = s.run("layer.addUrl", &json!({"url": unsaid, "name": "Given", "coverage": true, "coverageCrs": "EPSG:2056"})).unwrap();
    assert_eq!(crs_of(&s, &added["layer"]), lv95);
}
