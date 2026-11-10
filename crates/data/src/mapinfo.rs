//! MapInfo Interchange Format: a `.mif` file (header, columns and objects) with its `.mid` file
//! (one row of attributes per object). Points, lines, polylines, regions (holes found by
//! nesting), rectangles and ellipses are read; text and arcs are kept as rows without geometry.
//!
//! The `CoordSys` clause gives the coordinate system: longitude/latitude, or Transverse Mercator,
//! Lambert Conformal Conic, Albers or Mercator by their parameters, converted to
//! longitude/latitude; anything else is kept as it is and said.
//!
//! The file is hostile: counts are checked against what follows and bounded, and an object that
//! can't be read ends the reading with a message, keeping what came before.

use rustgis_geom::Coord;

use crate::raster::Crs;
use crate::raster::crs::CustomCrs;
use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// The most coordinates one object may hold.
const MAX_COORDS: usize = 10_000_000;

/// The most rings of a region whose holes are found by nesting (the test is quadratic).
const MAX_NESTED_RINGS: usize = 5_000;

pub fn is_mif_name(name: &str) -> bool {
    std::path::Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mif"))
}

/// The words of the file, with quoted strings as single words (quotes dropped).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                if quoted {
                    out.push(std::mem::take(&mut cur));
                }
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The coordinate system of a `CoordSys` clause's words, as PROJ, or why not.
fn coordsys(w: &[String]) -> std::result::Result<Option<Crs>, String> {
    let up: Vec<String> = w.iter().map(|s| s.to_ascii_uppercase()).collect();
    if up.first().map(String::as_str) == Some("NONEARTH") {
        return Err("a non-earth coordinate system (a drawing): coordinates are kept as they are".into());
    }
    let Some(p) = up.iter().position(|x| x == "PROJECTION") else { return Ok(None) };
    // Numbers after `Projection`, commas being separators.
    let rest = w.get(p + 1..).unwrap_or_default().join(" ");
    let parts: Vec<String> = rest.split(',').map(|s| s.trim().trim_matches('"').to_string()).collect();
    let num = |i: usize| parts.get(i).and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse::<f64>().ok());
    let ty = num(0).ok_or("a CoordSys without a projection type")? as i64;
    let datum = num(1).unwrap_or(104.0) as i64;
    if ty == 1 {
        return Ok(Some(Crs::LonLat));
    }
    // Other datums (custom ones also carry their shift parameters ahead of the projection's)
    // would place the data wrongly: it is kept as it is, and said.
    let datum_proj = match datum {
        104 | 0 => "+datum=WGS84",
        74 => "+datum=NAD83",
        62 => "+datum=NAD27",
        33 => "+ellps=GRS80",
        other => return Err(format!("MapInfo datum {other} isn't read: coordinates are kept as they are")),
    };
    // PROJ takes false eastings and northings in metres, whatever the units.
    let (unit, metres) = match parts.get(2).map(|u| u.to_ascii_lowercase()).as_deref() {
        Some("m") | None => ("+units=m", 1.0),
        Some("ft") => ("+units=ft", 0.3048),
        Some("survey ft") => ("+units=us-ft", 1200.0 / 3937.0),
        Some("km") => ("+units=km", 1000.0),
        Some(u) => return Err(format!("a CoordSys in '{u}' units isn't read: coordinates are kept as they are")),
    };
    let n = |i: usize| num(i).ok_or_else(|| format!("projection {ty} lacks its parameters"));
    let m = |i: usize| n(i).map(|v| v * metres);
    let proj = match ty {
        8 => format!("+proj=tmerc +lon_0={} +lat_0={} +k={} +x_0={} +y_0={}", n(3)?, n(4)?, n(5)?, m(6)?, m(7)?),
        3 => format!("+proj=lcc +lon_0={} +lat_0={} +lat_1={} +lat_2={} +x_0={} +y_0={}", n(3)?, n(4)?, n(5)?, n(6)?, m(7)?, m(8)?),
        9 => format!("+proj=aea +lon_0={} +lat_0={} +lat_1={} +lat_2={} +x_0={} +y_0={}", n(3)?, n(4)?, n(5)?, n(6)?, m(7)?, m(8)?),
        10 => format!("+proj=merc +lon_0={}", n(3)?),
        other => return Err(format!("MapInfo projection {other} isn't read: coordinates are kept as they are")),
    };
    let proj4 = format!("{proj} {datum_proj} {unit} +no_defs");
    let label = format!("MapInfo projection {ty}");
    Crs::custom(CustomCrs { proj4, label, geokeys: Vec::new(), doubles: Vec::new(), ascii: String::new() }).map(Some)
}

