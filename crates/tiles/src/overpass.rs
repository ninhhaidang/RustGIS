//! OpenStreetMap data for an area through an Overpass API server: the query (a preset or tag
//! filters over a box) and its answer read into points, lines and polygons (closed ways that are
//! areas, and multipolygon and boundary relations assembled from their members). Requests are
//! built and answers read here; the fetching is the caller's.

use serde_json::{Map, Value, json};

/// The public Overpass server, the default.
pub const OVERPASS: &str = "https://overpass-api.de/api/interpreter";

/// The largest area asked for, in square degrees (about 50 × 50 km at mid latitudes): the public
/// servers are shared, and a larger area is better read from an extract.
pub const MAX_AREA: f64 = 0.25;
/// How many elements an answer holds at most (`out geom N`).
pub const MAX_ELEMENTS: usize = 50_000;
/// The server's time limit, in seconds (under the downloads' own).
const TIMEOUT: u32 = 50;
/// Tag filters a query takes at most.
const MAX_FILTERS: usize = 10;
/// Columns a layer gets from its most common tags; the rest go in `other_tags`.
const MAX_COLUMNS: usize = 25;
/// Points a ring or line holds at most, and member ways a relation joins at most.
const MAX_POINTS: usize = 200_000;
const MAX_MEMBERS: usize = 5_000;

/// The presets: an id, a label and the statements they select.
pub const PRESETS: &[(&str, &str, &[&str])] = &[
    ("buildings", "Buildings", &["nwr[\"building\"]"]),
    ("roads", "Roads and paths", &["way[\"highway\"]"]),
    ("railways", "Railways", &["way[\"railway\"]"]),
    ("water", "Water", &["nwr[\"natural\"=\"water\"]", "way[\"waterway\"]", "nwr[\"water\"]"]),
    ("landuse", "Land use", &["nwr[\"landuse\"]"]),
    ("parks", "Parks and green spaces", &["nwr[\"leisure\"=\"park\"]", "nwr[\"leisure\"=\"garden\"]", "nwr[\"landuse\"=\"grass\"]"]),
    ("pois", "Points of interest", &["node[\"amenity\"]", "node[\"shop\"]", "node[\"tourism\"]"]),
    ("boundaries", "Administrative boundaries", &["relation[\"boundary\"=\"administrative\"]"]),
];

/// What to download: a box (west, south, east, north) and what is in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub bbox: [f64; 4],
    /// A preset's id, if any.
    pub preset: Option<String>,
    /// Tag filters: a key, and a value it must have (any value when none).
    pub tags: Vec<(String, Option<String>)>,
}

