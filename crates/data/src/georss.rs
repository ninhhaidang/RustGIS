//! GeoRSS: RSS 2.0, RSS 1.0 and Atom feeds whose items carry a place — GeoRSS Simple
//! (`georss:point`, `line`, `polygon`, `box`), GeoRSS GML (`georss:where` with a `gml:Point`,
//! `LineString`, `Polygon` or `Envelope`) or W3C Basic Geo (`geo:lat`/`geo:long`). Items become
//! points, lines and polygons layers with their title, link, description, date and id.
//!
//! The file is hostile: no DTD is read, the node count is capped, and items whose coordinates
//! can't be read are skipped and counted; unreadable polygon holes are omitted and counted.

use roxmltree::{Node, ParsingOptions};
use rustgis_geom::Coord;

use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// GeoRSS file types (feeds are often plain `.xml`, which is read when its contents say so).
pub const EXTENSIONS: &[&str] = &["georss", "rss", "atom"];

/// Whether XML text looks like a feed with places in it.
pub fn looks_like_georss(text: &str) -> bool {
    let head = crate::io::cut(text, 8192);
    (head.contains("<rss") || head.contains("<feed") || head.contains("<rdf:RDF"))
        && (head.contains("georss") || head.contains("w3.org/2003/01/geo"))
}

fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn child<'a, 'i>(n: &Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && local(c) == name)
}

fn text_of(n: &Node) -> String {
    n.descendants().filter(|t| t.is_text()).filter_map(|t| t.text()).collect::<String>().trim().to_string()
}

fn child_text(n: &Node, name: &str) -> Option<String> {
    let t = text_of(&child(n, name)?);
    (!t.is_empty()).then_some(t)
}

/// `lat lon lat lon …` (GeoRSS's order) as coordinates; `None` when any is out of range or the
/// count is odd.
fn lat_lons(s: &str) -> Option<Vec<Coord>> {
    let v: Vec<f64> =
        s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).map(|t| t.parse().ok()).collect::<Option<_>>()?;
    let (pairs, rest) = v.as_chunks::<2>();
    if pairs.is_empty() || !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[lat, lon]| ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then(|| Coord::new(lon, lat)))
        .collect()
}

fn ring(mut pts: Vec<Coord>) -> Option<Vec<Coord>> {
    if pts.first() != pts.last() {
        pts.push(*pts.first()?);
    }
    (pts.len() >= 4).then_some(pts)
}

fn bbox(ll: Coord, ur: Coord) -> Option<Geometry> {
    (ll.x < ur.x && ll.y < ur.y).then(|| Geometry::Polygon(vec![vec![vec![ll, Coord::new(ur.x, ll.y), ur, Coord::new(ll.x, ur.y), ll]]]))
}

/// An item's place, `Ok(None)` when it has none, `Err` when it has one that can't be read.
/// Unreadable polygon holes and coordinate pairs past the first in points are counted where omitted.
fn place(item: &Node, lost_holes: &mut usize, excess_points: &mut usize) -> std::result::Result<Option<Geometry>, ()> {
    let g = |name: &str| item.children().find(|c| c.is_element() && local(c) == name);
    let parse = |n: Node| lat_lons(&text_of(&n)).ok_or(());
    let mut point = |pts: Vec<Coord>| {
        *excess_points += pts.len().saturating_sub(1);
        pts.first().map(|c| Some(Geometry::Point(*c))).ok_or(())
    };
    if let Some(n) = g("point") {
        return point(parse(n)?);
    }
    if let Some(n) = g("line") {
        let pts = parse(n)?;
        return if pts.len() >= 2 { Ok(Some(Geometry::Line(vec![pts]))) } else { Err(()) };
    }
    if let Some(n) = g("polygon") {
        return ring(parse(n)?).map(|r| Some(Geometry::Polygon(vec![vec![r]]))).ok_or(());
    }
    if let Some(n) = g("box") {
        return match parse(n)?.as_slice() {
            [ll, ur] => bbox(*ll, *ur).map(Some).ok_or(()),
            _ => Err(()),
        };
    }
    if let Some(w) = g("where") {
        // GML inside: the first geometry element.
        let Some(gml) = w.descendants().find(|d| d.is_element() && matches!(local(d), "Point" | "LineString" | "Polygon" | "Envelope"))
        else {
            return Err(());
        };
        let pos = |name: &str| gml.descendants().find(|d| d.is_element() && local(d) == name).ok_or(());
        return match local(&gml) {
            "Point" => point(parse(pos("pos")?)?),
            "LineString" => {
                let pts = parse(pos("posList")?)?;
                if pts.len() >= 2 { Ok(Some(Geometry::Line(vec![pts]))) } else { Err(()) }
            }
            "Polygon" => {
                let ext = gml.descendants().find(|d| d.is_element() && local(d) == "exterior").ok_or(())?;
                let outer = ring(parse(ext.descendants().find(|d| d.is_element() && local(d) == "posList").ok_or(())?)?).ok_or(())?;
                let mut rings = vec![outer];
                for i in gml.descendants().filter(|d| d.is_element() && local(d) == "interior") {
                    if let Some(r) = i
                        .descendants()
                        .find(|d| d.is_element() && local(d) == "posList")
                        .and_then(|p| lat_lons(&text_of(&p)))
                        .and_then(ring)
                    {
                        rings.push(r);
                    } else {
                        *lost_holes += 1;
                    }
                }
                Ok(Some(Geometry::Polygon(vec![rings])))
            }
            _ => {
                let ll = parse(pos("lowerCorner")?)?;
                let ur = parse(pos("upperCorner")?)?;
                match (ll.first(), ur.first()) {
                    (Some(a), Some(b)) => bbox(*a, *b).map(Some).ok_or(()),
                    _ => Err(()),
                }
            }
        };
    }
    // W3C Basic Geo: geo:lat and geo:long, on the item or in a geo:Point.
    let holder = g("Point").unwrap_or(*item);
    match (child_text(&holder, "lat"), child_text(&holder, "long").or_else(|| child_text(&holder, "lon"))) {
        (Some(lat), Some(lon)) => point(lat_lons(&format!("{lat} {lon}")).ok_or(())?),
        (None, None) => Ok(None),
        _ => Err(()),
    }
}