/// Even-odd point in ring.
fn inside(p: Coord, ring: &[Coord]) -> bool {
    let mut c = false;
    for w in ring.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

fn area(r: &[Coord]) -> f64 {
    r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum::<f64>().abs() / 2.0
}

/// A region's rings as polygons: a ring inside another (an odd number of others) is a hole.
pub(crate) fn region(rings: Vec<Vec<Coord>>) -> Option<Geometry> {
    let mut rings: Vec<Vec<Coord>> = rings
        .into_iter()
        .filter_map(|mut r| {
            if r.first() != r.last() {
                r.push(*r.first()?);
            }
            (r.len() >= 4).then_some(r)
        })
        .collect();
    // Past this many rings, finding holes would take too long: each ring is a polygon.
    if rings.len() > MAX_NESTED_RINGS {
        return (!rings.is_empty()).then(|| Geometry::Polygon(rings.into_iter().map(|r| vec![r]).collect()));
    }
    rings.sort_by(|a, b| area(b).total_cmp(&area(a)));
    // Each ring with its bounding box, so most tests are a box comparison.
    let mut polys: Vec<Vec<(Vec<Coord>, [f64; 4])>> = Vec::new();
    let holds = |(ring, b): &(Vec<Coord>, [f64; 4]), p: Coord| p.x >= b[0] && p.y >= b[1] && p.x <= b[2] && p.y <= b[3] && inside(p, ring);
    for r in rings {
        let p = r.first().copied().unwrap_or_default();
        let b = r.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, c| {
            [b[0].min(c.x), b[1].min(c.y), b[2].max(c.x), b[3].max(c.y)]
        });
        // Every ring so far that holds it, outer rings and holes alike: inside an odd number it
        // is a hole (of the innermost polygon holding it), inside an even number (an island in a
        // lake) a polygon of its own.
        let depth: usize = polys.iter().map(|poly| poly.iter().filter(|ring| holds(ring, p)).count()).sum();
        if depth % 2 == 1
            && let Some(host) = polys.iter_mut().rev().find(|poly| poly.first().is_some_and(|o| holds(o, p)))
        {
            host.push((r, b));
        } else {
            polys.push(vec![(r, b)]);
        }
    }
    let polys: Vec<Vec<Vec<Coord>>> = polys.into_iter().map(|poly| poly.into_iter().map(|(r, _)| r).collect()).collect();
    (!polys.is_empty()).then_some(Geometry::Polygon(polys))
}