impl Query {
    /// A query from a preset and tag filters (`amenity=cafe, shop`); at least one of them.
    pub fn new(bbox: [f64; 4], preset: Option<&str>, tags: Option<&str>) -> Result<Query, String> {
        let [w, s, e, n] = bbox;
        let ok = |x: f64, lim: f64| x.is_finite() && (-lim..=lim).contains(&x);
        if !(ok(w, 180.0) && ok(e, 180.0) && ok(s, 90.0) && ok(n, 90.0) && w < e && s < n) {
            return Err("the box is [west, south, east, north] in longitude/latitude".into());
        }
        let area = (e - w) * (n - s);
        if area > MAX_AREA {
            return Err(format!(
                "the area is too large for Overpass ({area:.2} square degrees; at most {MAX_AREA}, about 50 × 50 km): zoom in, or use an extract (.osm.pbf)"
            ));
        }
        let preset = preset.map(str::trim).filter(|p| !p.is_empty()).map(|p| p.to_ascii_lowercase());
        if let Some(p) = &preset
            && !PRESETS.iter().any(|(id, _, _)| id == p)
        {
            let ids: Vec<&str> = PRESETS.iter().map(|(id, _, _)| *id).collect();
            return Err(format!("unknown preset '{p}' ({})", ids.join(", ")));
        }
        let tags: Vec<(String, Option<String>)> = tags
            .unwrap_or("")
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| match t.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), Some(v.trim().to_string()).filter(|v| !v.is_empty() && v != "*")),
                None => (t.to_string(), None),
            })
            .collect();
        if tags.len() > MAX_FILTERS {
            return Err(format!("at most {MAX_FILTERS} tag filters"));
        }
        if tags.iter().any(|(k, v)| k.is_empty() || k.len() > 100 || v.as_ref().is_some_and(|v| v.len() > 200)) {
            return Err("a tag filter is key or key=value".into());
        }
        if preset.is_none() && tags.is_empty() {
            return Err("give a preset or tag filters (such as amenity=cafe)".into());
        }
        Ok(Query { bbox, preset, tags })
    }

    /// The Overpass QL query.
    pub fn ql(&self) -> String {
        let [w, s, e, n] = self.bbox;
        let mut statements: Vec<String> = Vec::new();
        if let Some(p) = &self.preset
            && let Some((_, _, st)) = PRESETS.iter().find(|(id, _, _)| id == p)
        {
            statements.extend(st.iter().map(|s| format!("{s};")));
        }
        for (k, v) in &self.tags {
            statements.push(match v {
                Some(v) => format!("nwr[\"{}\"=\"{}\"];", quote(k), quote(v)),
                None => format!("nwr[\"{}\"];", quote(k)),
            });
        }
        format!("[out:json][timeout:{TIMEOUT}][bbox:{s},{w},{n},{e}];({});out geom {MAX_ELEMENTS};", statements.join(""))
    }

    /// The request to `server` (the public one when none).
    pub fn url(&self, server: Option<&str>) -> Result<String, String> {
        let server = server.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(OVERPASS);
        crate::address::check(server, None)?;
        let base = server.split(['?', '#']).next().unwrap_or(server);
        Ok(format!("{base}?data={}", encode(&self.ql())))
    }

    /// A name for the layers: the preset's label, or the filters.
    pub fn title(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some((_, label, _)) = self.preset.as_ref().and_then(|p| PRESETS.iter().find(|(id, _, _)| id == p)) {
            parts.push((*label).to_string());
        }
        parts.extend(self.tags.iter().map(|(k, v)| match v {
            Some(v) => format!("{k}={v}"),
            None => k.clone(),
        }));
        format!("OSM {}", parts.join(", "))
    }
}

/// A string inside Overpass QL's double quotes.
fn quote(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().replace('\\', "\\\\").replace('"', "\\\"")
}

/// Percent-encodes a query parameter.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// An answer read: GeoJSON FeatureCollections of points, lines and polygons, and what was left
/// out.
#[derive(Debug, Default)]
pub struct Answer {
    pub points: Value,
    pub lines: Value,
    pub polygons: Value,
    pub notes: Vec<String>,
}

type Ring = Vec<[f64; 2]>;

