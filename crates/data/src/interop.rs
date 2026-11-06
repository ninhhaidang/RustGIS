//! Writers for exchange formats: KML, GPX and CSV with WKT geometry.

use std::fmt::Write as _;

use rustgis_geom::Coord;

use crate::{Dataset, Geometry};

/// Text for XML: special characters escaped, control characters (invalid in XML 1.0) dropped.
fn xml(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn coords(pts: &[Coord]) -> String {
    pts.iter().map(|c| format!("{},{}", c.x, c.y)).collect::<Vec<_>>().join(" ")
}

fn kml_geometry(g: &Geometry) -> String {
    match g {
        Geometry::Point(c) => format!("<Point><coordinates>{},{}</coordinates></Point>", c.x, c.y),
        Geometry::MultiPoint(v) => {
            format!("<MultiGeometry>{}</MultiGeometry>", v.iter().map(|c| kml_geometry(&Geometry::Point(*c))).collect::<String>())
        }
        Geometry::Line(parts) => {
            let ls: String = parts.iter().map(|p| format!("<LineString><coordinates>{}</coordinates></LineString>", coords(p))).collect();
            if parts.len() == 1 { ls } else { format!("<MultiGeometry>{ls}</MultiGeometry>") }
        }
        Geometry::Polygon(polys) => {
            let ps: String = polys
                .iter()
                .map(|rings| {
                    let mut it = rings.iter();
                    let outer = it
                        .next()
                        .map(|r| {
                            format!("<outerBoundaryIs><LinearRing><coordinates>{}</coordinates></LinearRing></outerBoundaryIs>", coords(r))
                        })
                        .unwrap_or_default();
                    let inner: String = it
                        .map(|r| {
                            format!("<innerBoundaryIs><LinearRing><coordinates>{}</coordinates></LinearRing></innerBoundaryIs>", coords(r))
                        })
                        .collect();
                    format!("<Polygon>{outer}{inner}</Polygon>")
                })
                .collect();
            if polys.len() == 1 { ps } else { format!("<MultiGeometry>{ps}</MultiGeometry>") }
        }
    }
}

/// A KML document, one placemark per feature, named by `name_field` and with every attribute.
/// A cancelled run ([`crate::progress`]) stops it with [`crate::DataError::Cancelled`].
pub fn to_kml(ds: &Dataset, name_field: Option<usize>) -> crate::Result<String> {
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<kml xmlns=\"http://www.opengis.net/kml/2.2\"><Document><name>{}</name>\n",
        xml(&ds.name)
    );
    for (r, f) in ds.features.iter().enumerate() {
        crate::progress::step(1)?;
        let Some(g) = &f.geometry else { continue };
        let name = name_field.map(|fi| ds.value(r, fi).to_string()).unwrap_or_else(|| f.id.to_string());
        let data: String = ds
            .fields
            .iter()
            .zip(&f.attrs)
            .filter(|(_, v)| !v.is_null())
            .map(|(fd, v)| format!("<Data name=\"{}\"><value>{}</value></Data>", xml(&fd.name), xml(&v.to_string())))
            .collect();
        out.push_str(&format!(
            "<Placemark><name>{}</name><ExtendedData>{data}</ExtendedData>{}</Placemark>\n",
            xml(&name),
            kml_geometry(g)
        ));
    }
    out.push_str("</Document></kml>\n");
    Ok(out)
}

/// A GPX file: points become waypoints, lines tracks (polygon outlines too).
/// A cancelled run ([`crate::progress`]) stops it with [`crate::DataError::Cancelled`].
pub fn to_gpx(ds: &Dataset, name_field: Option<usize>) -> crate::Result<String> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"RustGIS\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n",
    );
    let pt = |c: &Coord| format!("lat=\"{}\" lon=\"{}\"", c.y, c.x);
    let mut tracks = String::new();
    for (r, f) in ds.features.iter().enumerate() {
        crate::progress::step(1)?;
        let Some(g) = &f.geometry else { continue };
        let name = xml(&name_field.map(|fi| ds.value(r, fi).to_string()).unwrap_or_else(|| f.id.to_string()));
        match g {
            Geometry::Point(c) => out.push_str(&format!("<wpt {}><name>{name}</name></wpt>\n", pt(c))),
            Geometry::MultiPoint(v) => v.iter().for_each(|c| out.push_str(&format!("<wpt {}><name>{name}</name></wpt>\n", pt(c)))),
            Geometry::Line(parts) => {
                let segs: String = parts
                    .iter()
                    .map(|p| format!("<trkseg>{}</trkseg>", p.iter().map(|c| format!("<trkpt {}/>", pt(c))).collect::<String>()))
                    .collect();
                tracks.push_str(&format!("<trk><name>{name}</name>{segs}</trk>\n"));
            }
            Geometry::Polygon(polys) => {
                let segs: String = polys
                    .iter()
                    .filter_map(|r| r.first())
                    .map(|p| format!("<trkseg>{}</trkseg>", p.iter().map(|c| format!("<trkpt {}/>", pt(c))).collect::<String>()))
                    .collect();
                tracks.push_str(&format!("<trk><name>{name}</name>{segs}</trk>\n"));
            }
        }
    }
    out.push_str(&tracks);
    out.push_str("</gpx>\n");
    Ok(out)
}