const FIELDS: [&str; 6] = ["title", "link", "description", "date", "id", "category"];

/// An item's attributes, from RSS or Atom elements.
fn attrs(item: &Node) -> Vec<Value> {
    let link =
        child(item, "link").and_then(|l| l.attribute("href").map(str::to_string).or_else(|| Some(text_of(&l)).filter(|t| !t.is_empty())));
    let desc = child_text(item, "description").or_else(|| child_text(item, "summary")).or_else(|| child_text(item, "content"));
    let date = ["pubDate", "updated", "published", "date"].iter().find_map(|k| child_text(item, k));
    let id = child_text(item, "guid").or_else(|| child_text(item, "id"));
    let category = child(item, "category")
        .and_then(|c| c.attribute("term").map(str::to_string).or_else(|| Some(text_of(&c)).filter(|t| !t.is_empty())));
    [child_text(item, "title"), link, desc, date, id, category].into_iter().map(|v| v.map(Value::Text).unwrap_or_default()).collect()
}

/// Reads a GeoRSS feed into its points, lines and polygons layers (those with items).
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Vec<(Dataset, ReadReport)>> {
    let name = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("feed").to_string();
    let text = std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = crate::xml::parse(text.trim_start_matches('\u{feff}'), opts).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    let root = doc.root_element();
    if !matches!(local(&root), "rss" | "feed" | "RDF") {
        return Err(DataError::Format(format!("{file_name}: not an RSS or Atom feed")));
    }
    let fields = || FIELDS.iter().map(|f| Field::new(*f, FieldType::Text)).collect::<Vec<_>>();
    let mut layers = [
        Dataset::new(format!("{name} points"), GeometryType::Point, fields()),
        Dataset::new(format!("{name} lines"), GeometryType::Polyline, fields()),
        Dataset::new(format!("{name} polygons"), GeometryType::Polygon, fields()),
    ];
    let mut excess_points = 0usize;
    let (mut skipped, mut without, mut lost_holes) = (0usize, 0usize, 0usize);
    for item in root.descendants().filter(|n| n.is_element() && matches!(local(n), "item" | "entry")) {
        match place(&item, &mut lost_holes, &mut excess_points) {
            Ok(Some(g)) => {
                let i = match g.kind() {
                    GeometryType::Polyline => 1,
                    GeometryType::Polygon => 2,
                    _ => 0,
                };
                if let Some(ds) = layers.get_mut(i) {
                    ds.push_next(Some(g), attrs(&item));
                }
            }
            Ok(None) => without += 1,
            Err(()) => skipped += 1,
        }
    }
    let mut report = ReadReport { skipped, ..ReadReport::default() };
    if excess_points > 0 {
        let (pairs, verb) = if excess_points == 1 { ("pair", "was") } else { ("pairs", "were") };
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedGeometry,
            format!("{excess_points} excess GeoRSS Point coordinate {pairs} {verb} left out"),
        ));
    }
    if skipped > 0 {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{skipped} items had places that couldn't be read"),
        ));
    }
    if without > 0 {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{without} items have no place and were left out"),
        ));
    }
    if lost_holes > 0 {
        let what = if lost_holes == 1 {
            "1 hole in a polygon was unreadable and left out".to_string()
        } else {
            format!("{lost_holes} holes in polygons were unreadable and left out")
        };
        report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedRing, what));
    }
    // What the file as a whole said goes with its first layer only.
    let out: Vec<(Dataset, ReadReport)> = layers
        .into_iter()
        .filter(|d| !d.features.is_empty())
        .enumerate()
        .map(|(i, d)| (d, if i == 0 { report.clone() } else { ReadReport::default() }))
        .collect();
    if out.is_empty() {
        return Err(DataError::Format(format!("{file_name}: no items with a place")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_loss_excess_point_pairs_are_counted() {
        for place in [
            "<georss:point>1 2 3 4 5 6</georss:point>",
            "<georss:where><gml:Point><gml:pos>1 2 3 4 5 6</gml:pos></gml:Point></georss:where>",
        ] {
            let xml = format!(
                r#"<rss xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml"><channel><item><title>kept</title>{place}</item></channel></rss>"#
            );
            let layers = read("a.rss", xml.as_bytes()).unwrap();
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].0.features[0].geometry, Some(Geometry::Point(Coord::new(2.0, 1.0))));
            assert_eq!(layers[0].0.value(0, 0), &Value::Text("kept".into()));
            assert_eq!(layers[0].1.messages.texts().collect::<Vec<_>>(), ["2 excess GeoRSS Point coordinate pairs were left out"]);
            let clean = xml.replace("1 2 3 4 5 6", "1 2");
            assert!(read("a.rss", clean.as_bytes()).unwrap()[0].1.messages.is_empty());
        }
    }

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml" xmlns:geo="http://www.w3.org/2003/01/geo/wgs84_pos#">
<channel><title>Quakes</title>
<item><title>M 4.5</title><link>https://example.com/1</link><pubDate>Mon, 12 Oct 2026 10:00:00 GMT</pubDate><georss:point>45.25 -110.5</georss:point></item>
<item><title>Road</title><georss:line>45 -110 46 -111 47 -111</georss:line></item>
<item><title>Area</title><georss:polygon>45 -110 46 -110 46 -111 45 -110</georss:polygon></item>
<item><title>Box</title><georss:box>42 -71 43 -69</georss:box></item>
<item><title>GML</title><georss:where><gml:Point><gml:pos>10 20</gml:pos></gml:Point></georss:where></item>
<item><title>W3C</title><geo:lat>-33.9</geo:lat><geo:long>151.2</geo:long></item>
<item><title>Bad</title><georss:point>95 0</georss:point></item>
<item><title>None</title></item>
</channel></rss>"#;

    #[test]
    fn reads_rss() {
        assert!(looks_like_georss(RSS));
        let layers = read("q.rss", RSS.as_bytes()).unwrap();
        let names: Vec<&str> = layers.iter().map(|(d, _)| d.name.as_str()).collect();
        assert_eq!(names, ["q points", "q lines", "q polygons"]);
        let points = &layers[0].0;
        assert_eq!(points.features.len(), 3);
        assert_eq!(points.features[0].geometry, Some(Geometry::Point(Coord::new(-110.5, 45.25))));
        assert_eq!(points.features[0].attrs[1], Value::Text("https://example.com/1".into()));
        assert_eq!(points.features[0].attrs[3], Value::Text("Mon, 12 Oct 2026 10:00:00 GMT".into()));
        assert_eq!(layers[2].0.features.len(), 2, "polygon and box");
        let rep = &layers[0].1;
        assert_eq!(rep.skipped, 1);
        assert!(rep.messages.iter().any(|m| m.contains("1 items have no place")));
    }

    #[test]
    fn reads_atom_with_gml() {
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml">
<entry><title>Lake</title><link href="https://x.org/lake"/><updated>2026-10-01</updated><id>urn:1</id>
<georss:where><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>45 -110 46 -110 46 -111 45 -110</gml:posList></gml:LinearRing></gml:exterior>
<gml:interior><gml:LinearRing><gml:posList>45.4 -110.4 45.6 -110.4 45.6 -110.6 45.4 -110.4</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></georss:where></entry>
<entry><title>Env</title><georss:where><gml:Envelope><gml:lowerCorner>42 -71</gml:lowerCorner><gml:upperCorner>43 -69</gml:upperCorner></gml:Envelope></georss:where></entry>
<entry><title>Trail</title><georss:where><gml:LineString><gml:posList>45 -110 46 -111</gml:posList></gml:LineString></georss:where></entry>
</feed>"#;
        let layers = read("a.atom", atom.as_bytes()).unwrap();
        assert_eq!(layers.len(), 2);
        let polys = &layers.iter().find(|(d, _)| d.name.ends_with("polygons")).unwrap().0;
        assert!(matches!(&polys.features[0].geometry, Some(Geometry::Polygon(p)) if p[0].len() == 2));
        assert_eq!(polys.features[0].attrs[1], Value::Text("https://x.org/lake".into()));
        assert_eq!(polys.features[0].attrs[4], Value::Text("urn:1".into()));
    }

    #[test]
    fn junk_is_refused() {
        assert!(read("x.rss", b"<html></html>").is_err());
        assert!(read("x.rss", b"<rss><channel><item><title>x</title></item></channel></rss>").is_err(), "no places");
        assert!(read("x.rss", b"\xff\xfe").is_err());
        let truncated: Vec<u8> = RSS.bytes().take(RSS.len() / 2).collect();
        assert!(read("x.rss", &truncated).is_err());
        assert_eq!(lat_lons("1 2 3"), None);
        assert_eq!(lat_lons("nan 2"), None);
    }

    #[test]
    fn unreadable_interior_ring_is_said() {
        let feed = |hole: &str| {
            format!(
                r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml">
<channel><title>Zoning</title>
<item><title>Park</title><georss:where><gml:Polygon>
  <gml:exterior><gml:LinearRing><gml:posList>45 -110 46 -110 46 -111 45 -110</gml:posList></gml:LinearRing></gml:exterior>
  <gml:interior><gml:LinearRing><gml:posList>{hole}</gml:posList></gml:LinearRing></gml:interior>
</gml:Polygon></georss:where></item>
</channel></rss>"#
            )
        };

        let lossy = read("lossy.rss", feed("bad coords").as_bytes()).unwrap();
        assert_eq!(lossy[0].0.features.len(), 1);
        assert!(lossy[0].0.features[0].geometry.is_some());
        assert!(
            lossy[0].1.messages.iter().any(|m| m.loss == Some(crate::loss::LossCode::DroppedRing)
                && m.text.contains("1 hole in a polygon was unreadable and left out")),
            "{:?}",
            lossy[0].1.messages
        );

        let clean = read("clean.rss", feed("45.2 -110.2 45.8 -110.2 45.8 -110.8 45.2 -110.2").as_bytes()).unwrap();
        assert_eq!(clean[0].0.features.len(), 1);
        assert!(clean[0].1.messages.iter().all(|m| m.loss.is_none()));

        let multi = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml">
<channel><title>Zoning</title>
<item><title>Park</title><georss:where><gml:Polygon>
  <gml:exterior><gml:LinearRing><gml:posList>45 -110 46 -110 46 -111 45 -110</gml:posList></gml:LinearRing></gml:exterior>
  <gml:interior><gml:LinearRing><gml:posList>bad one</gml:posList></gml:LinearRing></gml:interior>
  <gml:interior><gml:LinearRing><gml:posList>bad two</gml:posList></gml:LinearRing></gml:interior>
</gml:Polygon></georss:where></item>
</channel></rss>"#;
        let lossy_multi = read("multi.rss", multi.as_bytes()).unwrap();
        assert!(
            lossy_multi[0]
                .1
                .messages
                .iter()
                .any(|m| m.loss.is_some() && m.text.contains("2 holes in polygons were unreadable and left out")),
            "{:?}",
            lossy_multi[0].1.messages
        );

        let mixed = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:georss="http://www.georss.org/georss" xmlns:gml="http://www.opengis.net/gml">
<channel><title>Zoning</title>
<item><title>Park</title><georss:where><gml:Polygon>
  <gml:exterior><gml:LinearRing><gml:posList>45 -110 46 -110 46 -111 45 -110</gml:posList></gml:LinearRing></gml:exterior>
  <gml:interior><gml:LinearRing><gml:posList>45.2 -110.2 45.8 -110.2 45.8 -110.8 45.2 -110.2</gml:posList></gml:LinearRing></gml:interior>
  <gml:interior><gml:LinearRing><gml:posList>corrupt hole</gml:posList></gml:LinearRing></gml:interior>
</gml:Polygon></georss:where></item>
</channel></rss>"#;
        let mixed_lossy = read("mixed.rss", mixed.as_bytes()).unwrap();
        assert!(
            mixed_lossy[0]
                .1
                .messages
                .iter()
                .any(|m| m.loss.is_some() && m.text.contains("1 hole in a polygon was unreadable and left out")),
            "{:?}",
            mixed_lossy[0].1.messages
        );
        match &mixed_lossy[0].0.features[0].geometry {
            Some(Geometry::Polygon(polys)) => assert_eq!(polys[0].len(), 2, "must keep exterior and valid interior ring"),
            other => panic!("expected polygon, got {other:?}"),
        }
    }
}