/// Reads an Overpass JSON answer.
pub fn parse(bytes: &[u8]) -> Result<Answer, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| {
        // Overpass answers errors as HTML or XML.
        let head = String::from_utf8_lossy(bytes.get(..bytes.len().min(2000)).unwrap_or(bytes)).to_string();
        let msg = head
            .split("<strong")
            .nth(1)
            .and_then(|s| s.split_once('>'))
            .map(|(_, r)| r.split('<').next().unwrap_or("").trim().to_string())
            .filter(|m| !m.is_empty());
        match msg {
            Some(m) => format!("the Overpass server answered: {m}"),
            None => "the Overpass server's answer isn't JSON".to_string(),
        }
    })?;
    let elements = v.get("elements").and_then(Value::as_array).ok_or("the Overpass answer has no elements")?;
    let remark = v.get("remark").and_then(Value::as_str).map(str::to_string);
    if elements.is_empty()
        && let Some(r) = &remark
    {
        return Err(format!("the Overpass server: {r}"));
    }
    let mut points = Vec::new();
    let mut lines = Vec::new();
    let mut polygons = Vec::new();
    let (mut other_relations, mut broken, mut stray_inner) = (0usize, 0usize, 0usize);
    for el in elements.iter().take(MAX_ELEMENTS) {
        let tags: Vec<(String, String)> = el
            .get("tags")
            .and_then(Value::as_object)
            .map(|t| {
                t.iter().filter_map(|(k, v)| Some((k.chars().take(100).collect(), v.as_str()?.chars().take(1000).collect()))).collect()
            })
            .unwrap_or_default();
        let id = el.get("id").and_then(Value::as_i64).unwrap_or(0);
        match el.get("type").and_then(Value::as_str) {
            Some("node") => {
                if tags.is_empty() {
                    continue;
                }
                let (Some(lon), Some(lat)) = (num(el.get("lon")), num(el.get("lat"))) else { continue };
                points.push(("node", id, tags, json!({"type": "Point", "coordinates": [lon, lat]})));
            }
            Some("way") => {
                let pts = coords(el.get("geometry"));
                if pts.len() < 2 {
                    broken += 1;
                    continue;
                }
                let closed = pts.len() >= 4 && pts.first() == pts.last();
                if closed && rustgis_data::osm::closed_way_is_area(&tags) {
                    polygons.push(("way", id, tags, json!({"type": "Polygon", "coordinates": [pts]})));
                } else {
                    lines.push(("way", id, tags, json!({"type": "LineString", "coordinates": pts})));
                }
            }
            Some("relation") => {
                let kind = tags.iter().find(|(k, _)| k == "type").map(|(_, v)| v.as_str());
                if !matches!(kind, Some("multipolygon") | Some("boundary")) {
                    other_relations += 1;
                    continue;
                }
                match multipolygon(el.get("members")) {
                    Some((polys, stray)) if !polys.is_empty() => {
                        stray_inner += stray;
                        polygons.push(("relation", id, tags, json!({"type": "MultiPolygon", "coordinates": polys})))
                    }
                    _ => broken += 1,
                }
            }
            _ => {}
        }
    }
    let mut notes = Vec::new();
    if elements.len() >= MAX_ELEMENTS {
        notes.push(format!("the answer stopped at {MAX_ELEMENTS} elements: zoom in for the rest"));
    }
    if let Some(r) = remark {
        notes.push(format!("the Overpass server: {r}"));
    }
    if other_relations > 0 {
        notes.push(format!("{other_relations} relations other than multipolygons and boundaries (routes and such) were left out"));
    }
    if broken > 0 {
        notes.push(format!("{broken} ways or relations without a whole geometry were left out"));
    }
    if stray_inner > 0 {
        notes.push(format!("{stray_inner} inner rings (holes) outside every outer ring of their relation were left out"));
    }
    Ok(Answer { points: collection(points), lines: collection(lines), polygons: collection(polygons), notes })
}

fn num(v: Option<&Value>) -> Option<f64> {
    v?.as_f64().filter(|x| x.is_finite())
}

/// A way's (or member's) points as [lon, lat], skipping missing ones.
fn coords(g: Option<&Value>) -> Ring {
    g.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_POINTS)
        .filter_map(|p| {
            let (lon, lat) = (num(p.get("lon"))?, num(p.get("lat"))?);
            ((-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)).then_some([lon, lat])
        })
        .collect()
}

