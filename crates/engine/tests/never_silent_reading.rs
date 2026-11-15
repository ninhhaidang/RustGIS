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
//! Text formats only so far, without CityJSON and CityGML; those and the binary ones (Shapefile,
//! GeoPackage, FlatGeobuf, GeoParquet, OSM PBF, KMZ, spreadsheets) are still to add (ROADMAP.md).
use rustgis_engine::{Project, Session};
use serde_json::json;

/// What reading a case's lossy file does.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// Says the loss: a notice the clean file doesn't give, or a refusal, holding these words.
    Said(&'static str),
    /// Loses it without a notice today, still adding a layer: what is lost.
    Silent(&'static str),
}
use Expect::{Said, Silent};

/// A clean file and the same file with a loss in it.
struct Case {
    /// The reader and the loss, to name a failure.
    name: &'static str,
    /// The file name, whose extension picks the reader.
    file: &'static str,
    clean: String,
    lossy: String,
    /// Files beside it, the same in both (a MapInfo `.mid`).
    beside: Vec<(&'static str, String)>,
    expect: Expect,
}

fn case(name: &'static str, file: &'static str, clean: impl Into<String>, lossy: impl Into<String>, expect: Expect) -> Case {
    Case { name, file, clean: clean.into(), lossy: lossy.into(), beside: Vec::new(), expect }
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

fn cases() -> Vec<Case> {
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
            Silent("only the collection's first member is kept"),
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
            Said("hole in a polygon was unreadable"),
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
            Silent("one elevation per entity is kept, the first vertex's"),
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
    ];

    // MapInfo: the .mid beside each .mif holds one row per object.
    let mut mapinfo = |name, clean: &str, lossy: &str, mid: &str, expect| {
        let mut c = case(name, "a.mif", mif(clean), mif(lossy), expect);
        c.beside.push(("a.mid", mid.to_string()));
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
    out
}

/// What `layer.addData` did with a file: the notices it gave and whether it added a layer or table,
/// or why it refused the file.
type Reading = Result<(Vec<String>, bool), String>;

/// Writes `file` with `content`, and `beside`, into `dir` (emptied first) and reads it. `Err` when
/// the files can't be written: a failure of the test, never taken for a refusal. Each file of a
/// case is read from the same `dir`, so notices naming its path are the same for both.
fn read(dir: &std::path::Path, file: &str, content: &str, beside: &[(&str, String)]) -> std::io::Result<Reading> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    for (name, text) in beside {
        std::fs::write(dir.join(name), text)?;
    }
    let path = dir.join(file);
    std::fs::write(&path, content)?;
    let mut s = Session::new(Project::default());
    // Layers and standalone tables (what a file without geometry adds).
    let held = |s: &Session| s.map().layers.len() + s.project.tables.len();
    let before = held(&s);
    let mark = s.notice_mark();
    if let Err(e) = s.run("layer.addData", &json!({"path": path})) {
        return Ok(Err(e));
    }
    let said = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    Ok(Ok((said, held(&s) > before)))
}

#[test]
fn a_reading_that_loses_something_says_so() {
    let root = std::env::temp_dir().join(format!("rustgis-never-silent-reading-{}", std::process::id()));
    let dir = root.join("case");
    let mut failures = Vec::new();
    for c in cases() {
        let clean = match read(&dir, c.file, &c.clean, &c.beside) {
            Err(e) => {
                failures.push(format!("{}: the clean file couldn't be written: {e}", c.name));
                continue;
            }
            Ok(Err(e)) => {
                failures.push(format!("{}: the clean file was refused: {e}", c.name));
                continue;
            }
            Ok(Ok((said, false))) => {
                failures.push(format!("{}: the clean file added no layer; said: {said:?}", c.name));
                continue;
            }
            Ok(Ok((said, true))) => said,
        };
        // What the lossy file says that the clean one doesn't, and whether it added a layer.
        let (new, added): (Vec<String>, bool) = match read(&dir, c.file, &c.lossy, &c.beside) {
            Err(e) => {
                failures.push(format!("{}: the lossy file couldn't be written: {e}", c.name));
                continue;
            }
            // A refusal says why, but a crash caught by `Session::run` is a bug.
            Ok(Err(e)) if e.contains("failed unexpectedly") || e.contains("stopped unexpectedly") => {
                failures.push(format!("{}: reading crashed: {e}", c.name));
                continue;
            }
            Ok(Err(e)) => (vec![e], false),
            Ok(Ok((said, added))) => (said.into_iter().filter(|n| !clean.contains(n)).collect(), added),
        };
        match c.expect {
            // Both ways (AGENTS.md, Never silent): the clean file doesn't say the loss.
            Said(words) if clean.iter().any(|n| n.contains(words)) => {
                failures.push(format!("{}: the clean file says the loss it doesn't have ({words:?}): {clean:?}", c.name))
            }
            Said(words) if !new.iter().any(|n| n.contains(words)) => {
                failures.push(format!("{}: not said: nothing new holds {words:?}; new: {new:?}; the clean file said {clean:?}", c.name))
            }
            Silent(lost) if !new.is_empty() => failures.push(format!(
                "{}: a new notice ({new:?}): if it says that {lost}, make the case Said; if not, change the lossy file so it gives only that loss",
                c.name
            )),
            Silent(lost) if !added => {
                failures.push(format!("{}: the lossy file added no layer and said nothing, more than this case's loss ({lost})", c.name))
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{} of the cases failed:\n{}", failures.len(), failures.join("\n"));
}
