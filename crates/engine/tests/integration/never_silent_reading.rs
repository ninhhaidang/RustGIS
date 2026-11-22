//! Never silent (AGENTS.md), for reading: a file that loses something on the way in says so.
//!
//! Each case is a pair of small files, read with `layer.addData` in a fresh session from the same
//! place: a clean file, and the same file with one thing in it that reading loses (a height, an
//! unreadable coordinate, a hole, a part). The lossy file must give a notice the clean one doesn't
//! that says the loss (holds the case's words), or be refused saying it. Notices both give (a DXF
//! drawing without a coordinate system, say) cancel out, so a case asks only about its loss.
//!
//! Losses no reader says yet are listed as [`Expect::Silent`] with what is lost. They are the
//! roadmap's known silent losses (ROADMAP.md › Correct results); a reader that starts saying one
//! fails here until the case is moved to [`Expect::Said`], so the list only shrinks. Exports have
//! their own test, `never_silent_roundtrip.rs`.
//!
//! The table includes text, CityJSON, CityGML, KMZ, GeoPackage, FlatGeobuf, GeoParquet, FileGDB,
//! Shapefile, XLSX, ODS, XLS, XLSB, OSM PBF and feature-service PBF. More formats and loss kinds remain to add (ROADMAP.md); one loss
//! does not certify a reader.
use rustgis_engine::data::loss::LossCode;
use rustgis_engine::{Project, Session};
use serde_json::json;