/// A tag key or value in other_tags' `"key"=>"value"` form: backslashes, then quotes, escaped.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A multipolygon relation's polygons: its outer and inner member ways joined into rings, each
/// inner ring in the outer one holding it; and how many inner rings no outer ring holds (left
/// out).
fn multipolygon(members: Option<&Value>) -> Option<(Vec<Vec<Ring>>, usize)> {
    let (mut outer, mut inner): (Vec<Ring>, Vec<Ring>) = (Vec::new(), Vec::new());
    for m in members?.as_array()?.iter().take(MAX_MEMBERS) {
        if m.get("type").and_then(Value::as_str) != Some("way") {
            continue;
        }
        let pts = coords(m.get("geometry"));
        if pts.len() < 2 {
            continue;
        }
        match m.get("role").and_then(Value::as_str) {
            Some("inner") => inner.push(pts),
            _ => outer.push(pts),
        }
    }
    let outer = join(outer);
    if outer.is_empty() {
        return None;
    }
    let mut polys: Vec<Vec<Ring>> = outer.into_iter().map(|r| vec![r]).collect();
    let mut stray = 0usize;
    for ring in join(inner) {
        // An inner ring may touch its outer ring at a vertex, which tests either way: the first of
        // a few vertices inside an outer ring decides.
        let holder = ring.iter().take(8).find_map(|p| polys.iter().position(|poly| poly.first().is_some_and(|o| inside(*p, o))));
        match holder.and_then(|i| polys.get_mut(i)) {
            Some(poly) => poly.push(ring),
            None => stray += 1,
        }
    }
    Some((polys, stray))
}

/// Joins ways end to end into closed rings; a chain that can't be closed is dropped.
fn join(mut ways: Vec<Ring>) -> Vec<Ring> {
    let mut rings = Vec::new();
    while let Some(mut ring) = ways.pop() {
        while ring.first() != ring.last() && ring.len() < MAX_POINTS {
            let Some(end) = ring.last().copied() else { break };
            let Some(i) = ways.iter().position(|w| w.first() == Some(&end) || w.last() == Some(&end)) else { break };
            let mut next = ways.swap_remove(i);
            if next.first() != Some(&end) {
                next.reverse();
            }
            ring.extend(next.into_iter().skip(1));
        }
        if ring.len() >= 4 && ring.first() == ring.last() {
            rings.push(ring);
        }
    }
    rings
}

/// Whether a point is inside a ring (even-odd).
fn inside(p: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut c = false;
    let mut j = ring.len().saturating_sub(1);
    for (i, a) in ring.iter().enumerate() {
        let Some(b) = ring.get(j) else { break };
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
        j = i;
    }
    c
}