/// Well-known text for a geometry.
pub fn wkt(g: &Geometry) -> String {
    fn coord(out: &mut String, p: &Coord) {
        let _ = write!(out, "{} {}", p.x, p.y);
    }
    fn list(out: &mut String, v: &[Coord]) {
        for (i, p) in v.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            coord(out, p);
        }
    }
    fn rings(out: &mut String, rs: &[Vec<Coord>]) {
        for (i, r) in rs.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('(');
            list(out, r);
            out.push(')');
        }
    }
    let mut out = String::new();
    match g {
        Geometry::Point(p) => {
            out.push_str("POINT (");
            coord(&mut out, p);
            out.push(')');
        }
        Geometry::MultiPoint(v) => {
            out.push_str("MULTIPOINT (");
            for (i, p) in v.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('(');
                coord(&mut out, p);
                out.push(')');
            }
            out.push(')');
        }
        Geometry::Line(parts) if parts.len() == 1 => {
            out.push_str("LINESTRING (");
            if let Some(p) = parts.first() {
                list(&mut out, p);
            }
            out.push(')');
        }
        Geometry::Line(parts) => {
            out.push_str("MULTILINESTRING (");
            for (i, p) in parts.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('(');
                list(&mut out, p);
                out.push(')');
            }
            out.push(')');
        }
        Geometry::Polygon(polys) => {
            if polys.len() == 1 {
                out.push_str("POLYGON (");
                if let Some(rs) = polys.first() {
                    rings(&mut out, rs);
                }
                out.push(')');
            } else {
                out.push_str("MULTIPOLYGON (");
                for (i, rs) in polys.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push('(');
                    rings(&mut out, rs);
                    out.push(')');
                }
                out.push(')');
            }
        }
    }
    out
}

/// Decodes an encoded polyline (Google's algorithm; `precision` 5, or 6 as OSRM and Valhalla
/// can use) into longitude/latitude points.
pub fn decode_polyline(s: &str, precision: u32) -> Result<Vec<Coord>, String> {
    if !(1..=10).contains(&precision) {
        return Err("precision is 1 to 10 (usually 5 or 6)".into());
    }
    let factor = 10f64.powi(precision as i32);
    let mut bytes = s.trim().bytes().peekable();
    let mut next = || -> Result<Option<i64>, String> {
        if bytes.peek().is_none() {
            return Ok(None);
        }
        let (mut result, mut shift) = (0i64, 0u32);
        loop {
            let b = bytes.next().ok_or("the polyline ends in the middle of a number")?;
            if !(63..=126).contains(&b) {
                return Err(format!("'{}' isn't part of an encoded polyline", char::from(b)));
            }
            let chunk = i64::from(b - 63);
            if shift > 60 {
                return Err("a number in the polyline is too long".into());
            }
            result |= (chunk & 0x1f) << shift;
            shift += 5;
            if chunk < 0x20 {
                break;
            }
        }
        Ok(Some(if result & 1 == 1 { !(result >> 1) } else { result >> 1 }))
    };
    let (mut lat, mut lon) = (0i64, 0i64);
    let mut out = Vec::new();
    while let Some(dlat) = next()? {
        let dlon = next()?.ok_or("the polyline ends after a latitude")?;
        lat = lat.saturating_add(dlat);
        lon = lon.saturating_add(dlon);
        let (y, x) = (lat as f64 / factor, lon as f64 / factor);
        if !((-90.0..=90.0).contains(&y) && (-180.0..=180.0).contains(&x)) {
            return Err(format!(
                "the polyline leaves longitude/latitude ({x}, {y}): is its precision {}?",
                if precision == 5 { 6 } else { 5 }
            ));
        }
        out.push(Coord::new(x, y));
        if out.len() > MAX_WKT_COORDS {
            return Err("the polyline is too long".into());
        }
    }
    Ok(out)
}

/// The most coordinates one WKT geometry may hold.
const MAX_WKT_COORDS: usize = 5_000_000;

/// Reads well-known text (or EWKT, `SRID=2263;POINT (…)`): the geometry (`None` when empty) and
/// the SRID if one is given. Z and M values are dropped (see [`parse_wkt_zm`]); a
/// `GEOMETRYCOLLECTION` keeps its first non-empty member.
pub fn parse_wkt(text: &str) -> Result<(Option<Geometry>, Option<u32>), String> {
    parse_wkt_zm(text).map(|(g, srid, _)| (g, srid))
}

