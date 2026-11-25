//! GPX: GPS exchange files, split into layers as GPS software shows them — waypoints, routes,
//! tracks (one line per track, its segments as parts) and track points (with elevation and time,
//! for profiles and time series).
//!
//! The file is hostile: no DTD is read, the node count is capped, and points without valid
//! coordinates are skipped and counted.

use roxmltree::{Node, ParsingOptions};
use rustgis_geom::Coord;

use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn children<'a, 'i>(n: &Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| c.is_element() && local(c) == name)
}

fn child_text(n: &Node, name: &str) -> Option<String> {
    let c = n.children().find(|c| c.is_element() && local(c) == name)?;
    let t = c.children().filter(|t| t.is_text()).filter_map(|t| t.text()).collect::<String>();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A point's coordinate from its `lat` and `lon` attributes.
fn coord(n: &Node) -> Option<Coord> {
    let x = n.attribute("lon")?.trim().parse::<f64>().ok()?;
    let y = n.attribute("lat")?.trim().parse::<f64>().ok()?;
    crate::io::lon_lat_in_range(x, y).then(|| Coord::new(x, y))
}

/// Whether reading keeps a feature that [`crate::interop::to_gpx`] writes with geometry `g`: a
/// waypoint in range ([`coord`]), or a track with a segment of two such points.
pub(crate) fn keeps(g: &Geometry) -> bool {
    let kept = |c: &&Coord| crate::io::lon_lat_in_range(c.x, c.y);
    let segment = |p: &Vec<Coord>| p.iter().filter(kept).count() >= 2;
    match g {
        Geometry::Point(c) => kept(&c),
        Geometry::MultiPoint(v) => v.iter().any(|c| kept(&c)),
        Geometry::Line(parts) => parts.iter().any(segment),
        // Written as a track along each part's outer ring.
        Geometry::Polygon(polys) => polys.iter().filter_map(|rings| rings.first()).any(segment),
    }
}

/// The vertices of a feature reading keeps ([`keeps`]) that don't read back with it: points of a
/// multipoint outside longitude/latitude ([`coord`]), and those of a track's segments (a polygon's
/// outer rings) out of range, or all of a segment left with fewer than two, which reading leaves
/// out of the track.
pub(crate) fn lost_vertices(g: &Geometry) -> usize {
    let in_range = |p: &[Coord]| p.iter().filter(|c| crate::io::lon_lat_in_range(c.x, c.y)).count();
    let segment = |p: &Vec<Coord>| {
        let n = in_range(p);
        if n >= 2 { p.len().saturating_sub(n) } else { p.len() }
    };
    match g {
        Geometry::Point(_) => 0,
        Geometry::MultiPoint(v) => v.len().saturating_sub(in_range(v)),
        Geometry::Line(parts) => parts.iter().map(segment).sum(),
        Geometry::Polygon(polys) => polys.iter().filter_map(|rings| rings.first()).map(segment).sum(),
    }
}

fn text(v: Option<String>) -> Value {
    v.map(Value::Text).unwrap_or_default()
}

/// Reads a GPX file into its layers (those with features).
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Vec<(Dataset, ReadReport)>> {
    let name = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("gpx").to_string();
    let text_in = std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc =
        crate::xml::parse(text_in.trim_start_matches('\u{feff}'), opts).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    let gpx = doc.root_element();
    if local(&gpx) != "gpx" {
        return Err(DataError::Format(format!("{file_name}: not a GPX file")));
    }
    let mut skipped = 0usize;
    let mut short_segments = 0usize;
    // Route points without usable coordinates: vertices left out of their route, not features.
    let mut route_points = 0usize;
    let point_fields = |extra: &[(&str, FieldType)]| {
        let mut f: Vec<Field> = extra.iter().map(|(n, t)| Field::new(*n, *t)).collect();
        f.extend(["name", "ele", "time", "desc", "cmt", "sym", "type"].iter().map(|n| Field::new(*n, FieldType::Text)));
        f
    };
    let point_attrs =
        |p: &Node| -> Vec<Value> { ["name", "ele", "time", "desc", "cmt", "sym", "type"].iter().map(|k| text(child_text(p, k))).collect() };

    let mut waypoints = Dataset::new(format!("{name} waypoints"), GeometryType::Point, point_fields(&[]));
    for w in children(&gpx, "wpt") {
        match coord(&w) {
            Some(c) => waypoints.push_next(Some(Geometry::Point(c)), point_attrs(&w)),
            None => skipped += 1,
        }
    }

    let line_fields = || {
        ["name", "desc", "cmt", "type", "number", "points"]
            .iter()
            .map(|n| Field::new(*n, if *n == "points" { FieldType::Integer } else { FieldType::Text }))
            .collect::<Vec<_>>()
    };
    let line_attrs = |n: &Node, points: usize| -> Vec<Value> {
        let mut v: Vec<Value> = ["name", "desc", "cmt", "type", "number"].iter().map(|k| text(child_text(n, k))).collect();
        v.push(Value::Int(i64::try_from(points).unwrap_or(i64::MAX)));
        v
    };
    let mut routes = Dataset::new(format!("{name} routes"), GeometryType::Polyline, line_fields());
    for r in children(&gpx, "rte") {
        let (mut pts, mut unplaced) = (Vec::new(), 0usize);
        for p in children(&r, "rtept") {
            match coord(&p) {
                Some(c) => pts.push(c),
                None => unplaced += 1,
            }
        }
        let n = pts.len();
        if n >= 2 {
            routes.push_next(Some(Geometry::Line(vec![pts])), line_attrs(&r, n));
            route_points += unplaced;
        } else {
            // Left out whole, said with the routes; its points aren't also a kept route's loss.
            skipped += 1;
        }
    }

    let mut tracks = Dataset::new(format!("{name} tracks"), GeometryType::Polyline, line_fields());
    let mut track_points = Dataset::new(
        format!("{name} track points"),
        GeometryType::Point,
        point_fields(&[("track", FieldType::Integer), ("segment", FieldType::Integer)]),
    );
    for (ti, t) in children(&gpx, "trk").enumerate() {
        let mut parts = Vec::new();
        let mut count = 0;
        for (si, seg) in children(&t, "trkseg").enumerate() {
            let mut part = Vec::new();
            for p in children(&seg, "trkpt") {
                let Some(c) = coord(&p) else {
                    skipped += 1;
                    continue;
                };
                part.push(c);
                let mut attrs = vec![Value::Int(ti as i64 + 1), Value::Int(si as i64 + 1)];
                attrs.extend(point_attrs(&p));
                track_points.push_next(Some(Geometry::Point(c)), attrs);
            }
            count += part.len();
            if part.len() >= 2 {
                parts.push(part);
            } else if !part.is_empty() {
                short_segments += 1;
            }
        }
        if parts.is_empty() {
            skipped += 1;
        } else {
            tracks.push_next(Some(Geometry::Line(parts)), line_attrs(&t, count));
        }
    }

    let mut out = Vec::new();
    for mut ds in [waypoints, routes, tracks, track_points] {
        if ds.features.is_empty() {
            continue;
        }
        // Elevations are inferred; timestamps are explicitly typed by the GPX schema.
        crate::io::type_text_columns(&mut ds, &["name", "desc", "cmt", "sym", "type", "time", "number"]);
        let mut report = ReadReport::default();
        if let Some(k) = ds.field_index("time") {
            if let Some(field) = ds.fields.get_mut(k) {
                field.ty = FieldType::DateTime;
            }
            let mut counts = Vec::new();
            for feature in &mut ds.features {
                crate::progress::step(1)?;
                if let Some(value @ Value::Text(_)) = feature.attrs.get_mut(k) {
                    let Value::Text(s) = &*value else { continue };
                    let parsed = crate::DateTime::parse(s).filter(|_| s.contains(['T', 't']));
                    if parsed.is_none() {
                        crate::source::unread_date(&mut counts, k);
                    }
                    *value = parsed.map(Value::DateTime).unwrap_or_default();
                }
            }
            report.unread_dates = crate::source::unread_dates_note(&ds.fields, &counts);
            report.messages.extend(report.unread_dates.clone());
        }
        out.push((ds, report));
    }
    if let Some((_, report)) = out.first_mut() {
        if short_segments > 0 {
            let (segment, action) = if short_segments == 1 {
                ("segment", "was left out of tracks; its point remains in track points")
            } else {
                ("segments", "were left out of tracks; their points remain in track points")
            };
            report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedGeometry,
                format!("{name}: {short_segments} GPX track {segment} with fewer than two usable points {action}"),
            ));
        }
        if route_points > 0 {
            let what = if route_points == 1 {
                "point without usable coordinates was left out of its route"
            } else {
                "points without usable coordinates were left out of their routes"
            };
            report
                .messages
                .push(crate::loss::Note::loss(crate::loss::LossCode::DroppedGeometry, format!("{name}: {route_points} GPX route {what}")));
        }
        report.skipped = skipped;
        if skipped > 0 {
            let what = if skipped == 1 {
                "point, route or track without usable geometry was skipped"
            } else {
                "points, routes or tracks without usable geometry were skipped"
            };
            report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedFeature, format!("{name}: {skipped} {what}")));
        }
    }
    if out.is_empty() {
        return Err(DataError::Format(format!("{file_name}: no waypoints, routes or tracks")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_loss_short_segments_are_counted_but_their_points_remain() {
        let xml = br#"<gpx><trk><name>kept</name><trkseg><trkpt lat="1" lon="2"/><trkpt lat="3" lon="4"/></trkseg><trkseg><trkpt lat="5" lon="6"/></trkseg><trkseg/></trk></gpx>"#;
        let layers = read("a.gpx", xml).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].0.features[0].geometry, Some(Geometry::Line(vec![vec![Coord::new(2.0, 1.0), Coord::new(4.0, 3.0)]])));
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("kept".into()));
        assert_eq!(layers[1].0.features.len(), 3);
        assert_eq!(layers[1].0.features[2].geometry, Some(Geometry::Point(Coord::new(6.0, 5.0))));
        assert_eq!(layers[1].0.value(2, 1), &Value::Int(2));
        assert_eq!(
            layers[0].1.messages.texts().collect::<Vec<_>>(),
            ["a: 1 GPX track segment with fewer than two usable points was left out of tracks; its point remains in track points"]
        );
        assert_eq!(layers[0].1.messages[0].loss, Some(crate::loss::LossCode::DroppedGeometry));
        assert!(layers[1].1.messages.is_empty());
        let clean =
            read("a.gpx", br#"<gpx><trk><trkseg><trkpt lat="1" lon="2"/><trkpt lat="3" lon="4"/></trkseg><trkseg/></trk></gpx>"#).unwrap();
        assert!(clean.iter().all(|(_, r)| r.messages.is_empty()));
    }

    const GPX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="48.85" lon="2.35"><ele>35</ele><name>Paris</name><sym>Flag</sym></wpt>
  <wpt lat="91" lon="2"><name>bad</name></wpt>
  <rte><name>R1</name><rtept lat="1" lon="1"/><rtept lat="2" lon="2"/></rte>
  <trk><name>Morning run</name><type>running</type>
    <trkseg><trkpt lat="45.0" lon="6.0"><ele>1000.5</ele><time>2024-05-01T07:00:00Z</time></trkpt>
            <trkpt lat="45.001" lon="6.001"><ele>1002</ele><time>2024-05-01T07:00:10Z</time></trkpt></trkseg>
    <trkseg><trkpt lat="45.01" lon="6.01"/><trkpt lat="45.02" lon="6.02"/><trkpt lat="x" lon="6"/></trkseg>
  </trk>
</gpx>"#;

    /// A route point without usable coordinates is left out of a route that is kept: its geometry
    /// lost a vertex, said apart from the points, routes and tracks left out whole.
    #[test]
    fn a_route_point_without_coordinates_is_a_vertex_left_out() {
        let xml = br#"<gpx><rte><name>kept</name><rtept lat="1" lon="1"/><rtept lat="x" lon="1"/><rtept lat="2" lon="2"/></rte><wpt lat="91" lon="2"/></gpx>"#;
        let layers = read("a.gpx", xml).unwrap();
        let (routes, _) = layers.iter().find(|(d, _)| d.name == "a routes").unwrap();
        assert_eq!(routes.features.len(), 1);
        let report = &layers[0].1;
        assert_eq!(report.skipped, 1, "only the waypoint off the globe is a feature left out");
        let said: Vec<_> = report.messages.iter().map(|m| (m.as_str(), m.loss)).collect();
        assert_eq!(
            said,
            [
                ("a: 1 GPX route point without usable coordinates was left out of its route", Some(crate::loss::LossCode::DroppedGeometry)),
                ("a: 1 point, route or track without usable geometry was skipped", Some(crate::loss::LossCode::DroppedFeature)),
            ]
        );
    }

    #[test]
    fn splits_into_layers() {
        let layers = read("run.gpx", GPX.as_bytes()).unwrap();
        let names: Vec<&str> = layers.iter().map(|l| l.0.name.as_str()).collect();
        assert_eq!(names, ["run waypoints", "run routes", "run tracks", "run track points"]);
        let (wpt, report) = &layers[0];
        assert_eq!(wpt.features.len(), 1);
        assert_eq!(wpt.value(0, wpt.field_index("ele").unwrap()), &Value::Int(35));
        assert_eq!(report.skipped, 2, "the waypoint off the globe and the track point without coordinates");
        let trk = &layers[2].0;
        assert!(matches!(&trk.features[0].geometry, Some(Geometry::Line(p)) if p.len() == 2));
        assert_eq!(trk.value(0, trk.field_index("points").unwrap()), &Value::Int(4));
        assert_eq!(trk.value(0, trk.field_index("type").unwrap()), &Value::Text("running".into()));
        let tp = &layers[3].0;
        assert_eq!(tp.features.len(), 4);
        assert_eq!(tp.value(0, tp.field_index("ele").unwrap()), &Value::Float(1000.5));
        assert_eq!(tp.value(1, tp.field_index("time").unwrap()), &Value::parse("2024-05-01T07:00:10Z", FieldType::DateTime));
        assert_eq!(tp.value(3, tp.field_index("segment").unwrap()), &Value::Int(2));
    }

    #[test]
    fn invalid_gpx_times_are_counted_without_changing_clean_times() {
        let xml = br#"<gpx><wpt lat="1" lon="1"><time>2026-02-30T12:00:00Z</time></wpt><wpt lat="2" lon="2"><time>2026-03-05T14:30:00.123456+02:00</time></wpt><wpt lat="3" lon="3"/></gpx>"#;
        let layers = read("times.gpx", xml).unwrap();
        let (ds, report) = &layers[0];
        let k = ds.field_index("time").unwrap();
        assert_eq!(ds.fields[k].ty, FieldType::DateTime);
        assert!(ds.value(0, k).is_null() && ds.value(2, k).is_null());
        assert_eq!(ds.value(1, k).to_string(), "2026-03-05T14:30:00.123456+02:00");
        assert!(report.unread_dates.as_ref().map(crate::loss::Note::as_str).unwrap().starts_with("1 value"));
        assert!(read("clean.gpx", GPX.as_bytes()).unwrap().iter().all(|(_, r)| r.unread_dates.is_none()));
    }

    #[test]
    fn hostile_input() {
        assert!(read("x.gpx", b"<kml/>").is_err());
        assert!(read("x.gpx", b"<gpx>").is_err());
        assert!(read("x.gpx", b"<gpx version=\"1.1\"></gpx>").is_err());
        let bomb = r#"<?xml version="1.0"?><!DOCTYPE gpx [<!ENTITY a "aaaa">]><gpx><wpt lat="1" lon="1"><name>&a;</name></wpt></gpx>"#;
        assert!(read("b.gpx", bomb.as_bytes()).is_err());
        for n in (0..GPX.len()).step_by(41) {
            let _ = read("t.gpx", &GPX.as_bytes()[..n]);
        }
    }
}