/// A FeatureCollection: the most common tags as properties, the rest in `other_tags`.
fn collection(features: Vec<(&str, i64, Vec<(String, String)>, Value)>) -> Value {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (_, _, tags, _) in &features {
        for (k, _) in tags {
            match counts.iter().position(|(c, _)| c == k) {
                Some(i) => {
                    if let Some((_, n)) = counts.get_mut(i) {
                        *n += 1;
                    }
                }
                None if counts.len() < 10_000 => counts.push((k.clone(), 1)),
                None => {}
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // `osm_type` and `osm_id` are the columns RustGIS adds; a tag of that name stays in other_tags.
    let columns: Vec<&str> =
        counts.iter().map(|(k, _)| k.as_str()).filter(|k| !matches!(*k, "osm_type" | "osm_id")).take(MAX_COLUMNS).collect();
    let feats: Vec<Value> = features
        .iter()
        .map(|(kind, id, tags, geom)| {
            let mut props = Map::new();
            props.insert("osm_type".into(), json!(kind));
            props.insert("osm_id".into(), json!(id));
            for c in &columns {
                props.insert((*c).to_string(), tags.iter().find(|(k, _)| k == c).map(|(_, v)| json!(v)).unwrap_or(Value::Null));
            }
            let rest: Vec<String> = tags
                .iter()
                .filter(|(k, _)| !columns.contains(&k.as_str()))
                .map(|(k, v)| format!("\"{}\"=>\"{}\"", escape(k), escape(v)))
                .collect();
            if !rest.is_empty() {
                props.insert("other_tags".into(), json!(rest.join(",")));
            }
            json!({"type": "Feature", "properties": props, "geometry": geom})
        })
        .collect();
    json!({"type": "FeatureCollection", "features": feats})
}

impl Answer {
    /// The layers to add: a name suffix and its FeatureCollection, for those holding features.
    pub fn layers(&self) -> Vec<(&'static str, &Value)> {
        [("points", &self.points), ("lines", &self.lines), ("polygons", &self.polygons)]
            .into_iter()
            .filter(|(_, fc)| fc["features"].as_array().is_some_and(|f| !f.is_empty()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries() {
        let q = Query::new([7.44, 46.94, 7.45, 46.95], Some("Buildings"), Some("amenity=cafe, shop")).unwrap();
        assert_eq!(
            q.ql(),
            "[out:json][timeout:50][bbox:46.94,7.44,46.95,7.45];(nwr[\"building\"];nwr[\"amenity\"=\"cafe\"];nwr[\"shop\"];);out geom 50000;"
        );
        assert!(q.url(None).unwrap().starts_with("https://overpass-api.de/api/interpreter?data=%5Bout%3Ajson%5D"));
        assert_eq!(q.title(), "OSM Buildings, amenity=cafe, shop");
        // Quotes can't break out of a filter.
        let q = Query::new([0.0, 0.0, 0.1, 0.1], None, Some("name=a\"](\\")).unwrap();
        assert!(q.ql().contains("(nwr[\"name\"=\"a\\\"](\\\\\"];)"), "{}", q.ql());
        assert!(Query::new([0.0, 0.0, 1.0, 1.0], Some("roads"), None).unwrap_err().contains("too large"));
        assert!(Query::new([0.0, 0.0, 0.1, 0.1], None, None).is_err());
        assert!(Query::new([0.0, 0.0, 0.1, 0.1], Some("everything"), None).is_err());
        assert!(Query::new([1.0, 0.0, 0.0, 0.1], Some("roads"), None).is_err());
        assert!(q.url(Some("file:///etc/passwd")).is_err());
    }

    #[test]
    fn answers() {
        let a = br#"{"elements": [
            {"type": "node", "id": 1, "lat": 46.94, "lon": 7.44, "tags": {"amenity": "cafe", "name": "Kaffee"}},
            {"type": "node", "id": 2, "lat": 46.94, "lon": 7.44},
            {"type": "way", "id": 3, "tags": {"building": "yes"}, "geometry": [{"lat": 0, "lon": 0}, {"lat": 0, "lon": 1}, {"lat": 1, "lon": 1}, {"lat": 0, "lon": 0}]},
            {"type": "way", "id": 4, "tags": {"highway": "residential"}, "geometry": [{"lat": 0, "lon": 0}, {"lat": 0, "lon": 1}, {"lat": 1, "lon": 1}, {"lat": 0, "lon": 0}]},
            {"type": "way", "id": 5, "tags": {"highway": "path"}, "geometry": [{"lat": 0, "lon": 0}]},
            {"type": "way", "id": 8, "tags": {"destroyed:building": "house"}, "geometry": [{"lat": 0, "lon": 0}, {"lat": 0, "lon": 1}, {"lat": 1, "lon": 1}, {"lat": 0, "lon": 0}]},
            {"type": "relation", "id": 6, "tags": {"type": "multipolygon", "landuse": "forest"}, "members": [
                {"type": "way", "role": "outer", "geometry": [{"lat": 0, "lon": 0}, {"lat": 0, "lon": 10}]},
                {"type": "way", "role": "outer", "geometry": [{"lat": 10, "lon": 10}, {"lat": 0, "lon": 10}]},
                {"type": "way", "role": "outer", "geometry": [{"lat": 10, "lon": 10}, {"lat": 0, "lon": 0}]},
                {"type": "way", "role": "inner", "geometry": [{"lat": 2, "lon": 4}, {"lat": 2, "lon": 5}, {"lat": 3, "lon": 5}, {"lat": 2, "lon": 4}]}]},
            {"type": "relation", "id": 7, "tags": {"type": "route"}, "members": []}
        ]}"#;
        let r = parse(a).unwrap();
        let n = |fc: &Value| fc["features"].as_array().map(Vec::len).unwrap_or(0);
        assert_eq!((n(&r.points), n(&r.lines), n(&r.polygons)), (1, 1, 3), "a destroyed building is a polygon");
        assert_eq!(r.points["features"][0]["properties"]["name"], "Kaffee");
        let mp = &r.polygons["features"][2]["geometry"];
        assert_eq!(mp["type"], "MultiPolygon");
        assert_eq!(mp["coordinates"][0].as_array().map(Vec::len), Some(2), "the inner ring is in the outer polygon");
        assert!(r.notes.iter().any(|m| m.contains("1 relations other")) && r.notes.iter().any(|m| m.contains("1 ways or relations")));
        assert_eq!(r.layers().len(), 3);
        // A server's error, as HTML; a timeout remark.
        assert!(
            parse(b"<html><body><p><strong style=\"color:#FF0000\">Error</strong>: runtime error</p></body></html>")
                .unwrap_err()
                .contains("Error")
        );
        assert!(parse(br#"{"elements": [], "remark": "runtime error: Query timed out"}"#).unwrap_err().contains("timed out"));
        for junk in [&b"{}"[..], b"[", br#"{"elements": [{"type": "way", "geometry": 7}, {"type": "node", "lat": "x"}]}"#] {
            let _ = parse(junk);
        }
    }

    #[test]
    fn many_columns() {
        let tags: Map<String, Value> = (0..40).map(|i| (format!("k{i:02}"), json!("v"))).collect();
        let a = json!({"elements": [{"type": "node", "id": 1, "lat": 1, "lon": 1, "tags": tags}]});
        let r = parse(a.to_string().as_bytes()).unwrap();
        let p = &r.points["features"][0]["properties"];
        assert_eq!(p.as_object().map(Map::len), Some(2 + MAX_COLUMNS + 1));
        assert!(p["other_tags"].as_str().is_some_and(|t| t.contains("\"k39\"=>\"v\"")));
    }

    #[test]
    fn escapes_reserved_names_and_stray_holes() {
        // A backslash is escaped before a quote; a tag named osm_id stays out of the columns.
        let a = json!({"elements": [{"type": "node", "id": 7, "lat": 1, "lon": 1, "tags": {"name": "x", "osm_id": "999", "note": "a\\\"b\\"}}]});
        let r = parse(a.to_string().as_bytes()).unwrap();
        let p = &r.points["features"][0]["properties"];
        assert_eq!(p["osm_id"], json!(7));
        let other = p["other_tags"].as_str().unwrap();
        assert!(other.contains(r#""osm_id"=>"999""#), "{other}");
        assert_eq!(escape("a\"b\\"), r#"a\"b\\"#);
        // A hole outside its relation's outer ring is left out, and said.
        let way = |pts: &[[f64; 2]], role: &str| json!({"type": "way", "role": role, "geometry": pts.iter().map(|[x, y]| json!({"lon": x, "lat": y})).collect::<Vec<_>>()});
        let outer = way(&[[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0], [0.0, 0.0]], "outer");
        let hole = way(&[[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 1.0]], "inner");
        let stray = way(&[[9.0, 9.0], [10.0, 9.0], [10.0, 10.0], [9.0, 9.0]], "inner");
        let rel = json!({"elements": [{"type": "relation", "id": 1, "tags": {"type": "multipolygon", "landuse": "x"}, "members": [outer, hole, stray]}]});
        let r = parse(rel.to_string().as_bytes()).unwrap();
        assert_eq!(r.polygons["features"][0]["geometry"]["coordinates"][0].as_array().map(Vec::len), Some(2));
        assert!(r.notes.iter().any(|n| n.contains("1 inner rings")), "{:?}", r.notes);
    }
}