/// What reading a case's lossy file does.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// Says the loss: a notice the clean file doesn't give, or a refusal, holding these words.
    Said(&'static str),
    /// Imports the layer and says the loss in a notice; refusing this valid fixture is a failure.
    Imported(&'static str),
    /// Imports successfully and identifies the loss with a code.
    Classified(LossCode),
    /// Loses it without a notice today, still adding a layer: what is lost.
    Silent(&'static str),
}
use Expect::{Imported, Said, Silent};

/// A clean file and the same file with a loss in it.
struct Case {
    /// The reader and the loss, to name a failure.
    name: &'static str,
    /// The file name, whose extension picks the reader.
    file: &'static str,
    clean: Vec<u8>,
    lossy: Vec<u8>,
    /// Files beside the clean and lossy inputs (binary indexes can differ).
    beside: [Vec<(&'static str, Vec<u8>)>; 2],
    expect: Expect,
}

fn case(name: &'static str, file: &'static str, clean: impl AsRef<[u8]>, lossy: impl AsRef<[u8]>, expect: Expect) -> Case {
    Case { name, file, clean: clean.as_ref().to_vec(), lossy: lossy.as_ref().to_vec(), beside: [Vec::new(), Vec::new()], expect }
}

/// A GeoJSON feature collection of these geometries.
fn geojson(geometries: &[&str]) -> String {
    let features: Vec<String> =
        geometries.iter().enumerate().map(|(i, g)| format!(r#"{{"type":"Feature","properties":{{"n":{i}}},"geometry":{g}}}"#)).collect();
    format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","))
}

/// A KML document of these placemarks' contents.
fn kml(placemarks: &[&str]) -> String {
    let body: String = placemarks.iter().map(|p| format!("<Placemark>{p}</Placemark>")).collect();
    format!(r#"<?xml version="1.0"?><kml xmlns="http://www.opengis.net/kml/2.2"><Document>{body}</Document></kml>"#)
}

/// A GPX file around this content.
fn gpx(content: &str) -> String {
    format!(r#"<?xml version="1.0"?><gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">{content}</gpx>"#)
}

/// An RSS feed of these items' contents.
fn georss(items: &[&str]) -> String {
    let body: String = items.iter().map(|i| format!("<item><title>i</title>{i}</item>")).collect();
    format!(
        r#"<?xml version="1.0"?><rss version="2.0" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml"><channel><title>t</title>{body}</channel></rss>"#
    )
}

/// A GML feature collection with one road of this geometry.
fn gml(geometry: &str) -> String {
    format!(
        r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:app="http://example.com/app"><wfs:member><app:roads gml:id="r.1"><app:name>Main</app:name><app:geom>{geometry}</app:geom></app:roads></wfs:member></wfs:FeatureCollection>"#
    )
}

/// An ASCII DXF drawing of these entities, each written as group codes and values separated by
/// spaces.
fn dxf(entities: &[&str]) -> String {
    let words: Vec<&str> =
        ["0 SECTION 2 ENTITIES"].iter().chain(entities).chain(&["0 ENDSEC 0 EOF"]).flat_map(|e| e.split_whitespace()).collect();
    words.join("\n") + "\n"
}

/// A MapInfo MIF file of these objects, with one integer column.
fn mif(objects: &str) -> String {
    format!("Version 300\nCoordSys Earth Projection 1, 104\nColumns 1\n  id Integer\nData\n{objects}")
}

/// A LandXML file of this content.
fn landxml(content: &str) -> String {
    format!(r#"<?xml version="1.0"?><LandXML xmlns="http://www.landxml.org/schema/LandXML-1.2" version="1.2">{content}</LandXML>"#)
}

/// A zipped Shapefile of one point whose text field `NAME` holds these bytes. It names no
/// encoding: it has no `.cpg`, and its `.dbf` no language driver.
fn shapefile_zip(name: &[u8]) -> Vec<u8> {
    use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, io};
    // Written as a placeholder of the same length, which the bytes then replace in the `.dbf`.
    let holder = "A".repeat(name.len());
    let mut ds = Dataset::new("a", GeometryType::Point, vec![Field::new("NAME", FieldType::Text)]);
    ds.push(Some(Geometry::Point(Coord::new(121.5, 25.0))), vec![Value::Text(holder.clone())]);
    let mut entries = Vec::new();
    for (ext, mut bytes) in io::shapefile_parts(&ds).unwrap().0 {
        if ext == "cpg" {
            continue;
        }
        if ext == "dbf" {
            bytes[29] = 0;
            let at = bytes.windows(holder.len()).position(|w| w == holder.as_bytes()).unwrap();
            bytes[at..at + name.len()].copy_from_slice(name);
        }
        entries.push((format!("a.{ext}"), bytes));
    }
    rustgis_engine::data::zip::write(&entries).unwrap()
}

/// Builds paired inputs, keeping sidecars specific to each input when their binary layout differs.
fn cases(fixture_dir: &std::path::Path) -> Vec<Case> {
    let point = r#"{"type":"Point","coordinates":[1,2]}"#;
    let topo_line = |arcs: &str| format!(r#"{{"type":"Topology","objects":{{"o":{{"type":"LineString","arcs":[0]}}}},"arcs":[{arcs}]}}"#);
    // Quantized and delta-encoded.
    let topo_quantized = |arcs: &str| {
        format!(
            r#"{{"type":"Topology","transform":{{"scale":[0.5,0.5],"translate":[10,20]}},"objects":{{"o":{{"type":"LineString","arcs":[0]}}}},"arcs":[{arcs}]}}"#
        )
    };
    // A collection inside the object's: its members, a point and those given.
    let topo_nested = |more: &str| {
        format!(
            r#"{{"type":"Topology","objects":{{"o":{{"type":"GeometryCollection","geometries":[{{"type":"GeometryCollection","geometries":[{{"type":"Point","coordinates":[1,2]}}{more}]}}]}}}},"arcs":[[[0,0],[1,1]]]}}"#
        )
    };
    let kml_polygon = |hole: &str| {
        format!(
            "<Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 4,0 4,4 0,0</coordinates></LinearRing></outerBoundaryIs><innerBoundaryIs><LinearRing><coordinates>{hole}</coordinates></LinearRing></innerBoundaryIs></Polygon>"
        )
    };
    let georss_polygon = |hole: &str| {
        format!(
            "<georss:where><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>0 0 0 4 4 4 0 0</gml:posList></gml:LinearRing></gml:exterior><gml:interior><gml:LinearRing><gml:posList>{hole}</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></georss:where>"
        )
    };
    let polyline =
        |middle: &str| format!("0 POLYLINE 8 0 66 1 70 0 0 VERTEX 8 0 10 0 20 0 0 VERTEX 8 0 {middle} 0 VERTEX 8 0 10 2 20 2 0 SEQEND");
    let parcel = r#"<Parcels><Parcel name="Lot"><CoordGeom><Line><Start>0 0</Start><End>0 10</End></Line><Line><Start>0 10</Start><End>10 10</End></Line><Line><Start>10 10</Start><End>0 0</End></Line></CoordGeom></Parcel></Parcels>"#;
    let cg_points = |second: &str| format!(r#"<CgPoints><CgPoint name="a">0 0</CgPoint><CgPoint name="b">{second}</CgPoint></CgPoints>"#);

    let mut out = vec![
        // GeoJSON.
        case("GeoJSON: a height", "a.geojson", geojson(&[point]), geojson(&[r#"{"type":"Point","coordinates":[1,2,3]}"#]), Said("Z or M")),
        case(
            "GeoJSON: an unreadable geometry",
            "a.geojson",
            geojson(&[point, point]),
            geojson(&[point, r#"{"type":"Point","coordinates":["x",2]}"#]),
            Said("unreadable geometry"),
        ),
        // TopoJSON.
        case("TopoJSON: a height", "a.topojson", topo_line("[[0,0],[1,1]]"), topo_line("[[0,0,5],[1,1,6]]"), Said("Z or M")),
        case(
            "TopoJSON: an arc index the file doesn't hold",
            "a.topojson",
            topo_line("[[0,0],[1,1]]"),
            r#"{"type":"Topology","objects":{"o":{"type":"LineString","arcs":[7]}},"arcs":[[[0,0],[1,1]]]}"#,
            Said("geometry couldn't be read"),
        ),
        case(
            "TopoJSON: an unreadable position in a quantized arc",
            "a.topojson",
            topo_quantized("[[0,0],[1,1],[1,1]]"),
            topo_quantized(r#"[[0,0],["x",1],[1,1]]"#),
            Said("geometry couldn't be read"),
        ),
        case(
            "TopoJSON: an unreadable member of a nested collection",
            "a.topojson",
            topo_nested(""),
            topo_nested(r#",{"type":"LineString","arcs":[9]}"#),
            Said("geometry couldn't be read"),
        ),
        // CSV.
        case(
            "CSV: unreadable WKT",
            "a.csv",
            "wkt,id\n\"POINT(1 2)\",1\n\"POINT(3 4)\",2\n",
            "wkt,id\n\"POINT(1 2)\",1\n\"POINT(3\",2\n",
            Said("isn't readable WKT"),
        ),
        case("CSV: a height in WKT", "a.csv", "wkt,id\n\"POINT(1 2)\",1\n", "wkt,id\n\"POINT Z(1 2 3)\",1\n", Said("Z or M")),
        case(
            "CSV: an unreadable coordinate",
            "a.csv",
            "lon,lat,id\n1,2,a\n3,4,b\n",
            "lon,lat,id\n1,2,a\n3,x,b\n",
            Said("no usable coordinates"),
        ),
        case(
            "CSV: a WKT geometry collection's later members",
            "a.csv",
            "wkt,id\n\"GEOMETRYCOLLECTION(POINT(1 2))\",1\n",
            "wkt,id\n\"GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4))\",1\n",
            Imported("1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"),
        ),
        case(
            "CSV: cells past the header",
            "a.csv",
            "lon,lat\n1,2\n",
            "lon,lat\n1,2,lost\n",
            Silent("a row's cells beyond the header's columns are left out"),
        ),
        // KML.
        case(
            "KML: a height",
            "a.kml",
            kml(&["<Point><coordinates>1,2</coordinates></Point>"]),
            kml(&["<Point><coordinates>1,2,30</coordinates></Point>"]),
            Said("Z or M"),
        ),
        case(
            "KML: a placemark without geometry",
            "a.kml",
            kml(&["<Point><coordinates>1,2</coordinates></Point>", "<Point><coordinates>3,4</coordinates></Point>"]),
            kml(&["<Point><coordinates>1,2</coordinates></Point>", "<name>none</name>"]),
            Said("without a geometry"),
        ),
        case(
            "KML: an unreadable coordinate in a line",
            "a.kml",
            kml(&["<LineString><coordinates>0,0 1,1 2,2</coordinates></LineString>"]),
            kml(&["<LineString><coordinates>0,0 x,1 2,2</coordinates></LineString>"]),
            Silent("the coordinate is skipped and the line drawn without it"),
        ),
        case(
            "KML: an unreadable hole",
            "a.kml",
            kml(&[&kml_polygon("1,1 2,1 2,2 1,1")]),
            kml(&[&kml_polygon("1,1 x,y")]),
            Silent("the hole is left out of the polygon"),
        ),
        case(
            "KML: an unreadable part",
            "a.kml",
            kml(&[
                "<MultiGeometry><Point><coordinates>1,1</coordinates></Point><LineString><coordinates>0,0 1,1</coordinates></LineString></MultiGeometry>",
            ]),
            kml(&[
                "<MultiGeometry><Point><coordinates>1,1</coordinates></Point><LineString><coordinates>0,0</coordinates></LineString></MultiGeometry>",
            ]),
            Silent("a line of one point is left out of the placemark"),
        ),
        case(
            "KML: a point with more than one coordinate",
            "a.kml",
            kml(&["<Point><coordinates>1,1</coordinates></Point>"]),
            kml(&["<Point><coordinates>1,1 2,2</coordinates></Point>"]),
            Silent("only the first coordinate is kept"),
        ),
        // GPX.
        case(
            "GPX: a waypoint without usable coordinates",
            "a.gpx",
            gpx(r#"<wpt lat="1" lon="1"/><wpt lat="2" lon="2"/>"#),
            gpx(r#"<wpt lat="1" lon="1"/><wpt lat="91" lon="2"/>"#),
            Said("without usable coordinates"),
        ),
        case(
            "GPX: a track segment of one point",
            "a.gpx",
            gpx(
                r#"<trk><trkseg><trkpt lat="1" lon="1"/><trkpt lat="2" lon="2"/></trkseg><trkseg><trkpt lat="3" lon="3"/><trkpt lat="4" lon="4"/></trkseg></trk>"#,
            ),
            gpx(r#"<trk><trkseg><trkpt lat="1" lon="1"/><trkpt lat="2" lon="2"/></trkseg><trkseg><trkpt lat="3" lon="3"/></trkseg></trk>"#),
            Silent("the segment is left out of the track's line"),
        ),
        // Feature-service PBF has a distinct sniffed parser from OSM PBF.
        case(
            "feature-service PBF: a height",
            "a.feature.pbf",
            include_bytes!("../../testdata/reading-losses/feature-xy.pbf"),
            include_bytes!("../../testdata/reading-losses/feature-xyz.pbf"),
            Imported("1 feature had Z or M values"),
        ),
        case(
            "feature-service PBF: a measure",
            "a.feature.pbf",
            include_bytes!("../../testdata/reading-losses/feature-xy.pbf"),
            include_bytes!("../../testdata/reading-losses/feature-xym.pbf"),
            Imported("1 feature had Z or M values"),
        ),
        // OSM PBF: keep the outer polygon while one inner member is lost.
        case(
            "OSM PBF: a missing inner member way",
            "a.osm.pbf",
            include_bytes!("../../testdata/reading-losses/hole-clean.osm.pbf"),
            include_bytes!("../../testdata/reading-losses/hole-missing-member.osm.pbf"),
            Silent("the hole whose member way the file does not hold is left out"),
        ),
        case(
            "OSM PBF: a missing inner node",
            "a.osm.pbf",
            include_bytes!("../../testdata/reading-losses/hole-clean.osm.pbf"),
            include_bytes!("../../testdata/reading-losses/hole-missing-node.osm.pbf"),
            Silent("the hole whose member way refers to a missing node is left out"),
        ),
        case(
            "OSM PBF: an unclosed inner ring",
            "a.osm.pbf",
            include_bytes!("../../testdata/reading-losses/hole-clean.osm.pbf"),
            include_bytes!("../../testdata/reading-losses/hole-unclosed.osm.pbf"),
            Silent("the inner member way that does not close is left out"),
        ),
        case(
            "OSM PBF: an inner ring outside its outer",
            "a.osm.pbf",
            include_bytes!("../../testdata/reading-losses/hole-clean.osm.pbf"),
            include_bytes!("../../testdata/reading-losses/hole-outside.osm.pbf"),
            Silent("the inner ring outside every outer ring is left out"),
        ),
        // GML.
        case(
            "GML: a height",
            "a.gml",
            gml(r#"<gml:LineString srsName="urn:ogc:def:crs:EPSG::4326"><gml:posList>48 10 49 11</gml:posList></gml:LineString>"#),
            gml(
                r#"<gml:LineString srsName="urn:ogc:def:crs:EPSG::4326"><gml:posList srsDimension="3">48 10 5 49 11 6</gml:posList></gml:LineString>"#,
            ),
            Said("Z or M"),
        ),
        // GeoRSS.
        case(
            "GeoRSS: an unreadable place",
            "a.rss",
            georss(&["<georss:point>45 -71</georss:point>", "<georss:point>46 -72</georss:point>"]),
            georss(&["<georss:point>45 -71</georss:point>", "<georss:point>46 x</georss:point>"]),
            Said("couldn't be read"),
        ),
        case(
            "GeoRSS: an unreadable hole",
            "a.rss",
            georss(&[&georss_polygon("1 1 1 2 2 2 1 1")]),
            georss(&[&georss_polygon("1 x")]),
            Expect::Classified(LossCode::DroppedRing),
        ),
        case(
            "GeoRSS: a point with more than one pair",
            "a.rss",
            georss(&["<georss:point>45 -71</georss:point>"]),
            georss(&["<georss:point>45 -71 46 -72</georss:point>"]),
            Silent("only the first pair is kept"),
        ),
        // DXF.
        case(
            "DXF: an unreadable entity",
            "a.dxf",
            dxf(&["0 LINE 8 0 10 0 20 0 11 1 21 1", "0 LINE 8 0 10 2 20 2 11 3 21 3"]),
            dxf(&["0 LINE 8 0 10 0 20 0 11 1 21 1", "0 LINE 8 0 10 2 20 2 11 3"]),
            Said("couldn't be read or placed"),
        ),
        case(
            "DXF: a height that differs along a line",
            "a.dxf",
            dxf(&["0 LINE 8 0 10 0 20 0 30 5 11 1 21 1 31 5"]),
            dxf(&["0 LINE 8 0 10 0 20 0 30 5 11 1 21 1 31 9"]),
            Imported("1 feature lost varying Z coordinates; only the elevation attribute is kept"),
        ),
        case(
            "DXF: an unreadable polyline vertex",
            "a.dxf",
            dxf(&[&polyline("10 1 20 1")]),
            dxf(&[&polyline("10 1")]),
            Silent("the vertex is skipped and the line drawn without it"),
        ),
        // LandXML.
        case(
            "LandXML: an unreadable point",
            "a.xml",
            landxml(&cg_points("1 1")),
            landxml(&cg_points("x y")),
            Said("couldn't be read and were left out"),
        ),
        case(
            "LandXML: a parcel without geometry",
            "a.xml",
            landxml(&format!("{}{parcel}", cg_points("1 1"))),
            landxml(&format!(r#"{}<Parcels><Parcel name="Lot"/></Parcels>"#, cg_points("1 1"))),
            Silent("the parcel is left out"),
        ),
        // Shapefile. 臺北101 as UTF-8, and in Big5, which isn't UTF-8.
        case(
            "Shapefile: text in a code page it doesn't name",
            "a.zip",
            shapefile_zip("臺北101".as_bytes()),
            shapefile_zip(b"\xbb\x4f\xa5\x5f101"),
            Imported("text value isn't valid UTF-8"),
        ),
    ];

    out.extend([
        case(
            "XLSX: a height in WKT",
            "a.xlsx",
            include_bytes!("../../testdata/reading-losses/book-clean.xlsx"),
            include_bytes!("../../testdata/reading-losses/book-z.xlsx"),
            Imported("Z or M"),
        ),
        case(
            "XLSX: a WKT geometry collection's later members",
            "a.xlsx",
            include_bytes!("../../testdata/reading-losses/book-clean.xlsx"),
            include_bytes!("../../testdata/reading-losses/book-collection.xlsx"),
            Imported("1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"),
        ),
        case(
            "XLSX: an error-valued attribute cell",
            "a.xlsx",
            include_bytes!("../../testdata/reading-losses/book-clean.xlsx"),
            include_bytes!("../../testdata/reading-losses/book-error.xlsx"),
            Imported("1 spreadsheet error cell was read as empty, in 'note'"),
        ),
    ]);

    out.extend([
        case(
            "ODS: a height in WKT",
            "a.ods",
            include_bytes!("../../testdata/reading-losses/book-clean.ods"),
            include_bytes!("../../testdata/reading-losses/book-z.ods"),
            Imported("Z or M"),
        ),
        case(
            "ODS: a WKT geometry collection's later members",
            "a.ods",
            include_bytes!("../../testdata/reading-losses/book-clean.ods"),
            include_bytes!("../../testdata/reading-losses/book-collection.ods"),
            Imported("1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"),
        ),
    ]);

    out.extend([
        case(
            "XLS: a height in WKT",
            "a.xls",
            include_bytes!("../../testdata/reading-losses/book-clean.xls"),
            include_bytes!("../../testdata/reading-losses/book-z.xls"),
            Imported("Z or M"),
        ),
        case(
            "XLS: a WKT geometry collection's later members",
            "a.xls",
            include_bytes!("../../testdata/reading-losses/book-clean.xls"),
            include_bytes!("../../testdata/reading-losses/book-collection.xls"),
            Imported("1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"),
        ),
        case(
            "XLSB: a height in WKT",
            "a.xlsb",
            include_bytes!("../../testdata/reading-losses/book-clean.xlsb"),
            include_bytes!("../../testdata/reading-losses/book-z.xlsb"),
            Imported("Z or M"),
        ),
        case(
            "XLSB: short cell records lose attribute fields and values",
            "a.xlsb",
            include_bytes!("../../testdata/reading-losses/book-clean.xlsb"),
            include_bytes!("../../testdata/reading-losses/book-short.xlsb"),
            Silent("the point remains but all attribute fields and values are lost"),
        ),
        case(
            "XLSB: a WKT geometry collection's later members",
            "a.xlsb",
            include_bytes!("../../testdata/reading-losses/book-clean.xlsb"),
            include_bytes!("../../testdata/reading-losses/book-collection.xlsb"),
            Imported("1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"),
        ),
    ]);

    // MapInfo: the .mid beside each .mif holds one row per object.
    let mut mapinfo = |name, clean: &str, lossy: &str, mid: &str, expect| {
        let mut c = case(name, "a.mif", mif(clean), mif(lossy), expect);
        for beside in &mut c.beside {
            beside.push(("a.mid", mid.as_bytes().to_vec()));
        }
        out.push(c);
    };
    mapinfo("MapInfo: an unreadable object", "Point 0 0\nPoint 1 1\n", "Point 0 0\nPoint 1 x\n", "1\n2\n", Said("couldn't be read"));
    mapinfo("MapInfo: an arc", "Point 0 0\n", "Arc 0 0 2 2 0 90\n", "1\n", Silent("an arc or text object is kept without its geometry"));
    mapinfo(
        "MapInfo: a line part of one point",
        "Pline Multiple 2\n2\n0 0\n1 1\n2\n5 5\n6 6\n",
        "Pline Multiple 2\n2\n0 0\n1 1\n1\n5 5\n",
        "1\n",
        Silent("the part is left out of the line"),
    );
    mapinfo(
        "MapInfo: a ring of two points",
        "Region 2\n4\n0 0\n4 0\n4 4\n0 0\n4\n1 1\n2 1\n2 2\n1 1\n",
        "Region 2\n4\n0 0\n4 0\n4 4\n0 0\n2\n1 1\n2 2\n",
        "1\n",
        Silent("the ring is left out of the polygon"),
    );
    // The archive path must report the same losses as the loose KML reader. Derive both
    // fixtures from each KML row so a new loss cannot be tested in only one container.
    let kmz = out
        .iter()
        .filter(|c| c.file == "a.kml")
        .map(|c| Case {
            name: c.name,
            file: "a.kmz",
            clean: rustgis_engine::data::zip::write(&[("doc.kml".into(), c.clean.clone())]).unwrap(),
            lossy: rustgis_engine::data::zip::write(&[("doc.kml".into(), c.lossy.clone())]).unwrap(),
            beside: [Vec::new(), Vec::new()],
            expect: c.expect,
        })
        .collect::<Vec<_>>();
    out.extend(kmz);
    out.extend([
        case("CityJSON: varying line heights", "a.city.json", cityjson(0), cityjson(5), Said("Z or M")),
        case("CityGML: varying line heights", "a.gml", citygml(0), citygml(5), Said("Z or M")),
        case("GeoPackage: a height", "a.gpkg", gpkg(fixture_dir, false), gpkg(fixture_dir, true), Said("Z or M")),
        case(
            "FlatGeobuf: a height",
            "a.fgb",
            include_bytes!("../../testdata/reading-losses/xy.fgb"),
            include_bytes!("../../testdata/reading-losses/xyz.fgb"),
            Imported("Z or M"),
        ),
        case(
            "GeoParquet: a height",
            "a.parquet",
            include_bytes!("../../testdata/reading-losses/xy.parquet"),
            include_bytes!("../../testdata/reading-losses/xyz.parquet"),
            Imported("Z or M"),
        ),
        case(
            "FileGDB: a height",
            "a.gdb",
            include_bytes!("../../testdata/reading-losses/xy.gdb.zip"),
            include_bytes!("../../testdata/reading-losses/xyz.gdb.zip"),
            Imported("Z or M"),
        ),
    ]);
    // Independently encoded shapes: scalar Z, scalar M, and arrays with/without optional M.
    for (name, clean, lossy, clean_index, lossy_index) in [
        (
            "Shapefile: a point height",
            include_bytes!("../../testdata/reading-losses/shp-point-xy.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-point-z.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-point-xy.shx"),
            include_bytes!("../../testdata/reading-losses/shp-point-z.shx"),
        ),
        (
            "Shapefile: a point measure",
            include_bytes!("../../testdata/reading-losses/shp-point-xy.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-point-m.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-point-xy.shx"),
            include_bytes!("../../testdata/reading-losses/shp-point-m.shx"),
        ),
        (
            "Shapefile: line heights without measures",
            include_bytes!("../../testdata/reading-losses/shp-line-xy.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-line-z.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-line-xy.shx"),
            include_bytes!("../../testdata/reading-losses/shp-line-z.shx"),
        ),
        (
            "Shapefile: line heights and measures",
            include_bytes!("../../testdata/reading-losses/shp-line-xy.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-line-zm.shp").as_slice(),
            include_bytes!("../../testdata/reading-losses/shp-line-xy.shx"),
            include_bytes!("../../testdata/reading-losses/shp-line-zm.shx"),
        ),
    ] {
        let mut c = case(name, "a.shp", clean, lossy, Imported("Z or M"));
        for (beside, index) in c.beside.iter_mut().zip([clean_index, lossy_index]) {
            beside.extend([
                ("a.shx", index.to_vec()),
                ("a.dbf", include_bytes!("../../testdata/reading-losses/shp-row.dbf").to_vec()),
                ("a.cpg", b"UTF-8\n".to_vec()),
                ("a.prj", br#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#.to_vec()),
            ]);
        }
        out.push(c);
    }
    out
}

/// A line's uniform height is retained in city-model attributes; a varying one is lost.
fn cityjson(z: i32) -> String {
    json!({
        "type": "CityJSON", "version": "2.0",
        "transform": {"scale": [1, 1, 1], "translate": [0, 0, 0]},
        "metadata": {"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/4326"},
        "vertices": [[1, 2, 0], [3, 4, z]],
        "CityObjects": {"road": {"type": "Road", "geometry": [{"type": "MultiLineString", "lod": "0", "boundaries": [[0, 1]]}]}}
    })
    .to_string()
}

/// CityGML's line path shares the height policy with CityJSON but has its own parser.
fn citygml(z: i32) -> String {
    format!(
        r#"<core:CityModel xmlns:core="http://www.opengis.net/citygml/2.0"
        xmlns:bldg="http://www.opengis.net/citygml/building/2.0" xmlns:gml="http://www.opengis.net/gml">
        <gml:boundedBy><gml:Envelope srsName="EPSG:4326" srsDimension="3">
        <gml:lowerCorner>1 2 0</gml:lowerCorner><gml:upperCorner>3 4 5</gml:upperCorner>
        </gml:Envelope></gml:boundedBy><core:cityObjectMember><bldg:Building gml:id="line">
        <bldg:lod1MultiCurve><gml:MultiCurve><gml:curveMember><gml:LineString>
        <gml:posList>1 2 0 3 4 {z}</gml:posList></gml:LineString></gml:curveMember></gml:MultiCurve>
        </bldg:lod1MultiCurve></bldg:Building></core:cityObjectMember></core:CityModel>"#
    )
}

/// An independently encoded ISO WKB point in a minimal GeoPackage. Do not use the 2D
/// RustGIS exporter: it cannot produce the height whose loss this pair exercises.
fn gpkg(dir: &std::path::Path, z: bool) -> Vec<u8> {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(if z { "xyz.gpkg" } else { "xy.gpkg" });
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch(
        "PRAGMA application_id = 1196444487;
         CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         INSERT INTO gpkg_spatial_ref_sys VALUES ('WGS 84', 4326, 'EPSG', 4326, '', '');
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('points', 'features', 4326);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);
         CREATE TABLE points (fid INTEGER PRIMARY KEY, geom BLOB);"
    ).unwrap();
    c.execute("INSERT INTO gpkg_geometry_columns VALUES ('points', 'geom', 'POINT', 4326, ?1, 0)", [i32::from(z)]).unwrap();
    let mut blob = b"GP\x00\x01".to_vec();
    blob.extend(4326i32.to_le_bytes());
    blob.push(1); // Little-endian WKB, Point or PointZ.
    blob.extend(if z { 1001u32 } else { 1u32 }.to_le_bytes());
    blob.extend(1.0f64.to_le_bytes());
    blob.extend(2.0f64.to_le_bytes());
    if z {
        blob.extend(3.0f64.to_le_bytes());
    }
    c.execute("INSERT INTO points VALUES (1, ?1)", [&blob]).unwrap();
    c.close().unwrap();
    std::fs::read(path).unwrap()
}

/// Both Shapefile inputs must retain the one attribute row and every XY vertex, through
/// the native reader and the command. A notice over an empty or partial import isn't enough.
fn check_shapefile_xy(ds: &rustgis_engine::data::Dataset, shape_type: Option<u8>) -> std::io::Result<()> {
    use rustgis_engine::data::{Geometry, Value};
    use rustgis_engine::geom::Coord;
    let expected = match shape_type {
        Some(1 | 11 | 21) => Geometry::Point(Coord::new(1.0, 2.0)),
        Some(3 | 13) => Geometry::Line(vec![vec![Coord::new(1.0, 2.0), Coord::new(3.0, 4.0)]]),
        _ => return Err(std::io::Error::other("Shapefile fixture has the wrong geometry type")),
    };
    if ds.features.len() != 1 || ds.features[0].geometry.as_ref() != Some(&expected) || ds.features[0].attrs != vec![Value::Int(1)] {
        return Err(std::io::Error::other("Shapefile fixture didn't retain its XY geometry and ID"));
    }
    Ok(())
}

/// Workbooks retain the point and two attributes except the pinned XLSB short-cell loss,
/// which retains the point but drops every attribute field and value.
fn check_workbook(ds: &rustgis_engine::data::Dataset, short_cells: bool) -> std::io::Result<()> {
    use rustgis_engine::data::{Field, FieldType, Geometry, Value};
    use rustgis_engine::geom::Coord;
    let fields = if short_cells { vec![] } else { vec![Field::new("id", FieldType::Integer), Field::new("note", FieldType::Text)] };
    let attrs = if short_cells { vec![] } else { vec![Value::Int(7), Value::Null] };
    if ds.fields != fields
        || ds.features.len() != 1
        || ds.features[0].geometry != Some(Geometry::Point(Coord::new(1.0, 2.0)))
        || ds.features[0].attrs != attrs
    {
        return Err(std::io::Error::other(format!(
            "spreadsheet fixture has unexpected retained data: fields={:?}, features={:?}",
            ds.fields, ds.features
        )));
    }
    Ok(())
}

/// The height pair keeps the same XY and all ten DXF attributes; only the second Z is lost.
fn check_dxf_height(ds: &rustgis_engine::data::Dataset) -> std::io::Result<()> {
    use rustgis_engine::data::{Geometry, Value};
    use rustgis_engine::geom::Coord;
    let names: Vec<&str> = ds.fields.iter().map(|f| f.name.as_str()).collect();
    if names != ["layer", "entity", "color", "linetype", "text", "height", "rotation", "block", "elevation", "paperspace"]
        || ds.features.len() != 1
        || ds.features[0].geometry != Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]]))
        || ds.features[0].attrs
            != [
                Value::Text("0".into()),
                Value::Text("LINE".into()),
                Value::Int(7),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Float(5.0),
                Value::Bool(false),
            ]
    {
        return Err(std::io::Error::other(format!("DXF height fixture retained unexpected data: {ds:?}")));
    }
    Ok(())
}

/// The PBF variants differ only in the discarded dimension; fractional XY and attributes survive.
fn check_feature_pbf(ds: &rustgis_engine::data::Dataset, lossy: bool) -> std::io::Result<()> {
    use rustgis_engine::data::{Field, FieldType, Geometry, GeometryType, Value};
    use rustgis_engine::geom::Coord;
    if ds.geometry_type != GeometryType::Point
        || ds.fields != [Field::new("id", FieldType::Integer), Field::new("name", FieldType::Text)]
        || ds.features.len() != 1
        || ds.features[0].id != 1
        || ds.features[0].geometry != Some(Geometry::Point(Coord::new(11.5, 17.5)))
        || ds.features[0].attrs != [Value::Int(7), Value::Text("Place".into())]
        || ds.zm_dropped != lossy
    {
        return Err(std::io::Error::other(format!("feature-service PBF retained unexpected data: {ds:?}")));
    }
    Ok(())
}

/// What `layer.addData` did with a file: the notices it gave and whether it added a layer or table,
/// or why it refused the file.
type SaidNotices = Vec<(String, Option<LossCode>)>;
type Reading = Result<(SaidNotices, bool), String>;

/// Writes `file` with `content`, and `beside`, into `dir` (emptied first) and reads it. `Err` when
/// the fixture can't be prepared: a failure of the test, never taken for a refusal. Each file of a
/// case is read from the same `dir`, so notices naming its path are the same for both.
/// A `.gdb` fixture is stored as a ZIP of its table files, extracted and checked with the native
/// reader before the command opens it, so an optional GDAL fallback cannot mask a regression.
fn read(dir: &std::path::Path, file: &str, content: &[u8], beside: &[(&str, Vec<u8>)]) -> std::io::Result<Reading> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    for (name, bytes) in beside {
        std::fs::write(dir.join(name), bytes)?;
    }
    let path = dir.join(file);
    let short_cells = content == include_bytes!("../../testdata/reading-losses/book-short.xlsb");
    if file.ends_with(".gdb") {
        // The archive is fixture storage only: RustGIS opens the extracted folder. Its ZIP
        // importer deliberately refuses FileGDB folders, so do not test that refusal as a loss.
        std::fs::create_dir_all(&path)?;
        for (name, bytes) in rustgis_engine::data::zip::read(content).map_err(std::io::Error::other)? {
            if std::path::Path::new(&name).components().count() != 1
                || !matches!(std::path::Path::new(&name).components().next(), Some(std::path::Component::Normal(_)))
            {
                return Err(std::io::Error::other("a FileGDB fixture member isn't a plain file name"));
            }
            std::fs::write(path.join(name), bytes)?;
        }
        // GDAL can supply the same loss notice if the native reader fails. Treat a native
        // failure as fixture preparation failure, never as an acceptable import refusal.
        let native = rustgis_engine::data::filegdb::read_dir(&path, None)
            .map_err(|e| std::io::Error::other(format!("native FileGDB fixture: {e}")))?;
        if native.layers.len() != 1 || !native.unread.is_empty() {
            return Err(std::io::Error::other(format!("native FileGDB fixture wasn't read completely: {:?}", native.unread)));
        }
    } else {
        std::fs::write(&path, content)?;
    }
    let collection = content == b"wkt,id\n\"GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4))\",1\n"
        || content == include_bytes!("../../testdata/reading-losses/book-collection.xlsx")
        || content == include_bytes!("../../testdata/reading-losses/book-collection.ods")
        || content == include_bytes!("../../testdata/reading-losses/book-collection.xls")
        || content == include_bytes!("../../testdata/reading-losses/book-collection.xlsb");
    let collection_note = "1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept";
    let csv_collection = file.ends_with(".csv") && (collection || content == b"wkt,id\n\"GEOMETRYCOLLECTION(POINT(1 2))\",1\n");
    if csv_collection {
        let (native, report) = rustgis_engine::data::io::read_path(&path).map_err(std::io::Error::other)?;
        if native.features.len() != 1
            || native.features[0].geometry != Some(rustgis_engine::data::Geometry::Point(rustgis_engine::geom::Coord::new(1.0, 2.0)))
            || native.features[0].attrs != [rustgis_engine::data::Value::Int(1)]
            || report.skipped != 0
            || report.messages != if collection { vec![collection_note] } else { vec![] }
        {
            return Err(std::io::Error::other(format!("CSV collection retained data or notice differs: {native:?}, {report:?}")));
        }
    }
    if file.ends_with(".shp") {
        let (native, report) =
            rustgis_engine::data::io::read_path(&path).map_err(|e| std::io::Error::other(format!("native Shapefile fixture: {e}")))?;
        check_shapefile_xy(&native, content.get(32).copied())?;
        let zm = matches!(content.get(32), Some(11 | 13 | 21));
        if native.zm_dropped != zm || report.messages.iter().any(|m| m.contains("Z or M")) != zm {
            return Err(std::io::Error::other(format!("native Shapefile fixture has the wrong loss report: {report:?}")));
        }
        if report.skipped != 0 {
            return Err(std::io::Error::other("native Shapefile fixture skipped a record"));
        }
    }
    if rustgis_engine::data::sheets::is_sheet_name(file) {
        let (native, report) = rustgis_engine::data::sheets::read(content, "a", None, &Default::default())
            .map_err(|e| std::io::Error::other(format!("native spreadsheet fixture: {e}")))?;
        check_workbook(&native, short_cells)?;
        let height = content == include_bytes!("../../testdata/reading-losses/book-z.xlsx")
            || content == include_bytes!("../../testdata/reading-losses/book-z.ods")
            || content == include_bytes!("../../testdata/reading-losses/book-z.xls")
            || content == include_bytes!("../../testdata/reading-losses/book-z.xlsb");
        let errors = content == include_bytes!("../../testdata/reading-losses/book-error.xlsx");
        if collection && report.messages != [collection_note] {
            return Err(std::io::Error::other(format!("native spreadsheet collection notice: {report:?}")));
        }
        if errors && report.messages != ["1 spreadsheet error cell was read as empty, in 'note'"] {
            return Err(std::io::Error::other(format!("native spreadsheet error notice: {report:?}")));
        }
        if report.skipped != 0
            || native.zm_dropped != height
            || if height {
                report.messages.len() != 1 || !report.messages[0].contains("Z or M")
            } else {
                !errors && !collection && !report.messages.is_empty()
            }
        {
            return Err(std::io::Error::other(format!("native spreadsheet fixture has the wrong loss report: {report:?}")));
        }
    }
    let feature_pbf = file == "a.feature.pbf";
    let pbf_lossy = content != include_bytes!("../../testdata/reading-losses/feature-xy.pbf");
    let pbf_messages = if pbf_lossy { vec![rustgis_engine::data::zm_note(Some(1))] } else { vec![] };
    if feature_pbf {
        let (native, report) = rustgis_engine::data::esripbf::read(content, "a").map_err(std::io::Error::other)?;
        check_feature_pbf(&native, pbf_lossy)?;
        if report.skipped != 0 || report.messages != pbf_messages {
            return Err(std::io::Error::other(format!("feature-service PBF has the wrong native report: {report:?}")));
        }
    }
    let dxf_lossy = file == "a.dxf" && content == dxf(&["0 LINE 8 0 10 0 20 0 30 5 11 1 21 1 31 9"]).as_bytes();
    let dxf_height = dxf_lossy || (file == "a.dxf" && content == dxf(&["0 LINE 8 0 10 0 20 0 30 5 11 1 21 1 31 5"]).as_bytes());
    let mut dxf_messages = vec![
        "a DXF drawing doesn't say its coordinate system (give \"crs\", such as EPSG:2263): coordinates are kept as they are".to_string(),
    ];
    if dxf_lossy {
        dxf_messages.push("1 feature lost varying Z coordinates; only the elevation attribute is kept".into());
    }
    if dxf_height {
        let native = rustgis_engine::data::dxf::read(file, content, &Default::default()).map_err(std::io::Error::other)?;
        if native.len() != 1 || native[0].1.skipped != 0 || native[0].1.messages != dxf_messages {
            return Err(std::io::Error::other(format!("DXF height native report: {native:?}")));
        }
        check_dxf_height(&native[0].0)?;
    }
    let mut s = Session::new(Project::default());
    // Layers and standalone tables (what a file without geometry adds).
    let held = |s: &Session| s.map().layers.len() + s.project.tables.len();
    let before = held(&s);
    let mark = s.notice_mark();
    if let Err(e) = s.run("layer.addData", &json!({"path": path})) {
        return Ok(Err(e));
    }
    if file.ends_with(".shp") {
        if s.map().layers.len() != 1 {
            return Err(std::io::Error::other("Shapefile fixture didn't add exactly one layer"));
        }
        check_shapefile_xy(&s.map().layers[0].dataset, content.get(32).copied())?;
    }
    if rustgis_engine::data::sheets::is_sheet_name(file) {
        if s.map().layers.len() != 1 || !s.project.tables.is_empty() {
            return Err(std::io::Error::other("spreadsheet fixture didn't add exactly one layer"));
        }
        check_workbook(&s.map().layers[0].dataset, short_cells)?;
        if (content == include_bytes!("../../testdata/reading-losses/book-clean.xlsx")
            || content == include_bytes!("../../testdata/reading-losses/book-clean.ods")
            || content == include_bytes!("../../testdata/reading-losses/book-clean.xls")
            || content == include_bytes!("../../testdata/reading-losses/book-clean.xlsb"))
            && !s.notices_since(mark).is_empty()
        {
            return Err(std::io::Error::other("clean spreadsheet fixture gave a notice"));
        }
    }
    if collection || csv_collection {
        let notices = s.notices_since(mark);
        if notices.iter().map(|n| n.text.as_str()).collect::<Vec<_>>() != if collection { vec![collection_note] } else { vec![] } {
            return Err(std::io::Error::other(format!("collection command notice: {notices:?}")));
        }
        if csv_collection {
            if s.map().layers.len() != 1 || !s.project.tables.is_empty() {
                return Err(std::io::Error::other("CSV collection didn't add exactly one layer"));
            }
            use rustgis_engine::data::{Geometry, Value};
            use rustgis_engine::geom::Coord;
            let ds = &s.map().layers[0].dataset;
            if ds.features.len() != 1
                || ds.features[0].geometry != Some(Geometry::Point(Coord::new(1.0, 2.0)))
                || ds.features[0].attrs != [Value::Int(1)]
            {
                return Err(std::io::Error::other("CSV command lost retained point or ID"));
            }
        }
    }
    if feature_pbf {
        if s.map().layers.len() != 1 || !s.project.tables.is_empty() {
            return Err(std::io::Error::other("feature-service PBF didn't add exactly one layer"));
        }
        check_feature_pbf(&s.map().layers[0].dataset, pbf_lossy)?;
        let notices = s.notices_since(mark);
        // Require the exact loss count and subject, and no additional notice.
        if notices.iter().map(|n| n.text.clone()).collect::<Vec<_>>() != pbf_messages {
            return Err(std::io::Error::other(format!("feature-service PBF has the wrong command notices: {notices:?}")));
        }
    }
    if dxf_height {
        if s.map().layers.len() != 1 || !s.project.tables.is_empty() {
            return Err(std::io::Error::other("DXF height fixture didn't add exactly one layer"));
        }
        check_dxf_height(&s.map().layers[0].dataset)?;
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        if said != dxf_messages {
            return Err(std::io::Error::other(format!("DXF height command notices: {said:?}")));
        }
    }
    let said = s.notices_since(mark).iter().map(|n| (n.text.clone(), n.loss())).collect();
    Ok(Ok((said, held(&s) > before)))
}

#[test]
fn a_reading_that_loses_something_says_so() {
    let root = std::env::temp_dir().join(format!("rustgis-never-silent-reading-{}", std::process::id()));
    let dir = root.join("case");
    let mut failures = Vec::new();
    for c in cases(&root.join("fixtures")) {
        let name = format!("{} [{}]", c.name, c.file);
        let clean = match read(&dir, c.file, &c.clean, &c.beside[0]) {
            Err(e) => {
                failures.push(format!("{name}: the clean file couldn't be written: {e}"));
                continue;
            }
            Ok(Err(e)) => {
                failures.push(format!("{name}: the clean file was refused: {e}"));
                continue;
            }
            Ok(Ok((said, false))) => {
                failures.push(format!("{name}: the clean file added no layer; said: {said:?}"));
                continue;
            }
            Ok(Ok((said, true))) => said,
        };
        // What the lossy file says that the clean one doesn't, and whether it added a layer.
        let (new, added): (SaidNotices, bool) = match read(&dir, c.file, &c.lossy, &c.beside[1]) {
            Err(e) => {
                failures.push(format!("{name}: the lossy file couldn't be written: {e}"));
                continue;
            }
            // A refusal says why, but a crash caught by `Session::run` is a bug.
            Ok(Err(e)) if e.contains("failed unexpectedly") || e.contains("stopped unexpectedly") => {
                failures.push(format!("{name}: reading crashed: {e}"));
                continue;
            }
            Ok(Err(e)) => (vec![(e, None)], false),
            Ok(Ok((said, added))) => (said.into_iter().filter(|n| !clean.contains(n)).collect(), added),
        };
        if let Expect::Classified(code) = c.expect {
            if !added
                || new.iter().filter(|(_, loss)| *loss == Some(code)).count() != 1
                || clean.iter().any(|(_, loss)| *loss == Some(code))
            {
                failures.push(format!("{name}: expected one {code:?} only for the lossy import; clean: {clean:?}, new: {new:?}"));
            }
            continue;
        }
        match c.expect {
            // Both ways (AGENTS.md, Never silent): the clean file doesn't say the loss.
            Said(words) | Imported(words) if clean.iter().any(|(text, _)| text.contains(words)) => {
                failures.push(format!("{name}: the clean file says the loss it doesn't have ({words:?}): {clean:?}"))
            }
            Imported(_) if !added => {
                failures.push(format!("{name}: the valid lossy fixture was refused instead of imported: {new:?}"))
            }
            Said(words) | Imported(words) if !new.iter().any(|(text, _)| text.contains(words)) => {
                failures.push(format!("{name}: not said: nothing new holds {words:?}; new: {new:?}; the clean file said {clean:?}"))
            }
            Silent(lost) if !new.is_empty() => failures.push(format!(
                "{name}: a new notice ({new:?}): if it says that {lost}, make the case Said; if not, change the lossy file so it gives only that loss"
            )),
            Silent(lost) if !added => {
                failures.push(format!("{name}: the lossy file added no layer and said nothing, more than this case's loss ({lost})"))
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{} of the cases failed:\n{}", failures.len(), failures.join("\n"));
}