/// [`parse_wkt`], also telling whether a coordinate had Z or M values, which were dropped.
pub fn parse_wkt_zm(text: &str) -> Result<(Option<Geometry>, Option<u32>, bool), String> {
    let text = text.trim();
    let (srid, body) = if text.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("SRID=")) {
        let (s, rest) = text.split_once(';').ok_or("an SRID= without the ';' before its geometry")?;
        (Some(s.get(5..).unwrap_or("").trim().parse::<u32>().map_err(|_| "a bad SRID".to_string())?), rest)
    } else {
        (None, text)
    };
    let mut p = WktParser { toks: wkt_tokens(body)?, at: 0, coords: 0, zm: false };
    let g = p.geometry(0)?;
    if p.at != p.toks.len() {
        return Err("text after the geometry".into());
    }
    Ok((g, srid, p.zm))
}

#[derive(Debug, PartialEq)]
enum WktTok {
    Word(String),
    Num(f64),
    Open,
    Close,
    Comma,
}

fn wkt_tokens(s: &str) -> Result<Vec<WktTok>, String> {
    let mut out = Vec::new();
    let mut chars = s.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' | '[' => {
                out.push(WktTok::Open);
                chars.next();
            }
            ')' | ']' => {
                out.push(WktTok::Close);
                chars.next();
            }
            ',' => {
                out.push(WktTok::Comma);
                chars.next();
            }
            c if c.is_ascii_alphabetic() => {
                let mut end = i;
                while let Some(&(j, d)) = chars.peek() {
                    if !d.is_ascii_alphabetic() {
                        break;
                    }
                    end = j + d.len_utf8();
                    chars.next();
                }
                out.push(WktTok::Word(s.get(i..end).unwrap_or("").to_ascii_uppercase()));
            }
            c if c.is_ascii_digit() || matches!(c, '-' | '+' | '.') => {
                let mut end = i;
                while let Some(&(j, d)) = chars.peek() {
                    if !(d.is_ascii_digit() || matches!(d, '-' | '+' | '.' | 'e' | 'E')) {
                        break;
                    }
                    end = j + d.len_utf8();
                    chars.next();
                }
                let n: f64 = s.get(i..end).unwrap_or("").parse().map_err(|_| format!("a bad number '{}'", s.get(i..end).unwrap_or("")))?;
                out.push(WktTok::Num(n));
            }
            c => return Err(format!("an unexpected '{c}'")),
        }
    }
    Ok(out)
}

struct WktParser {
    toks: Vec<WktTok>,
    at: usize,
    coords: usize,
    /// A coordinate had more than x and y.
    zm: bool,
}

impl WktParser {
    fn next(&mut self) -> Option<&WktTok> {
        let t = self.toks.get(self.at);
        self.at += 1;
        t
    }

    fn peek(&self) -> Option<&WktTok> {
        self.toks.get(self.at)
    }

    fn expect(&mut self, t: WktTok) -> Result<(), String> {
        if self.next() == Some(&t) { Ok(()) } else { Err(format!("expected {t:?}")) }
    }

    /// `x y [z [m]]`.
    fn coord(&mut self) -> Result<Coord, String> {
        let mut nums = [0.0f64; 4];
        let mut len = 0usize;
        while let Some(WktTok::Num(n)) = self.peek() {
            // Two to four numbers: a fifth is already wrong, so stop there.
            if len == nums.len() {
                return Err("a coordinate needs two to four numbers".into());
            }
            nums[len] = *n;
            len += 1;
            self.at += 1;
        }
        self.coords += 1;
        self.zm |= len > 2;
        if self.coords > MAX_WKT_COORDS {
            return Err("too many coordinates".into());
        }
        let (x, y) = (nums[0], nums[1]);
        if len < 2 || !x.is_finite() || !y.is_finite() {
            return Err("a coordinate needs two to four numbers".into());
        }
        Ok(Coord::new(x, y))
    }

    /// `( a, b, … )`, each item read by `item`.
    fn list<T>(&mut self, mut item: impl FnMut(&mut Self) -> Result<T, String>) -> Result<Vec<T>, String> {
        self.expect(WktTok::Open)?;
        let mut out = vec![item(self)?];
        loop {
            match self.next() {
                Some(WktTok::Comma) => out.push(item(self)?),
                Some(WktTok::Close) => return Ok(out),
                _ => return Err("expected ',' or ')'".into()),
            }
        }
    }

    fn coords(&mut self) -> Result<Vec<Coord>, String> {
        self.list(Self::coord)
    }

    fn rings(&mut self) -> Result<Vec<Vec<Coord>>, String> {
        self.list(Self::coords)
    }

