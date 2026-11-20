//! A QGIS project's web service layers (#882 piece 10b) come in through `service.add`, as Add Web
//! Data adds them, served here by the local fixture server.

use super::{Project, Session, ogc_server};
use ogc_server::{Route, Server, arcgis_layer, arcgis_page};
use serde_json::json;

/// A project with a WFS layer (filtered by its `sql=`), an ArcGIS feature layer (with a login) and
/// a map service's layer that the service doesn't draw alone: each comes in from its service,
/// with its visibility and filter, and what isn't carried over noted. A type the service doesn't
/// list is noted, and the rest still come in.
#[test]
fn a_qgis_projects_web_service_layers_come_in_from_their_services() {
    let features: Vec<_> = [1, 2, 3]
        .iter()
        .map(|id| json!({"type": "Feature", "id": id, "properties": {"n": id}, "geometry": {"type": "Point", "coordinates": [*id, 1]}}))
        .collect();
    let page = json!({"type": "FeatureCollection", "features": features}).to_string();
    let query = "SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&TYPENAMES=roads";
    let format = "OUTPUTFORMAT=application%2Fjson&SRSNAME=EPSG:4326";
    let server = Server::new(vec![
        Route::new("/wfs?SERVICE=WFS&REQUEST=GetCapabilities", ""),
        Route::new(&format!("/wfs?{query}&{format}&COUNT=50000"), &page),
        Route::new(&format!("/wfs?{query}&SRSNAME=EPSG:4326&RESULTTYPE=hits"), r#"<FeatureCollection numberMatched="3"/>"#),
        Route::new(
            "/arcgis/rest/services/P/FeatureServer",
            r#"{"currentVersion": 11.5, "capabilities": "Query", "layers": [{"id": 0, "name": "Parcels"}]}"#,
        ),
        Route::new("/arcgis/rest/services/P/FeatureServer/0", &arcgis_layer(10, false)),
        Route::new("/arcgis/rest/services/P/FeatureServer/0/query", &arcgis_page(&[0, 1], false)),
        Route::new(
            "/arcgis/rest/services/T/MapServer",
            r#"{"currentVersion": 11.5, "capabilities": "Map", "layers": [{"id": 0, "name": "Base"}]}"#,
        ),
    ]);
    let base = &server.base;
    server.set(
        "/wfs?SERVICE=WFS&REQUEST=GetCapabilities",
        &format!(
            r#"<WFS_Capabilities version="2.0.0"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get href="{base}/wfs?"/></HTTP></DCP></Operation></OperationsMetadata><FeatureTypeList><FeatureType><Name>roads</Name><Title>Roads</Title></FeatureType></FeatureTypeList></WFS_Capabilities>"#
        ),
    );
    let layer = |id: &str, kind: &str, provider: &str, source: &str| {
        format!(
            r#"<maplayer type="{kind}"><id>{id}</id><datasource>{source}</datasource><layername>{id}</layername><provider>{provider}</provider></maplayer>"#
        )
    };
    let qgs = format!(
        r#"<qgis version="4.2.3"><layer-tree-group>
            <layer-tree-layer id="Roads" checked="Qt::Unchecked"/><layer-tree-layer id="Parcels" checked="Qt::Checked"/>
            <layer-tree-layer id="Topo" checked="Qt::Checked"/><layer-tree-layer id="Topo 2" checked="Qt::Checked"/>
            <layer-tree-layer id="Missing" checked="Qt::Checked"/><layer-tree-layer id="Gone" checked="Qt::Checked"/>
            <layer-tree-layer id="Topo 3" checked="Qt::Checked"/>
        </layer-tree-group><projectlayers>{}{}{}{}{}{}{}</projectlayers></qgis>"#,
        layer(
            "Roads",
            "vector",
            "WFS",
            &format!(
                "maxNumFeatures='5' pagingEnabled='true' sql='SELECT * FROM roads WHERE n &gt; 1' srsname='EPSG:2056' typename='roads' url='{base}/wfs' version='2.0.0'"
            ),
        ),
        layer(
            "Parcels",
            "vector",
            "arcgisfeatureserver",
            &format!("authcfg=abc1234 crs='EPSG:2056' url='{base}/arcgis/rest/services/P/FeatureServer/0'")
        ),
        layer(
            "Topo",
            "raster",
            "arcgismapserver",
            &format!("crs='EPSG:2056' format='PNG32' layer='5' url='{base}/arcgis/rest/services/T/MapServer'")
        ),
        layer("Topo 2", "raster", "arcgismapserver", &format!("layer='6' url='{base}/arcgis/rest/services/T/MapServer'")),
        layer("Missing", "vector", "WFS", &format!("typename='nope' url='{base}/wfs'")),
        layer("Gone", "raster", "arcgismapserver", &format!("layer='1' url='{base}/arcgis/rest/services/Gone/MapServer'")),
        layer("Topo 3", "raster", "arcgismapserver", &format!("layer='5,6' url='{base}/arcgis/rest/services/T/MapServer'")),
    );
    let mut s = Session::new(Project::default());
    let out = s.run("project.importQgis", &json!({"text": qgs})).unwrap();
    let notes = serde_json::to_string(&out["notes"]).unwrap();
    // The WFS layer: its features, hidden as in QGIS, its filter its definition query.
    let roads = s.map().layer_by_name("Roads").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(roads.dataset.features.len(), 3);
    assert!(!roads.visible);
    assert_eq!(roads.definition_query.as_deref(), Some("n > 1"));
    assert!(notes.contains("its source options maxNumFeatures=… (not read)"), "{notes}");
    // The ArcGIS feature layer, its login noted and never named.
    let parcels = s.map().layer_by_name("Parcels").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(parcels.dataset.features.len(), 2);
    assert!(notes.contains("its login") && !notes.contains("abc1234"), "{notes}");
    // The map service's layers 5 and 6 aren't ones it draws alone: the whole map, said. Both are
    // the same tiles, which the map keeps once: the lower one (added first) gives way, said.
    assert_eq!(s.map().tiles.len(), 1, "{out}");
    for id in [5, 6] {
        assert!(notes.contains(&format!("its map layer {id} alone, which the service doesn't list: the whole map")), "{notes}");
    }
    assert!(notes.contains("the same tiles as Topo, which takes its place"), "{notes}");
    // A type the service doesn't list: noted, the others still in.
    assert!(notes.contains(r#""kind":"skipped","layer":"Missing","text":"the layer: "#) && notes.contains("no layer 'nope'"), "{notes}");
    assert!(s.map().layer_by_name("Missing").is_none());
    assert_eq!(out["layers"].as_array().map(Vec::len), Some(3), "{out}");
    assert!(!notes.contains("abc1234"), "{notes}");
    // A map service that doesn't answer is left out: no fall back to its whole map.
    assert!(notes.contains(r#""kind":"skipped","layer":"Gone","text":"the layer: "#), "{notes}");
    assert!(!notes.contains("its map layer 1 alone"), "{notes}");
    // Two layers of it: its whole map, said as what is drawn, not as an unread setting.
    assert!(
        notes.contains(r#""layer":"Topo 3","text":"the layers QGIS draws of its map service (several, or by name): the whole map"#),
        "{notes}"
    );
    assert!(!notes.contains("layer=…"), "{notes}");
}

/// An OGC API Features collection comes in as features; a WMTS layer (QGIS's WMS source naming a
/// tile matrix set) is asked as a WMTS; a WCS coverage is asked over the view the project was
/// saved with.
#[test]
fn ogc_api_wmts_and_wcs_layers_come_in_from_their_services() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let r = Raster::new(
        "soc",
        8,
        8,
        vec![vec![3.0; 64]],
        GeoTransform { origin_x: 5.0, origin_y: 50.0, pixel_width: 0.1, pixel_height: -0.1 },
        Crs::LonLat,
        None,
        ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let tiff = rustgis_data::raster::geotiff::write(&r, 0).unwrap();
    let mut routes = vec![
        Route::new(ogc_server::ROOT, ogc_server::fixture_landing()),
        Route::new(ogc_server::CATALOG, ogc_server::fixture_collections()),
        Route::new(ogc_server::ITEMS, ogc_server::fixture_features()),
        Route::new("/wcs?SERVICE=WCS&REQUEST=GetCapabilities", ""),
        Route::new(
            "/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=DescribeCoverage&COVERAGEID=soc",
            r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
            <lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#,
        ),
        Route::bytes("/wcs", tiff),
    ];
    // Any answer: what is asked is what is checked.
    routes.push(Route::new("/wmts", "<nothing/>"));
    let server = Server::new(routes);
    let base = &server.base;
    server.set(
        "/wcs?SERVICE=WCS&REQUEST=GetCapabilities",
        &format!(
            r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
            <ows:OperationsMetadata><ows:Operation name="GetCoverage"><ows:DCP><ows:HTTP><ows:Get xlink:href="{base}/wcs?" xmlns:xlink="http://www.w3.org/1999/xlink"/></ows:HTTP></ows:DCP></ows:Operation></ows:OperationsMetadata>
            <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Soil carbon</ows:Title>
            <ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
            </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#
        ),
    );
    let layer = |id: &str, kind: &str, provider: &str, source: &str| {
        format!(
            r#"<maplayer type="{kind}"><id>{id}</id><datasource>{source}</datasource><layername>{id}</layername><provider>{provider}</provider></maplayer>"#
        )
    };
    let root = ogc_server::ROOT;
    let qgs = format!(
        r#"<qgis version="4.2.3"><projectCrs><spatialrefsys><authid>EPSG:4326</authid></spatialrefsys></projectCrs>
        <mapcanvas name="theMapCanvas"><extent><xmin>5.1</xmin><ymin>49.5</ymin><xmax>5.3</xmax><ymax>49.9</ymax></extent></mapcanvas>
        <layer-tree-group><layer-tree-layer id="Places" checked="Qt::Checked"/><layer-tree-layer id="Tiles" checked="Qt::Checked"/><layer-tree-layer id="Soil" checked="Qt::Checked"/></layer-tree-group>
        <projectlayers>{}{}{}</projectlayers></qgis>"#,
        layer("Places", "vector", "OAPIF", &format!("typename='places' url='{base}{root}'")),
        layer(
            "Tiles",
            "raster",
            "wms",
            &format!("crs=EPSG:4326&amp;format=image/png&amp;layers=roads&amp;styles=default&amp;tileMatrixSet=WGS84&amp;url={base}/wmts")
        ),
        layer(
            "Soil",
            "raster",
            "wcs",
            &format!("cache=PreferNetwork&amp;crs=EPSG:4326&amp;format=GTiff&amp;identifier=soc&amp;url={base}/wcs")
        ),
    );
    let mut s = Session::new(Project::default());
    let out = s.run("project.importQgis", &json!({"text": qgs})).unwrap();
    let notes = serde_json::to_string(&out["notes"]).unwrap();
    // The collection's features.
    let places = s.map().layer_by_name("Places").unwrap_or_else(|| panic!("{out}"));
    assert!(!places.dataset.features.is_empty(), "{out}");
    // The coverage, over the saved view (within the coverage).
    let soil = s.map().layer_by_name("Soil").unwrap_or_else(|| panic!("{out}"));
    assert!(soil.is_raster());
    let asked = server.requests();
    let get = asked
        .iter()
        .find(|r| r.path == "/wcs" && r.query.iter().any(|(k, v)| k == "REQUEST" && v == "GetCoverage"))
        .unwrap_or_else(|| panic!("{asked:?}"));
    let subsets: Vec<_> = get.query.iter().filter(|(k, _)| k == "SUBSET").map(|(_, v)| v.as_str()).collect();
    assert_eq!(subsets, ["Long(5.1,5.3)", "Lat(49.5,49.9)"], "{get:?}");
    // The WMTS layer was asked as one (its answer here isn't one: noted).
    assert!(asked.iter().any(|r| r.path == "/wmts" && r.query.iter().any(|(k, v)| k == "SERVICE" && v == "WMTS")), "{asked:?}");
    assert!(notes.contains(r#""layer":"Tiles","text":"the layer: "#), "{notes}");
    // Its tile matrix set is RustGIS's choice, not QGIS's: said for the WMTS layer, and for no other.
    assert!(notes.contains(r#""layer":"Tiles","text":"its tile matrix set: RustGIS takes the service's Web Mercator one"#), "{notes}");
    assert_eq!(notes.matches("its tile matrix set").count(), 1, "{notes}");
    assert!(notes.contains(r#""layer":"Soil","text":"its coverage, read once over the view the project was saved with"#), "{notes}");
    // A layer whose source says nothing RustGIS doesn't read comes in with no note.
    assert!(!notes.contains(r#""layer":"Places""#), "{notes}");
}

/// A WCS coverage in a polar project is asked for up to the pole the saved view holds, all the way
/// round (the view's corners alone miss it); with no saved view, whole, and said either way.
#[test]
fn a_wcs_coverage_reaches_the_pole_a_polar_view_holds() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let r = Raster::new(
        "ice",
        8,
        4,
        vec![vec![1.0; 32]],
        GeoTransform { origin_x: -180.0, origin_y: 90.0, pixel_width: 45.0, pixel_height: -45.0 },
        Crs::LonLat,
        None,
        ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let server = Server::new(vec![
        Route::new("/wcs?SERVICE=WCS&REQUEST=GetCapabilities", ""),
        Route::new(
            "/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=DescribeCoverage&COVERAGEID=ice",
            r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
            <lowerCorner>-90 -180</lowerCorner><upperCorner>90 180</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#,
        ),
        Route::bytes("/wcs", rustgis_data::raster::geotiff::write(&r, 0).unwrap()),
    ]);
    let base = &server.base;
    server.set(
        "/wcs?SERVICE=WCS&REQUEST=GetCapabilities",
        &format!(
            r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
            <ows:OperationsMetadata><ows:Operation name="GetCoverage"><ows:DCP><ows:HTTP><ows:Get xlink:href="{base}/wcs?" xmlns:xlink="http://www.w3.org/1999/xlink"/></ows:HTTP></ows:DCP></ows:Operation></ows:OperationsMetadata>
            <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>ice</wcs:CoverageId><ows:Title>Ice</ows:Title>
            <ows:WGS84BoundingBox><ows:LowerCorner>-180 -90</ows:LowerCorner><ows:UpperCorner>180 90</ows:UpperCorner></ows:WGS84BoundingBox>
            </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#
        ),
    );
    let layer = format!(
        r#"<maplayer type="raster"><id>Ice</id><datasource>identifier=ice&amp;url={base}/wcs</datasource><layername>Ice</layername><provider>wcs</provider></maplayer>"#
    );
    // Polar stereographic north, the view ±1000 km round the pole.
    let polar = format!(
        r#"<qgis version="4.2.3"><projectCrs><spatialrefsys><authid>EPSG:3413</authid></spatialrefsys></projectCrs>
        <mapcanvas name="theMapCanvas"><extent><xmin>-1000000</xmin><ymin>-1000000</ymin><xmax>1000000</xmax><ymax>1000000</ymax></extent></mapcanvas>
        <projectlayers>{layer}</projectlayers></qgis>"#
    );
    let mut s = Session::new(Project::default());
    let out = s.run("project.importQgis", &json!({"text": polar})).unwrap();
    let asked = server.requests();
    let subsets = |asked: &[ogc_server::Request]| -> Vec<String> {
        let get = asked
            .iter()
            .rev()
            .find(|r| r.path == "/wcs" && r.query.iter().any(|(k, v)| k == "REQUEST" && v == "GetCoverage"))
            .unwrap_or_else(|| panic!("{asked:?}"));
        get.query.iter().filter(|(k, _)| k == "SUBSET").map(|(_, v)| v.clone()).collect()
    };
    let polar_subsets = subsets(&asked);
    assert_eq!(polar_subsets.first().map(String::as_str), Some("Long(-180,180)"), "{out}");
    assert!(polar_subsets.get(1).is_some_and(|lat| lat.ends_with(",90)")), "{polar_subsets:?}");
    // Web Mercator draws a pole as its top edge, not a point: a view reaching it isn't widened.
    let mercator = format!(
        r#"<qgis version="4.2.3"><projectCrs><spatialrefsys><authid>EPSG:3857</authid></spatialrefsys></projectCrs>
        <mapcanvas name="theMapCanvas"><extent><xmin>-1000000</xmin><ymin>19000000</ymin><xmax>1000000</xmax><ymax>20100000</ymax></extent></mapcanvas>
        <projectlayers>{layer}</projectlayers></qgis>"#
    );
    // Each import's own coverage request: one it skips fails here rather than finding the last's.
    let import = |s: &mut Session, text: &str| {
        let before = server.requests().len();
        let out = s.run("project.importQgis", &json!({"text": text})).unwrap();
        let asked = server.requests();
        (out, subsets(asked.get(before..).unwrap_or_default()))
    };
    let (_, narrow) = import(&mut s, &mercator);
    assert!(narrow.first().is_some_and(|lon| lon.starts_with("Long(-8.98") && lon.contains(",8.98")), "{narrow:?}");
    // No saved view: the whole coverage, said.
    let whole = format!(r#"<qgis version="4.2.3"><projectlayers>{layer}</projectlayers></qgis>"#);
    let (out, asked) = import(&mut s, &whole);
    assert!(serde_json::to_string(&out["notes"]).unwrap().contains("its coverage, read once and whole"), "{out}");
    assert_eq!(asked, ["Long(-180,180)", "Lat(-90,90)"]);
    // A saved view with no coordinate system it is in: no view either, not one read as Web
    // Mercator's.
    let unplaced = format!(
        r#"<qgis version="4.2.3"><mapcanvas name="theMapCanvas"><extent><xmin>-1000000</xmin><ymin>-1000000</ymin><xmax>1000000</xmax><ymax>1000000</ymax></extent></mapcanvas>
        <projectlayers>{layer}</projectlayers></qgis>"#
    );
    let (out, asked) = import(&mut s, &unplaced);
    assert!(serde_json::to_string(&out["notes"]).unwrap().contains("its coverage, read once and whole"), "{out}");
    assert_eq!(asked, ["Long(-180,180)", "Lat(-90,90)"]);
    // A view in degrees across the antimeridian (170° to 190°): both sides of it, all the way round.
    let across = format!(
        r#"<qgis version="4.2.3"><projectCrs><spatialrefsys><authid>EPSG:4326</authid></spatialrefsys></projectCrs>
        <mapcanvas name="theMapCanvas"><extent><xmin>170</xmin><ymin>40</ymin><xmax>190</xmax><ymax>50</ymax></extent></mapcanvas>
        <projectlayers>{layer}</projectlayers></qgis>"#
    );
    let (out, asked) = import(&mut s, &across);
    assert!(serde_json::to_string(&out["notes"]).unwrap().contains("its coverage, read once over the view"), "{out}");
    assert_eq!(asked, ["Long(-180,180)", "Lat(40,50)"]);
    // A Web Mercator view across the antimeridian, given unwrapped (170° to 190° east): both sides.
    let unwrapped = format!(
        r#"<qgis version="4.2.3"><projectCrs><spatialrefsys><authid>EPSG:3857</authid></spatialrefsys></projectCrs>
        <mapcanvas name="theMapCanvas"><extent><xmin>18924313</xmin><ymin>4865942</ymin><xmax>21150708</xmax><ymax>6446276</ymax></extent></mapcanvas>
        <projectlayers>{layer}</projectlayers></qgis>"#
    );
    let (_, asked) = import(&mut s, &unwrapped);
    assert_eq!(asked.first().map(String::as_str), Some("Long(-180,180)"), "{asked:?}");
}
