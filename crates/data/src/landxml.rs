//! LandXML: survey and civil design files — coordinate geometry points (`CgPoint`), surfaces (TIN
//! faces), parcels and alignments, as points, polygons and lines layers.
//!
//! Coordinates are northing, easting (and elevation), usually in a projected system: the file's
//! `CoordinateSystem epsgCode`, or the one given, converts them to longitude/latitude; without
//! either they're kept as they are and the report says so. Curves and spirals are drawn as chords
//! between their points.
//!
//! The file is hostile: no DTD is read, the node count is capped, face and point counts are
//! bounded, and anything that can't be read is skipped and counted.

use std::collections::HashMap;

use roxmltree::{Node, ParsingOptions};
use rustgis_geom::Coord;

use crate::raster::Crs;
use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// LandXML file types (often plain `.xml`, read when its contents say so).
pub const EXTENSIONS: &[&str] = &["landxml"];

/// The most TIN faces read from one surface.
const MAX_FACES: usize = 2_000_000;

pub fn is_landxml_name(name: &str) -> bool {
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// Whether XML text looks like LandXML.
pub fn looks_like_landxml(text: &str) -> bool {
    crate::io::cut(text, 4096).contains("<LandXML")
}

fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn elements<'a, 'i>(n: &Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.descendants().filter(move |c| c.is_element() && local(c) == name)
}

fn text_of(n: &Node) -> String {
    n.children().filter(|t| t.is_text()).filter_map(|t| t.text()).collect::<String>()
}

/// `northing easting [elevation]` as a coordinate (x = easting) and the elevation.
fn nez(s: &str) -> Option<(Coord, Option<f64>)> {
    let v: Vec<f64> = s.split_whitespace().map(|t| t.parse().ok()).collect::<Option<_>>()?;
    match v.as_slice() {
        [n, e] if n.is_finite() && e.is_finite() => Some((Coord::new(*e, *n), None)),
        [n, e, z, ..] if n.is_finite() && e.is_finite() => Some((Coord::new(*e, *n), z.is_finite().then_some(*z))),
        _ => None,
    }
}

/// Reads a LandXML file into its layers (those with features). `crs` is the coordinate system of
/// a file that doesn't say.
pub fn read(file_name: &str, bytes: &[u8], crs: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    let name = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("landxml").to_string();
    let text = std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = crate::xml::parse(text.trim_start_matches('\u{feff}'), opts).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    let root = doc.root_element();
    if local(&root) != "LandXML" {
        return Err(DataError::Format(format!("{file_name}: not a LandXML file")));
    }
    let mut report = ReadReport::default();
    let file_crs = elements(&root, "CoordinateSystem").find_map(|c| c.attribute("epsgCode")?.trim().parse::<u32>().ok());
    let crs = crs.or_else(|| file_crs.and_then(Crs::from_epsg));
    match (crs, file_crs) {
        (Some(_), _) => {}
        (None, Some(code)) => report.messages.push(format!(
            "the file's coordinate system, EPSG:{code}, isn't one RustGIS converts (give \"crs\" as a WKT or another code): coordinates are kept as they are"
        )),
        (None, None) => report
            .messages
            .push("the file doesn't say its coordinate system (give \"crs\", such as EPSG:2263): coordinates are kept as they are"),
    }
    let mut skipped = 0usize;
    let mut missing_parcels = 0usize;

    // Coordinate geometry points, by name too (parcels and alignments may refer to them).
    let mut points = Dataset::new(
        format!("{name} points"),
        GeometryType::Point,
        vec![
            Field::new("name", FieldType::Text),
            Field::new("code", FieldType::Text),
            Field::new("desc", FieldType::Text),
            Field::new("elevation", FieldType::Double),
        ],
    );
    let mut named: HashMap<String, Coord> = HashMap::new();
    for p in elements(&root, "CgPoint") {
        let Some((c, z)) = nez(&text_of(&p)) else {
            skipped += 1;
            continue;
        };
        let attr = |k: &str| p.attribute(k).map(|v| Value::Text(v.to_string())).unwrap_or_default();
        if let Some(n) = p.attribute("name") {
            named.insert(n.to_string(), c);
        }
        points.push_next(Some(Geometry::Point(c)), vec![attr("name"), attr("code"), attr("desc"), z.map(Value::Float).unwrap_or_default()]);
    }
    // A point of a parcel or alignment: inline text, or a `pntRef` to a CgPoint.
    let pt = |n: &Node| -> Option<Coord> {
        let t = text_of(n);
        if !t.trim().is_empty() {
            return nez(&t).map(|(c, _)| c);
        }
        named.get(n.attribute("pntRef")?).copied()
    };
    // The points along a CoordGeom: each Line, Curve or Spiral's start (and end).
    let path = |cg: &Node, skipped: &mut usize| -> Vec<Coord> {
        let mut pts: Vec<Coord> = Vec::new();
        for seg in cg.children().filter(|c| c.is_element() && matches!(local(c), "Line" | "Curve" | "Spiral" | "IrregularLine")) {
            // A curve's PI (where its tangents meet) is off the curve: its chord runs Start to End.
            for k in ["Start", "End"] {
                if let Some(n) = seg.children().find(|c| c.is_element() && local(c) == k) {
                    match pt(&n) {
                        Some(c) if pts.last() != Some(&c) => pts.push(c),
                        Some(_) => {}
                        None => *skipped += 1,
                    }
                }
            }
        }
        pts
    };

    let mut parcels = Dataset::new(
        format!("{name} parcels"),
        GeometryType::Polygon,
        vec![Field::new("name", FieldType::Text), Field::new("area", FieldType::Double), Field::new("desc", FieldType::Text)],
    );
    for p in elements(&root, "Parcel") {
        let Some(cg) = p.children().find(|c| c.is_element() && local(c) == "CoordGeom") else {
            missing_parcels += 1;
            continue;
        };
        let mut ring = path(&cg, &mut skipped);
        if ring.first() != ring.last()
            && let Some(f) = ring.first().copied()
        {
            ring.push(f);
        }
        if ring.len() < 4 {
            skipped += 1;
            continue;
        }
        let area = p.attribute("area").and_then(|a| a.parse::<f64>().ok()).map(Value::Float).unwrap_or_default();
        let attr = |k: &str| p.attribute(k).map(|v| Value::Text(v.to_string())).unwrap_or_default();
        parcels.push_next(Some(Geometry::Polygon(vec![vec![ring]])), vec![attr("name"), area, attr("desc")]);
    }

    let mut alignments = Dataset::new(
        format!("{name} alignments"),
        GeometryType::Polyline,
        vec![Field::new("name", FieldType::Text), Field::new("length", FieldType::Double), Field::new("staStart", FieldType::Double)],
    );
    for a in elements(&root, "Alignment") {
        let Some(cg) = a.children().find(|c| c.is_element() && local(c) == "CoordGeom") else { continue };
        let line = path(&cg, &mut skipped);
        if line.len() < 2 {
            skipped += 1;
            continue;
        }
        let num = |k: &str| a.attribute(k).and_then(|v| v.parse::<f64>().ok()).map(Value::Float).unwrap_or_default();
        alignments.push_next(
            Some(Geometry::Line(vec![line])),
            vec![a.attribute("name").map(|v| Value::Text(v.to_string())).unwrap_or_default(), num("length"), num("staStart")],
        );
    }

    // Surfaces: each TIN face a triangle, with its surface and mean elevation.
    let mut faces = Dataset::new(
        format!("{name} surfaces"),
        GeometryType::Polygon,
        vec![Field::new("surface", FieldType::Text), Field::new("elevation", FieldType::Double)],
    );
    for s in elements(&root, "Surface") {
        let sname = s.attribute("name").unwrap_or("").to_string();
        let mut pnts: HashMap<&str, (Coord, Option<f64>)> = HashMap::new();
        for p in elements(&s, "P") {
            match (p.attribute("id"), nez(&text_of(&p))) {
                (Some(id), Some(v)) => {
                    pnts.insert(id, v);
                }
                _ => skipped += 1,
            }
        }
        for (i, f) in elements(&s, "F").enumerate() {
            if i >= MAX_FACES {
                report.messages.push(format!("surface '{sname}' has more than {MAX_FACES} faces: the rest are left out"));
                break;
            }
            // Invisible faces (i="1") are holes in the surface.
            if f.attribute("i") == Some("1") {
                continue;
            }
            let tri: Option<Vec<(Coord, Option<f64>)>> = text_of(&f).split_whitespace().map(|id| pnts.get(id).copied()).collect();
            match tri.as_deref() {
                Some([a, b, c]) => {
                    let zs: Vec<f64> = [a.1, b.1, c.1].into_iter().flatten().collect();
                    let z = (zs.len() == 3).then(|| zs.iter().sum::<f64>() / 3.0);
                    faces.push_next(
                        Some(Geometry::Polygon(vec![vec![vec![a.0, b.0, c.0, a.0]]])),
                        vec![Value::Text(sname.clone()), z.map(Value::Float).unwrap_or_default()],
                    );
                }
                _ => skipped += 1,
            }
        }
    }

    if missing_parcels > 0 {
        let (parcels, verb) = if missing_parcels == 1 { ("parcel", "was") } else { ("parcels", "were") };
        report.messages.push(format!("{missing_parcels} LandXML {parcels} without CoordGeom {verb} left out"));
    }
    report.skipped = skipped + missing_parcels;
    if skipped > 0 {
        report.messages.push(format!("{skipped} points, faces or figures couldn't be read and were left out"));
    }
    let mut out: Vec<(Dataset, ReadReport)> = Vec::new();
    let transformer = match crs.filter(|c| *c != Crs::LonLat) {
        Some(c) => Some((c.transformer().map_err(DataError::Format)?, c)),
        None => None,
    };
    for mut ds in [points, parcels, alignments, faces] {
        if ds.features.is_empty() {
            continue;
        }
        // What the file as a whole said goes with its first layer only.
        let mut rep = if out.is_empty() { report.clone() } else { ReadReport::default() };
        if let Some((t, c)) = &transformer {
            // Kept as the file holds them, in its system, where they have a place in
            // longitude/latitude.
            ds.held_in = *c;
            let mut lost = 0usize;
            for f in &mut ds.features {
                if let Some(g) = f.geometry.take() {
                    if crate::io::places_in_lonlat(&g, t) {
                        f.geometry = Some(g);
                    } else {
                        lost += 1;
                    }
                }
            }
            if lost > 0 {
                rep.messages.push(format!("{lost} features couldn't be converted from {}", c.label()));
            }
        }
        out.push((ds, rep));
    }
    if out.is_empty() {
        return Err(DataError::Format(format!("{file_name}: no points, parcels, alignments or surfaces")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_loss_parcels_without_coordgeom_are_counted() {
        let xml = br#"<LandXML><CoordinateSystem epsgCode="4326"/><CgPoints><CgPoint name="kept">1 2</CgPoint></CgPoints><Parcels><Parcel name="lost"/><Parcel name="also lost"/></Parcels></LandXML>"#;
        let layers = read("a.xml", xml, None).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].0.features[0].geometry, Some(Geometry::Point(Coord::new(2.0, 1.0))));
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("kept".into()));
        assert_eq!(layers[0].1.messages, ["2 LandXML parcels without CoordGeom were left out"]);
        let clean = br#"<LandXML><CoordinateSystem epsgCode="4326"/><CgPoints><CgPoint name="kept">1 2</CgPoint></CgPoints></LandXML>"#;
        assert!(read("a.xml", clean, None).unwrap()[0].1.messages.is_empty());
    }

    const LANDXML: &str = r#"<?xml version="1.0"?>
<LandXML xmlns="http://www.landxml.org/schema/LandXML-1.2" version="1.2">
<CoordinateSystem epsgCode="32633"/>
<CgPoints>
  <CgPoint name="1" code="IP">4649776 500000 100.5</CgPoint>
  <CgPoint name="2">4649776 500100 101</CgPoint>
  <CgPoint name="3">4649876 500100</CgPoint>
  <CgPoint name="bad">x y</CgPoint>
</CgPoints>
<Parcels><Parcel name="Lot 1" area="10000"><CoordGeom>
  <Line><Start pntRef="1"/><End pntRef="2"/></Line>
  <Line><Start pntRef="2"/><End pntRef="3"/></Line>
  <Line><Start pntRef="3"/><End>4649876 500000</End></Line>
</CoordGeom></Parcel></Parcels>
<Alignments><Alignment name="Main St" length="200" staStart="0"><CoordGeom>
  <Line><Start>4649700 499900</Start><End>4649700 500000</End></Line>
  <Curve rot="cw"><Start>4649700 500000</Start><Center>4649650 500000</Center><End>4649650 500050</End><PI>4649700 500050</PI></Curve>
</CoordGeom></Alignment></Alignments>
<Surfaces><Surface name="Ground"><Definition surfType="TIN"><Pnts>
  <P id="1">4649776 500000 100</P><P id="2">4649776 500100 102</P><P id="3">4649876 500100 104</P><P id="4">4649876 500000 106</P>
</Pnts><Faces><F>1 2 3</F><F>1 3 4</F><F i="1">1 2 4</F><F>1 2 9</F></Faces></Definition></Surface></Surfaces>
</LandXML>"#;

    #[test]
    fn reads_landxml() {
        assert!(looks_like_landxml(LANDXML));
        let layers = read("site.xml", LANDXML.as_bytes(), None).unwrap();
        let names: Vec<&str> = layers.iter().map(|(d, _)| d.name.as_str()).collect();
        assert_eq!(names, ["site points", "site parcels", "site alignments", "site surfaces"]);
        let pts = &layers[0].0;
        assert_eq!(pts.features.len(), 3);
        // Held in its own system; placed in longitude/latitude.
        let pts = pts.in_lonlat().unwrap().into_owned();
        let Some(Geometry::Point(c)) = &pts.features[0].geometry else { panic!() };
        assert!((c.x - 15.0).abs() < 1e-6 && (c.y - 42.0).abs() < 0.01, "UTM 33N from the file: {c:?}");
        assert_eq!(pts.features[0].attrs[3], Value::Float(100.5));
        let parcel = &layers[1].0.features[0];
        assert!(matches!(&parcel.geometry, Some(Geometry::Polygon(p)) if p[0][0].len() == 5));
        assert!(
            matches!(&layers[2].0.features[0].geometry, Some(Geometry::Line(l)) if l[0].len() == 3),
            "the curve is a chord, without its PI"
        );
        // A file-wide message isn't repeated on every layer.
        assert_eq!(layers.iter().filter(|(_, r)| r.skipped > 0).count(), 1);
        let faces = &layers[3].0;
        assert_eq!(faces.features.len(), 2, "the invisible face is a hole; the face naming a missing point is skipped");
        assert_eq!(faces.features[0].attrs[1], Value::Float(102.0));
        assert!(layers[0].1.skipped >= 2);
        assert!(looks_like_landxml(&format!("<LandXML>{}é", " ".repeat(4087))), "a head cut inside a character");
        // A given system wins; none at all keeps the coordinates.
        let plain = LANDXML.replace("<CoordinateSystem epsgCode=\"32633\"/>", "");
        let layers = read("site.xml", plain.as_bytes(), None).unwrap();
        let Some(Geometry::Point(c)) = &layers[0].0.features[0].geometry else { panic!() };
        assert_eq!((c.x, c.y), (500000.0, 4649776.0));
        assert!(layers[0].1.messages.iter().any(|m| m.contains("\"crs\"")));
        let layers = read("site.xml", plain.as_bytes(), Crs::from_epsg(32633)).unwrap();
        let Some(Geometry::Point(c)) = &layers[0].0.in_lonlat().unwrap().features[0].geometry else { panic!() };
        assert!((c.x - 15.0).abs() < 1e-6);
    }

    #[test]
    fn junk_is_refused() {
        assert!(read("x.xml", b"<other/>", None).is_err());
        assert!(read("x.xml", b"<LandXML></LandXML>", None).is_err(), "nothing in it");
        for cut in (0..LANDXML.len()).step_by(97) {
            let _ = read("x.xml", LANDXML.as_bytes().get(..cut).unwrap_or_default(), None);
        }
    }
}