fn ellipse(x1: f64, y1: f64, x2: f64, y2: f64) -> Geometry {
    let (cx, cy, rx, ry) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0, (x2 - x1).abs() / 2.0, (y2 - y1).abs() / 2.0);
    let ring: Vec<Coord> = (0..=72)
        .map(|i| {
            let a = f64::from(i) / 72.0 * std::f64::consts::TAU;
            Coord::new(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect();
    Geometry::Polygon(vec![vec![ring]])
}

const OBJECTS: &[&str] =
    &["POINT", "LINE", "PLINE", "REGION", "RECT", "ROUNDRECT", "ELLIPSE", "ARC", "TEXT", "NONE", "MULTIPOINT", "COLLECTION"];

/// The text of a MIF or MID file in the character set the MIF's header names: Latin-1 files
/// (`WindowsLatin1`, `ISO8859_1`, `Neutral`) are read as Windows-1252, as MapInfo writes them;
/// UTF-8 as it is.
fn decode(bytes: &[u8], charset: &str) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    if matches!(charset.to_ascii_lowercase().as_str(), "windowslatin1" | "iso8859_1" | "neutral" | "") {
        // Windows-1252's characters for 0x80–0x9F; the rest of the byte range is Latin-1.
        const HIGH: [char; 32] = [
            '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”',
            '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
        ];
        return bytes
            .iter()
            .map(|&b| if (0x80..0xa0).contains(&b) { HIGH.get(usize::from(b - 0x80)).copied().unwrap_or('?') } else { char::from(b) })
            .collect();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The character set a MIF's header names (`Charset "WindowsLatin1"`), if any.
fn charset(mif: &[u8]) -> String {
    let head = String::from_utf8_lossy(mif.get(..mif.len().min(4096)).unwrap_or(mif));
    let lower = head.to_ascii_lowercase();
    lower.find("charset").and_then(|i| head.get(i + 7..)).and_then(|rest| rest.split('"').nth(1)).map(str::to_string).unwrap_or_default()
}

/// Reads a MIF file's bytes and its MID's, decoded in the MIF's character set.
pub fn read_bytes(mif: &[u8], mid: Option<&[u8]>, name: &str) -> Result<(Dataset, ReadReport)> {
    let cs = charset(mif);
    let mid = mid.map(|m| decode(m, &cs));
    read(&decode(mif, &cs), mid.as_deref(), name)
}

/// Reads a MIF file and its MID rows (none when the `.mid` is missing).
pub fn read(mif: &str, mid: Option<&str>, name: &str) -> Result<(Dataset, ReadReport)> {
    let w = words(mif.trim_start_matches('\u{feff}'));
    let up = |i: usize| w.get(i).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let mut report = ReadReport::default();
    // The header, up to DATA.
    let mut i = 0usize;
    let mut delimiter = b'\t';
    let mut columns: Vec<(String, FieldType)> = Vec::new();
    let mut crs: Option<Crs> = None;
    let data = loop {
        let Some(word) = w.get(i) else { return Err(DataError::Format("not a MIF file (no DATA section)".into())) };
        match word.to_ascii_uppercase().as_str() {
            "DELIMITER" => {
                delimiter = w.get(i + 1).and_then(|d| d.bytes().next()).unwrap_or(b'\t');
                i += 2;
            }
            "COORDSYS" => {
                // Up to the next header keyword.
                let end = (i + 1..w.len())
                    .find(|&j| {
                        matches!(
                            up(j).as_str(),
                            "COLUMNS" | "DATA" | "TRANSFORM" | "INDEX" | "UNIQUE" | "DELIMITER" | "CHARSET" | "VERSION"
                        )
                    })
                    .unwrap_or(w.len());
                match coordsys(w.get(i + 1..end).unwrap_or_default()) {
                    Ok(c) => crs = c,
                    Err(e) => report.messages.push(e),
                }
                i = end;
            }
            "COLUMNS" => {
                let n: usize = w
                    .get(i + 1)
                    .and_then(|n| n.parse().ok())
                    .filter(|n| *n <= 1000)
                    .ok_or_else(|| DataError::Format("a bad COLUMNS count".into()))?;
                i += 2;
                for _ in 0..n {
                    let col = w.get(i).cloned().ok_or_else(|| DataError::Format("columns cut short".into()))?;
                    let ty = up(i + 1);
                    let ty = if ty.starts_with("INTEGER") || ty.starts_with("SMALLINT") || ty.starts_with("LARGEINT") {
                        FieldType::Integer
                    } else if ty.starts_with("FLOAT") || ty.starts_with("DECIMAL") {
                        FieldType::Double
                    } else {
                        FieldType::Text
                    };
                    columns.push((col, ty));
                    // A type with its width on the same line (`Char (20)`, `Decimal (10,2)`) is one or two words.
                    i += 2;
                    // Its width, `(20)`, `(10,2)` or `(10, 2)`: words up to the closing parenthesis.
                    if w.get(i).is_some_and(|x| x.starts_with('(')) {
                        let mut n = 0;
                        while let Some(x) = w.get(i)
                            && n < 4
                        {
                            i += 1;
                            n += 1;
                            if x.ends_with(')') {
                                break;
                            }
                        }
                    }
                }
            }
            "DATA" => break i + 1,
            _ => i += 1,
        }
    };
    // The objects.
    let mut geoms: Vec<Option<Geometry>> = Vec::new();
    let mut i = data;
    let num = |j: usize| w.get(j).and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite());
    let count = |j: usize| w.get(j).and_then(|s| s.parse::<usize>().ok());
    let pts = |j: usize, n: usize| -> Option<Vec<Coord>> {
        if n > MAX_COORDS {
            return None;
        }
        (0..n).map(|k| Some(Coord::new(num(j + 2 * k)?, num(j + 2 * k + 1)?))).collect()
    };
    let mut error = None;
    while i < w.len() {
        let kw = up(i);
        if !OBJECTS.contains(&kw.as_str()) {
            // Styles (Pen, Brush, Symbol, Smooth, Center…) up to the next object.
            i += 1;
            continue;
        }
        let r: Option<(Option<Geometry>, usize)> = match kw.as_str() {
            "NONE" => Some((None, i + 1)),
            "POINT" => (|| Some((Some(Geometry::Point(Coord::new(num(i + 1)?, num(i + 2)?))), i + 3)))(),
            "LINE" => (|| Some((Some(Geometry::Line(vec![pts(i + 1, 2)?])), i + 5)))(),
            "MULTIPOINT" => (|| {
                let n = count(i + 1)?;
                Some((Some(Geometry::MultiPoint(pts(i + 2, n)?)), i + 2 + 2 * n))
            })(),
            "PLINE" => (|| {
                let (sections, mut j) = if up(i + 1) == "MULTIPLE" { (count(i + 2)?, i + 3) } else { (1, i + 1) };
                // Past the cap the object isn't read (clamping would leave the reader mid-object).
                if sections > 100_000 {
                    return None;
                }
                let mut parts = Vec::new();
                for _ in 0..sections {
                    let n = count(j)?;
                    parts.push(pts(j + 1, n)?);
                    j += 1 + 2 * n;
                }
                parts.retain(|p| p.len() >= 2);
                Some(((!parts.is_empty()).then_some(Geometry::Line(parts)), j))
            })(),
            "REGION" => (|| {
                let k = count(i + 1)?;
                let mut j = i + 2;
                if k > 1_000_000 {
                    return None;
                }
                let mut rings = Vec::new();
                for _ in 0..k {
                    let n = count(j)?;
                    rings.push(pts(j + 1, n)?);
                    j += 1 + 2 * n;
                }
                Some((region(rings), j))
            })(),
            "RECT" | "ROUNDRECT" => (|| {
                let (x1, y1, x2, y2) = (num(i + 1)?, num(i + 2)?, num(i + 3)?, num(i + 4)?);
                let r = vec![Coord::new(x1, y1), Coord::new(x2, y1), Coord::new(x2, y2), Coord::new(x1, y2), Coord::new(x1, y1)];
                Some((region(vec![r]), i + 5 + usize::from(kw == "ROUNDRECT")))
            })(),
            "ELLIPSE" => (|| Some((Some(ellipse(num(i + 1)?, num(i + 2)?, num(i + 3)?, num(i + 4)?)), i + 5)))(),
            // Kept as rows without geometry.
            "ARC" => Some((None, i + 7)),
            "TEXT" => Some((None, i + 6)),
            _ => None,
        };
        match r {
            Some((g, next)) => {
                geoms.push(g);
                i = next;
            }
            None => {
                error = Some(format!("object {} ({kw}) couldn't be read; the objects after it are left out", geoms.len() + 1));
                break;
            }
        }
    }
    if let Some(e) = error {
        report.messages.push(e);
    }
    if !columns.is_empty() && mid.is_none() {
        report.messages.push("no .mid file next to the .mif: the attributes are empty".into());
    }
    // The rows, one per object.
    let mut rows: Vec<Vec<String>> = Vec::new();
    if let Some(mid) = mid {
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .delimiter(delimiter)
            .from_reader(mid.trim_start_matches('\u{feff}').as_bytes());
        // A record the reader rejects stays a row (empty), so the rows after it keep their objects.
        rows = rdr.records().map(|r| r.map(|r| r.iter().map(str::to_string).collect()).unwrap_or_default()).collect();
        if rows.len() != geoms.len() {
            report.messages.push(format!("the .mid has {} rows for {} objects", rows.len(), geoms.len()));
        }
    }
    let fields: Vec<Field> = columns.iter().map(|(n, t)| Field::new(n.clone(), *t)).collect();
    let mut ds = Dataset::new(name, GeometryType::None, fields);
    let t = match crs {
        Some(Crs::LonLat) | None => None,
        Some(c) => match c.transformer() {
            Ok(t) => Some(t),
            Err(e) => {
                report.messages.push(format!("the coordinate system: {e}; coordinates are kept as they are"));
                None
            }
        },
    };
    let mut lost = 0usize;
    for (k, g) in geoms.into_iter().enumerate() {
        let g = match (&t, g) {
            (Some(t), Some(g)) => {
                let out = g.map_coords(&|c| t.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
                let mut ok = true;
                out.for_each_coord(|c| ok &= c.is_finite());
                lost += usize::from(!ok);
                ok.then_some(out)
            }
            (_, g) => g,
        };
        let row = rows.get(k);
        let attrs = ds
            .fields
            .iter()
            .enumerate()
            .map(|(c, f)| Value::parse_source(row.and_then(|r| r.get(c)).map(String::as_str).unwrap_or(""), f.ty))
            .collect();
        ds.push_next(g, attrs);
    }
    if lost > 0 {
        report.messages.push(format!("{lost} objects couldn't be converted to longitude/latitude"));
    }
    ds.geometry_type = crate::io::dominant_type(&ds);
    Ok((ds, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIF: &str = "Version 300\nCharset \"WindowsLatin1\"\nDelimiter \",\"\nCoordSys Earth Projection 1, 104\nColumns 3\n  name Char(20)\n  pop Integer\n  area Decimal(10,2)\nData\n\nPoint 10 20\n    Symbol (35,0,12)\nPline 3\n10 20\n11 21\n12 20\n    Pen (1,2,0)\nRegion 2\n  5\n0 0\n10 0\n10 10\n0 10\n0 0\n  4\n2 2\n4 2\n3 4\n2 2\n    Pen (1,2,0)\n    Brush (2,16777215,16777215)\n    Center 5 5\nPline Multiple 2\n  2\n0 0\n1 1\n  2\n2 2\n3 3\nText \"Hello world\"\n 1 1 2 2\nNone\nRect 0 0 1 1\n";
    const MID: &str = "\"A\",1,1.5\n\"B, c\",2,2.5\n\"C\",3,3.5\n\"D\",4,4.5\n\"E\",5,5.5\n\"F\",6,6.5\n\"G\",7,7.5\n";

    #[test]
    fn integer_source_numbers_are_not_truncated_or_clamped() {
        let mif = "Version 300\nCoordSys Earth Projection 1, 104\nColumns 1\n n Integer\nData\nPoint 0 0\nPoint 1 1\nPoint 2 2\n";
        let (ds, report) = read(mif, Some("2.7\n1e30\n3\n"), "numbers").unwrap();
        assert_eq!(ds.fields[0].ty, FieldType::Integer);
        assert_eq!(ds.value(0, 0), &Value::Float(2.7));
        assert_eq!(ds.value(1, 0), &Value::Float(1e30));
        assert_eq!(ds.value(2, 0), &Value::Int(3));
        assert!(report.messages.is_empty(), "{:?}", report.messages);
    }

    #[test]
    fn widths_with_spaces_and_bad_mid_rows() {
        // `Decimal (10, 2)` is one column; a .mid record the reader rejects keeps its place.
        let mif = "Version 300\nDelimiter \",\"\nCoordSys Earth Projection 1, 104\nColumns 2\n  area Decimal (10, 2)\n  name Char (8)\nData\nPoint 1 1\nPoint 2 2\nPoint 3 3\n";
        let mid = "1.5,\"a\"\n2.5,\"b\" x\"\n3.5,\"c\"\n";
        let (ds, _) = read(mif, Some(mid), "t").unwrap();
        assert_eq!(ds.fields.len(), 2);
        assert_eq!(ds.fields[0].ty, FieldType::Double);
        assert_eq!(ds.features.len(), 3);
        assert_eq!(ds.features[2].attrs[1], crate::Value::Text("c".into()));
    }

    #[test]
    fn reads_objects_and_rows() {
        let (ds, rep) = read(MIF, Some(MID), "t").unwrap();
        assert_eq!(ds.features.len(), 7, "{:?}", rep.messages);
        assert_eq!(
            ds.fields.iter().map(|f| (f.name.as_str(), f.ty)).collect::<Vec<_>>(),
            [("name", FieldType::Text), ("pop", FieldType::Integer), ("area", FieldType::Double)]
        );
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(10.0, 20.0))));
        assert_eq!(ds.features[1].attrs[0], Value::Text("B, c".into()));
        assert!(
            matches!(&ds.features[2].geometry, Some(Geometry::Polygon(p)) if p.len() == 1 && p[0].len() == 2),
            "the inner ring is a hole"
        );
        assert!(matches!(&ds.features[3].geometry, Some(Geometry::Line(l)) if l.len() == 2));
        assert!(ds.features[4].geometry.is_none() && ds.features[5].geometry.is_none(), "text and none");
        assert!(matches!(&ds.features[6].geometry, Some(Geometry::Polygon(_))));
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
    }

    #[test]
    fn islands_and_latin1() {
        // Land, a lake in it, an island in the lake: two polygons, the land with one hole.
        let sq = |c: f64, r: f64| {
            format!("5\n{} {}\n{} {}\n{} {}\n{} {}\n{} {}\n", c - r, c - r, c + r, c - r, c + r, c + r, c - r, c + r, c - r, c - r)
        };
        let mif = format!(
            "Version 300\nCharset \"WindowsLatin1\"\nColumns 1\n  name Char(10)\nData\nRegion 3\n{}{}{}",
            sq(0.0, 10.0),
            sq(0.0, 5.0),
            sq(0.0, 2.0)
        );
        let mid = b"\"Caf\xe9 \x80\"\n";
        let (ds, _) = read_bytes(mif.as_bytes(), Some(mid), "t").unwrap();
        let Some(Geometry::Polygon(p)) = &ds.features[0].geometry else { panic!() };
        assert_eq!(p.len(), 2, "the island is a polygon of its own");
        assert_eq!(p[0].len(), 2, "the land has the lake as its hole");
        assert_eq!(ds.features[0].attrs[0], Value::Text("Café €".into()));
    }

    #[test]
    fn projected_and_broken() {
        let utm = "Version 300\nCoordSys Earth Projection 8, 104, \"m\", 15, 0, 0.9996, 500000, 0\nColumns 1\n  id Integer\nData\nPoint 500000 4649776\nPoint 500100 x\n";
        let (ds, rep) = read(utm, None, "u").unwrap();
        let Some(Geometry::Point(c)) = &ds.features[0].geometry else { panic!() };
        assert!((c.x - 15.0).abs() < 1e-6 && (c.y - 42.0).abs() < 0.01, "{c:?}");
        assert_eq!(ds.features.len(), 1);
        assert!(rep.messages.iter().any(|m| m.contains("object 2")));
        assert!(rep.messages.iter().any(|m| m.contains(".mid")));
        assert!(read("Version 300\nColumns 1\n x Integer\n", None, "x").is_err(), "no DATA");
        assert!(read("Version 300\nColumns 99999\nData\n", None, "x").is_err());
        let (_, rep) = read("CoordSys NonEarth Units \"m\" Bounds (0, 0) (10, 10)\nData\nPoint 1 1\n", None, "n").unwrap();
        assert!(rep.messages.iter().any(|m| m.contains("non-earth")));
        for cut in (0..MIF.len()).step_by(7) {
            let _ = read(MIF.get(..cut).unwrap_or(""), Some(MID), "t");
        }
        assert!(read("Data\nRegion 999999999999\n", None, "x").is_ok(), "a huge count is an error message, not an allocation");
    }

    #[test]
    fn units_and_datums() {
        // The same point in feet: the false easting is in feet too, and PROJ wants metres.
        let ft = "CoordSys Earth Projection 8, 104, \"ft\", 15, 0, 0.9996, 1640416.6667, 0\nData\nPoint 1640416.6667 15255170.6\n";
        let (ds, rep) = read(ft, None, "f").unwrap();
        let Some(Geometry::Point(c)) = &ds.features[0].geometry else { panic!("{rep:?}") };
        assert!((c.x - 15.0).abs() < 1e-6 && (c.y - 42.0).abs() < 0.01, "{c:?}");
        // A datum not read keeps the coordinates as they are, and says so.
        let custom = "CoordSys Earth Projection 8, 999, 4, 1, 2, 3, \"m\", 15, 0, 0.9996, 500000, 0\nData\nPoint 500000 4649776\n";
        let (ds, rep) = read(custom, None, "c").unwrap();
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(500000.0, 4649776.0))));
        assert!(rep.messages.iter().any(|m| m.contains("datum 999")), "{rep:?}");
    }

    #[test]
    fn many_rings_are_polygons_of_their_own() {
        let ring = |x: f64| vec![Coord::new(x, 0.0), Coord::new(x + 0.5, 0.0), Coord::new(x, 0.5), Coord::new(x, 0.0)];
        let rings: Vec<Vec<Coord>> = (0..MAX_NESTED_RINGS + 1).map(|i| ring(i as f64)).collect();
        let Some(Geometry::Polygon(p)) = region(rings) else { panic!() };
        assert_eq!(p.len(), MAX_NESTED_RINGS + 1);
        // Below the cap, a ring inside another is still its hole.
        let outer = vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(10.0, 10.0), Coord::new(0.0, 10.0), Coord::new(0.0, 0.0)];
        let Some(Geometry::Polygon(p)) = region(vec![ring(20.0), ring(2.0), outer]) else { panic!() };
        assert_eq!(p.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 1]);
    }
}