    /// Whether the next word is `EMPTY` (taken if so).
    fn empty(&mut self) -> bool {
        if matches!(self.peek(), Some(WktTok::Word(w)) if w == "EMPTY") {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn geometry(&mut self, depth: usize) -> Result<Option<Geometry>, String> {
        if depth > 8 {
            return Err("geometry collections nested too deep".into());
        }
        let Some(WktTok::Word(kind)) = self.next() else { return Err("expected a geometry type".into()) };
        let kind = kind.clone();
        // `POINT Z (…)`, `POINTZ (…)`, `POINT ZM (…)`: the dimensions are read, then dropped.
        if matches!(self.peek(), Some(WktTok::Word(w)) if matches!(w.as_str(), "Z" | "M" | "ZM")) {
            self.at += 1;
        }
        let kind = kind.strip_suffix("ZM").or_else(|| kind.strip_suffix('Z')).or_else(|| kind.strip_suffix('M')).unwrap_or(&kind);
        if self.empty() {
            return Ok(None);
        }
        Ok(Some(match kind {
            "POINT" => {
                self.expect(WktTok::Open)?;
                let c = self.coord()?;
                self.expect(WktTok::Close)?;
                Geometry::Point(c)
            }
            // Points with or without their own parentheses.
            "MULTIPOINT" => Geometry::MultiPoint(self.list(|p| {
                if p.peek() == Some(&WktTok::Open) {
                    p.at += 1;
                    let c = p.coord()?;
                    p.expect(WktTok::Close)?;
                    Ok(c)
                } else {
                    p.coord()
                }
            })?),
            "LINESTRING" => Geometry::Line(vec![self.coords()?]),
            "MULTILINESTRING" => Geometry::Line(self.rings()?),
            "POLYGON" => Geometry::Polygon(vec![self.rings()?]),
            "MULTIPOLYGON" => Geometry::Polygon(self.list(Self::rings)?),
            "GEOMETRYCOLLECTION" => {
                let members = self.list(|p| p.geometry(depth + 1))?;
                return Ok(members.into_iter().flatten().next());
            }
            other => return Err(format!("'{other}' geometries aren't read")),
        }))
    }
}

/// CSV with a `wkt` geometry column and every attribute.
pub fn to_wkt_csv(ds: &Dataset) -> String {
    // Text starting like a formula is prefixed with ' so spreadsheets show it rather than run it.
    let q = |s: &str| {
        let s = if s.starts_with(['=', '+', '-', '@']) && s.parse::<f64>().is_err() { format!("'{s}") } else { s.to_string() };
        if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
    };
    let mut out = String::from("wkt");
    for f in &ds.fields {
        out.push(',');
        out.push_str(&q(&f.name));
    }
    out.push('\n');
    for f in &ds.features {
        out.push_str(&q(&f.geometry.as_ref().map(wkt).unwrap_or_default()));
        for v in &f.attrs {
            out.push(',');
            out.push_str(&q(&if v.is_null() { String::new() } else { v.to_string() }));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Field, FieldType, GeometryType, Value};

    #[test]
    fn encoded_polylines() {
        // Google's example.
        let pts = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 5).unwrap();
        let want = [(-120.2, 38.5), (-120.95, 40.7), (-126.453, 43.252)];
        assert_eq!(pts.len(), 3);
        for (p, (x, y)) in pts.iter().zip(want) {
            assert!((p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9, "{p:?}");
        }
        // Precision 6 of the same text is a tenth of the size.
        let six = decode_polyline("_p~iF~ps|U", 6).unwrap();
        assert!((six[0].y - 3.85).abs() < 1e-9);
        assert!(decode_polyline("_p~iF", 5).is_err(), "a latitude without a longitude");
        assert!(decode_polyline("_p~iF~ps|U ", 5).is_ok(), "trailing space");
        assert!(decode_polyline("_p~i", 5).is_err(), "cut in a number");
        assert!(decode_polyline("abc def", 5).is_err());
        assert!(decode_polyline("~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~", 5).is_err());
        assert!(decode_polyline("_p~iF~ps|U", 0).is_err());
        assert_eq!(decode_polyline("", 5).unwrap(), Vec::<Coord>::new());
    }

    #[test]
    fn writes_kml_gpx_wkt() {
        let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        ds.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris & co".into())]);
        let k = to_kml(&ds, Some(0)).unwrap();
        assert!(k.contains("<name>Paris &amp; co</name>") && k.contains("<coordinates>2.35,48.85</coordinates>"));
        let g = to_gpx(&ds, Some(0)).unwrap();
        assert!(g.contains("<wpt lat=\"48.85\" lon=\"2.35\">"));
        assert_eq!(wkt(&Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), "LINESTRING (0 0, 1 1)");
        assert!(to_wkt_csv(&ds).starts_with("wkt,name\nPOINT (2.35 48.85),Paris & co"));
    }
}
