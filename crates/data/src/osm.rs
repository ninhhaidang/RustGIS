//! OpenStreetMap PBF (`.osm.pbf`): nodes, ways and relations, read in pure Rust (the browser
//! too) into the layers GIS software shows — points (tagged nodes), lines (open ways and closed
//! ways that aren't areas) and multipolygons (closed ways that are areas, and multipolygon and
//! boundary relations, their member ways joined into rings). Common keys get their own fields;
//! the rest are kept in `other_tags`.
//!
//! The file is hostile: every length is checked against the bytes there, blobs inflate to at most
//! the format's 32 MiB, and the nodes kept are capped (large extracts belong to GDAL or a
//! database). Node coordinates are kept at OSM's own precision (1e-7°) to save memory.

use std::collections::HashMap;

use rustgis_geom::Coord;

use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// The most nodes kept (16 bytes each).
const MAX_NODES: usize = if cfg!(target_arch = "wasm32") { 4_000_000 } else { 40_000_000 };
/// A blob inflates to at most this (the format's own limit).
const MAX_BLOB: usize = 32 << 20;
/// The most members of a relation, and ways in one multipolygon, looked at.
const MAX_MEMBERS: usize = 100_000;
/// The most ways, node references on ways, and relations kept (beside [`MAX_NODES`]).
const MAX_WAYS: usize = MAX_NODES / 4;
const MAX_REFS: usize = MAX_NODES * 2;
const MAX_RELATIONS: usize = MAX_NODES / 40;

/// Keys with a field of their own (as GDAL's OSM driver gives them).
const KEYS: &[&str] = &[
    "name",
    "highway",
    "waterway",
    "railway",
    "aerialway",
    "barrier",
    "man_made",
    "natural",
    "landuse",
    "building",
    "amenity",
    "leisure",
    "shop",
    "tourism",
    "place",
    "boundary",
    "admin_level",
    "type",
    "ref",
];
/// Keys that make a closed way an area.
const AREA_KEYS: &[&str] = &[
    "aeroway",
    "amenity",
    "boundary",
    "building",
    "craft",
    "geological",
    "historic",
    "landuse",
    "leisure",
    "military",
    "natural",
    "office",
    "place",
    "shop",
    "sport",
    "tourism",
    "water",
];

/// Whether bytes start like an OSM PBF file (its first block an `OSMHeader`), not a vector tile.
pub fn looks_like_osm_pbf(bytes: &[u8]) -> bool {
    bytes.get(4..64).is_some_and(|h| h.windows(9).any(|w| w == b"OSMHeader"))
}

fn bad(what: &str) -> DataError {
    DataError::Format(format!("not a readable OSM PBF file: {what}"))
}

/// A protocol buffer message's fields, read one at a time.
struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Other,
}

impl<'a> Pb<'a> {
    fn new(b: &'a [u8]) -> Self {
        Pb { b, i: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// The next field: its number and value; `None` at the end, `Err` when the bytes are bad.
    fn next(&mut self) -> std::result::Result<Option<(u64, Wire<'a>)>, DataError> {
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let key = self.varint().ok_or_else(|| bad("a field key"))?;
        let v = match key & 7 {
            0 => Wire::Varint(self.varint().ok_or_else(|| bad("a varint"))?),
            1 => {
                self.i = self.i.checked_add(8).filter(|&i| i <= self.b.len()).ok_or_else(|| bad("a 64-bit field"))?;
                Wire::Other
            }
            2 => {
                let n = usize::try_from(self.varint().ok_or_else(|| bad("a length"))?).map_err(|_| bad("a length"))?;
                let end = self.i.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| bad("a length past the end"))?;
                let s = self.b.get(self.i..end).ok_or_else(|| bad("a length past the end"))?;
                self.i = end;
                Wire::Bytes(s)
            }
            5 => {
                self.i = self.i.checked_add(4).filter(|&i| i <= self.b.len()).ok_or_else(|| bad("a 32-bit field"))?;
                Wire::Other
            }
            _ => return Err(bad("a field of an unknown wire type")),
        };
        Ok(Some((key >> 3, v)))
    }
}

fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// The most values a packed field holds: writers put about 8,000 entities in a block, and a way
/// has at most 2,000 nodes, so this is far past real files and keeps a hostile block of one-byte
/// varints from growing a vector to its whole 32 MiB.
const MAX_PACKED: usize = 1 << 20;

/// The varints of a packed field.
fn packed(b: &[u8]) -> Result<Vec<u64>> {
    let mut p = Pb::new(b);
    let mut out = Vec::new();
    while let Some(v) = p.varint() {
        if out.len() >= MAX_PACKED {
            return Err(DataError::Format(format!("an OSM PBF block holds a list of more than {MAX_PACKED} values")));
        }
        out.push(v);
    }
    Ok(out)
}

/// Delta-coded signed values of a packed field, summed.
fn deltas(b: &[u8]) -> Result<Vec<i64>> {
    let mut acc = 0i64;
    Ok(packed(b)?
        .into_iter()
        .map(|v| {
            acc = acc.wrapping_add(zigzag(v));
            acc
        })
        .collect())
}

type Tags = Vec<(String, String)>;

/// Editing bookkeeping, dropped as GDAL drops it: a node with only these is no point of interest.
fn ignored(key: &str) -> bool {
    matches!(key, "created_by" | "converted_by" | "source" | "time" | "ele" | "note" | "todo" | "fixme" | "FIXME")
        || key.starts_with("openGeoDB:")
}

fn tags(strings: &[String], keys: &[u64], vals: &[u64]) -> Tags {
    keys.iter()
        .zip(vals)
        .filter_map(|(k, v)| Some((strings.get(usize::try_from(*k).ok()?)?.clone(), strings.get(usize::try_from(*v).ok()?)?.clone())))
        .filter(|(k, _)| !ignored(k))
        .collect()
}

fn tag<'t>(t: &'t Tags, k: &str) -> Option<&'t str> {
    t.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
}

/// What was read: nodes (sorted by id), tagged nodes, ways, and relations.
#[derive(Default)]
struct Osm {
    nodes: Vec<(i64, i32, i32)>,
    points: Vec<(i64, Coord, Tags)>,
    /// (id, node ids, tags, the nodes' places when the file stores them on its ways).
    ways: Vec<(i64, Vec<i64>, Tags, Option<Vec<Coord>>)>,
    refs: usize,
    /// (id, members (type, id, role), tags).
    relations: Vec<(i64, Vec<(u64, i64, String)>, Tags)>,
    capped: bool,
}

/// Degrees from OSM's fixed point (1e-7°), and back.
fn to_fixed(nano: i64) -> i32 {
    (nano / 100).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn block(osm: &mut Osm, data: &[u8]) -> Result<()> {
    let mut strings: Vec<String> = Vec::new();
    let mut groups: Vec<&[u8]> = Vec::new();
    let (mut gran, mut lat_off, mut lon_off) = (100i64, 0i64, 0i64);
    let mut p = Pb::new(data);
    while let Some((f, v)) = p.next()? {
        match (f, v) {
            (1, Wire::Bytes(st)) => {
                let mut s = Pb::new(st);
                while let Some((sf, sv)) = s.next()? {
                    if let (1, Wire::Bytes(b)) = (sf, sv) {
                        strings.push(String::from_utf8_lossy(b).into_owned());
                    }
                }
            }
            (2, Wire::Bytes(g)) => groups.push(g),
            (17, Wire::Varint(v)) => gran = i64::try_from(v).unwrap_or(100).max(1),
            (19, Wire::Varint(v)) => lat_off = v as i64,
            (20, Wire::Varint(v)) => lon_off = v as i64,
            _ => {}
        }
    }
    let nano = |off: i64, v: i64| off.saturating_add(gran.saturating_mul(v));
    let keep = |osm: &mut Osm, id: i64, lat: i64, lon: i64, t: Tags| {
        let (la, lo) = (nano(lat_off, lat), nano(lon_off, lon));
        if osm.nodes.len() < MAX_NODES {
            osm.nodes.push((id, to_fixed(lo), to_fixed(la)));
        } else {
            osm.capped = true;
        }
        if !t.is_empty() {
            if osm.points.len() < MAX_NODES {
                osm.points.push((id, Coord::new(lo as f64 * 1e-9, la as f64 * 1e-9), t));
            } else {
                osm.capped = true;
            }
        }
    };
    for g in groups {
        let mut gp = Pb::new(g);
        while let Some((f, v)) = gp.next()? {
            let Wire::Bytes(m) = v else { continue };
            match f {
                // A node.
                1 => {
                    let (mut id, mut lat, mut lon, mut k, mut vv) = (0i64, 0i64, 0i64, Vec::new(), Vec::new());
                    let mut n = Pb::new(m);
                    while let Some((nf, nv)) = n.next()? {
                        match (nf, nv) {
                            (1, Wire::Varint(x)) => id = zigzag(x),
                            (2, Wire::Bytes(b)) => k = packed(b)?,
                            (3, Wire::Bytes(b)) => vv = packed(b)?,
                            (8, Wire::Varint(x)) => lat = zigzag(x),
                            (9, Wire::Varint(x)) => lon = zigzag(x),
                            _ => {}
                        }
                    }
                    keep(osm, id, lat, lon, tags(&strings, &k, &vv));
                }
                // Dense nodes.
                2 => {
                    let (mut ids, mut lats, mut lons, mut kv) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                    let mut d = Pb::new(m);
                    while let Some((df, dv)) = d.next()? {
                        match (df, dv) {
                            (1, Wire::Bytes(b)) => ids = deltas(b)?,
                            (8, Wire::Bytes(b)) => lats = deltas(b)?,
                            (9, Wire::Bytes(b)) => lons = deltas(b)?,
                            (10, Wire::Bytes(b)) => kv = packed(b)?,
                            _ => {}
                        }
                    }
                    let mut kvi = kv.into_iter();
                    for ((id, lat), lon) in ids.into_iter().zip(lats).zip(lons) {
                        let mut t = Vec::new();
                        // Key, value pairs up to a 0.
                        while let Some(k) = kvi.next() {
                            if k == 0 {
                                break;
                            }
                            let Some(v) = kvi.next() else { break };
                            let at = |i: u64| usize::try_from(i).ok().and_then(|i| strings.get(i));
                            if let (Some(k), Some(v)) = (at(k), at(v))
                                && !ignored(k)
                            {
                                t.push((k.clone(), v.clone()));
                            }
                        }
                        keep(osm, id, lat, lon, t);
                    }
                }
                // A way.
                3 => {
                    let (mut id, mut k, mut vv, mut refs, mut lats, mut lons) =
                        (0i64, Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                    let mut w = Pb::new(m);
                    while let Some((wf, wv)) = w.next()? {
                        match (wf, wv) {
                            (1, Wire::Varint(x)) => id = x as i64,
                            (2, Wire::Bytes(b)) => k = packed(b)?,
                            (3, Wire::Bytes(b)) => vv = packed(b)?,
                            (8, Wire::Bytes(b)) => refs = deltas(b)?,
                            // LocationsOnWays: each node's place, on the way itself.
                            (9, Wire::Bytes(b)) => lats = deltas(b)?,
                            (10, Wire::Bytes(b)) => lons = deltas(b)?,
                            _ => {}
                        }
                    }
                    if osm.ways.len() >= MAX_WAYS || osm.refs + refs.len() > MAX_REFS {
                        osm.capped = true;
                        continue;
                    }
                    let places = (!lats.is_empty() && lats.len() == refs.len() && lons.len() == refs.len()).then(|| {
                        lats.iter()
                            .zip(&lons)
                            .map(|(la, lo)| Coord::new(nano(lon_off, *lo) as f64 * 1e-9, nano(lat_off, *la) as f64 * 1e-9))
                            .collect()
                    });
                    osm.refs += refs.len();
                    osm.ways.push((id, refs, tags(&strings, &k, &vv), places));
                }
                // A relation.
                4 => {
                    let (mut id, mut k, mut vv, mut roles, mut mems, mut types) =
                        (0i64, Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                    let mut r = Pb::new(m);
                    while let Some((rf, rv)) = r.next()? {
                        match (rf, rv) {
                            (1, Wire::Varint(x)) => id = x as i64,
                            (2, Wire::Bytes(b)) => k = packed(b)?,
                            (3, Wire::Bytes(b)) => vv = packed(b)?,
                            (8, Wire::Bytes(b)) => roles = packed(b)?,
                            (9, Wire::Bytes(b)) => mems = deltas(b)?,
                            (10, Wire::Bytes(b)) => types = packed(b)?,
                            _ => {}
                        }
                    }
                    let t = tags(&strings, &k, &vv);
                    // Only the relations drawn are kept.
                    if osm.relations.len() >= MAX_RELATIONS {
                        osm.capped = true;
                    } else if matches!(tag(&t, "type"), Some("multipolygon" | "boundary")) {
                        let members = mems
                            .into_iter()
                            .zip(types)
                            .zip(roles)
                            .take(MAX_MEMBERS)
                            .map(|((m, ty), role)| {
                                (ty, m, usize::try_from(role).ok().and_then(|r| strings.get(r)).cloned().unwrap_or_default())
                            })
                            .collect();
                        osm.relations.push((id, members, t));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Reads the file's blocks.
fn blocks(bytes: &[u8]) -> Result<Osm> {
    let mut osm = Osm::default();
    let mut i = 0usize;
    let mut saw_header = false;
    while i < bytes.len() {
        let len =
            bytes.get(i..i + 4).and_then(|s| s.try_into().ok()).map(u32::from_be_bytes).ok_or_else(|| bad("a block length"))? as usize;
        if len > 64 * 1024 {
            return Err(bad("a block header too large"));
        }
        i += 4;
        let header = bytes.get(i..i + len).ok_or_else(|| bad("a block header past the end"))?;
        i += len;
        let (mut kind, mut size) = (String::new(), 0usize);
        let mut h = Pb::new(header);
        while let Some((f, v)) = h.next()? {
            match (f, v) {
                (1, Wire::Bytes(b)) => kind = String::from_utf8_lossy(b).into_owned(),
                (3, Wire::Varint(n)) => size = usize::try_from(n).map_err(|_| bad("a blob size"))?,
                _ => {}
            }
        }
        let blob = bytes.get(i..i.checked_add(size).ok_or_else(|| bad("a blob size"))?).ok_or_else(|| bad("a blob past the end"))?;
        i += size;
        // The blob's data, inflated.
        let mut data: Option<Vec<u8>> = None;
        let mut b = Pb::new(blob);
        while let Some((f, v)) = b.next()? {
            match (f, v) {
                (1, Wire::Bytes(raw)) => data = Some(raw.to_vec()),
                (3, Wire::Bytes(z)) => {
                    data = Some(
                        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(z, MAX_BLOB)
                            .map_err(|_| bad("a damaged or oversized compressed block"))?,
                    )
                }
                (4 | 6 | 7, _) => return Err(DataError::Unsupported("OSM PBF blocks compressed with LZMA, LZ4 or Zstandard".into())),
                _ => {}
            }
        }
        let data = data.ok_or_else(|| bad("an empty block"))?;
        match kind.as_str() {
            "OSMHeader" => {
                saw_header = true;
                let mut p = Pb::new(&data);
                while let Some((f, v)) = p.next()? {
                    if let (4, Wire::Bytes(feat)) = (f, v) {
                        let feat = String::from_utf8_lossy(feat);
                        // The format asks readers to refuse required features they don't know (they can
                        // change how the blocks decode); these don't change it for what is read here
                        // (history files repeat objects, which are read as they come).
                        if !matches!(
                            feat.as_ref(),
                            "OsmSchema-V0.6"
                                | "DenseNodes"
                                | "Sort.Type_then_ID"
                                | "LocationsOnWays"
                                | "HistoricalInformation"
                                | "Has_Metadata"
                        ) {
                            return Err(DataError::Unsupported(format!("OSM PBF feature '{feat}'")));
                        }
                    }
                }
            }
            "OSMData" => block(&mut osm, &data)?,
            _ => {}
        }
    }
    if !saw_header {
        return Err(bad("no OSMHeader block"));
    }
    Ok(osm)
}

/// Joins ways (as node-id lists) into closed rings by their ends.
fn join_rings(ways: Vec<Vec<i64>>) -> Vec<Vec<i64>> {
    // The ways by their end nodes, so each join is a lookup. A way taken stays listed until a
    // lookup passes it, which drops it: each entry is passed once.
    let mut ends: HashMap<i64, Vec<usize>> = HashMap::new();
    for (k, w) in ways.iter().enumerate() {
        if let (Some(&a), Some(&b)) = (w.first(), w.last()) {
            ends.entry(a).or_default().push(k);
            if b != a {
                ends.entry(b).or_default().push(k);
            }
        }
    }
    let mut ways: Vec<Option<Vec<i64>>> = ways.into_iter().map(Some).collect();
    let mut rings = Vec::new();
    for start in 0..ways.len() {
        let Some(mut cur) = ways.get_mut(start).and_then(Option::take) else { continue };
        let mut guard = 0;
        while cur.first() != cur.last() && guard < MAX_MEMBERS {
            guard += 1;
            let Some(&end) = cur.last() else { break };
            let Some(list) = ends.get_mut(&end) else { break };
            let free = list.iter().position(|&k| ways.get(k).is_some_and(Option::is_some)).unwrap_or(list.len());
            list.drain(..free);
            let Some(mut next) = list.first().and_then(|&k| ways.get_mut(k)).and_then(Option::take) else { break };
            if next.first() != Some(&end) {
                next.reverse();
            }
            cur.extend(next.into_iter().skip(1));
        }
        if cur.len() >= 4 && cur.first() == cur.last() {
            rings.push(cur);
        }
    }
    rings
}

/// A ring's bounding box: west, south, east, north.
fn bbox(ring: &[Coord]) -> [f64; 4] {
    ring.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, c| {
        [b[0].min(c.x), b[1].min(c.y), b[2].max(c.x), b[3].max(c.y)]
    })
}

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

/// Reads an OSM PBF file into its points, lines and multipolygons layers.
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Vec<(Dataset, ReadReport)>> {
    let stem = std::path::Path::new(file_name).file_name().and_then(|s| s.to_str()).unwrap_or("osm");
    let stem = stem.trim_end_matches(".pbf").trim_end_matches(".osm").to_string();
    let mut osm = blocks(bytes)?;
    osm.nodes.sort_unstable_by_key(|n| n.0);
    let node = |id: i64| -> Option<Coord> {
        let i = osm.nodes.binary_search_by_key(&id, |n| n.0).ok()?;
        let (_, x, y) = *osm.nodes.get(i)?;
        Some(Coord::new(f64::from(x) * 1e-7, f64::from(y) * 1e-7))
    };
    let line_of = |refs: &[i64]| -> Option<Vec<Coord>> { refs.iter().map(|r| node(*r)).collect() };
    let mut fields: Vec<Field> = vec![Field::new("osm_id", FieldType::Integer)];
    fields.extend(KEYS.iter().map(|k| Field::new(*k, FieldType::Text)));
    fields.push(Field::new("other_tags", FieldType::Text));
    let attrs = |id: i64, t: &Tags| -> Vec<Value> {
        let mut v = vec![Value::Int(id)];
        v.extend(KEYS.iter().map(|k| tag(t, k).map(|s| Value::Text(s.to_string())).unwrap_or_default()));
        let other: Vec<String> = t
            .iter()
            .filter(|(k, _)| !KEYS.contains(&k.as_str()))
            .map(|(k, v)| format!("\"{k}\"=>\"{}\"", v.replace('"', "\\\"")))
            .collect();
        v.push(if other.is_empty() { Value::Null } else { Value::Text(other.join(",")) });
        v
    };
    let mut points = Dataset::new(format!("{stem} points"), GeometryType::Point, fields.clone());
    for (id, c, t) in &osm.points {
        points.push_next(Some(Geometry::Point(*c)), attrs(*id, t));
    }
    let mut lines = Dataset::new(format!("{stem} lines"), GeometryType::Polyline, fields.clone());
    let mut polys = Dataset::new(format!("{stem} multipolygons"), GeometryType::Polygon, fields);
    let mut missing = 0usize;
    // Ways in a multipolygon are drawn as its parts, not again on their own when their tags are
    // the relation's (as old-style multipolygons repeat them): only a way saying more is a
    // feature of its own.
    let mut member_of: HashMap<i64, Vec<usize>> = HashMap::new();
    for (r, (_, m, _)) in osm.relations.iter().enumerate() {
        for x in m.iter().filter(|x| x.0 == 1) {
            member_of.entry(x.1).or_default().push(r);
        }
    }
    let only_relations = |id: &i64, t: &Tags| {
        member_of
            .get(id)
            .is_some_and(|rs| rs.iter().filter_map(|r| osm.relations.get(*r)).any(|(_, _, rt)| t.iter().all(|kv| rt.contains(kv))))
    };
    let mut way_refs: HashMap<i64, usize> = HashMap::new();
    for (k, (id, refs, t, places)) in osm.ways.iter().enumerate() {
        way_refs.insert(*id, k);
        if t.is_empty() || only_relations(id, t) {
            continue;
        }
        let Some(pts) = places.clone().or_else(|| line_of(refs)) else {
            missing += 1;
            continue;
        };
        let closed = pts.len() >= 4 && pts.first() == pts.last();
        let area = closed
            && tag(t, "area") != Some("no")
            && (tag(t, "area") == Some("yes")
                || t.iter().any(|(k, v)| {
                    AREA_KEYS.contains(&k.as_str())
                        && !(k == "natural" && matches!(v.as_str(), "coastline" | "cliff" | "tree_row" | "ridge"))
                }));
        if area {
            polys.push_next(Some(Geometry::Polygon(vec![vec![pts]])), attrs(*id, t));
        } else if pts.len() >= 2 {
            lines.push_next(Some(Geometry::Line(vec![pts])), attrs(*id, t));
        }
    }
    // Multipolygon and boundary relations: outer and inner member ways joined into rings.
    for (id, members, t) in &osm.relations {
        let mut outer_ways = Vec::new();
        let mut inner_ways = Vec::new();
        // Places the member ways store themselves (LocationsOnWays), by node id.
        let mut local: HashMap<i64, Coord> = HashMap::new();
        for (ty, m, role) in members {
            if *ty != 1 {
                continue;
            }
            let Some(w) = way_refs.get(m).and_then(|k| osm.ways.get(*k)) else { continue };
            if let Some(places) = &w.3 {
                local.extend(w.1.iter().copied().zip(places.iter().copied()));
            }
            if role == "inner" { inner_ways.push(w.1.clone()) } else { outer_ways.push(w.1.clone()) }
        }
        let to_coords = |rings: Vec<Vec<i64>>| -> Vec<Vec<Coord>> {
            rings
                .iter()
                .filter_map(|r| r.iter().map(|n| local.get(n).copied().or_else(|| node(*n))).collect::<Option<Vec<Coord>>>())
                .collect()
        };
        let outers = to_coords(join_rings(outer_ways));
        let inners = to_coords(join_rings(inner_ways));
        if outers.is_empty() {
            missing += 1;
            continue;
        }
        // Each inner ring goes to the outer ring holding it: a box test first, so most pairs cost
        // nothing, and at most MAX_MEMBERS full tests per relation.
        let boxes: Vec<[f64; 4]> = outers.iter().map(|o| bbox(o)).collect();
        let mut parts: Vec<Vec<Vec<Coord>>> = outers.into_iter().map(|o| vec![o]).collect();
        let mut budget = MAX_MEMBERS;
        for inner in inners {
            let p = inner.first().copied().unwrap_or_default();
            let host = (0..parts.len()).find(|&k| {
                boxes.get(k).is_some_and(|b| p.x >= b[0] && p.y >= b[1] && p.x <= b[2] && p.y <= b[3]) && budget > 0 && {
                    budget -= 1;
                    parts.get(k).and_then(|poly| poly.first()).is_some_and(|o| inside(p, o))
                }
            });
            if let Some(poly) = host.and_then(|k| parts.get_mut(k)) {
                poly.push(inner);
            }
        }
        polys.push_next(Some(Geometry::Polygon(parts)), attrs(*id, t));
    }
    let mut report = ReadReport { skipped: missing, ..ReadReport::default() };
    if osm.capped {
        report
            .messages
            .push("the extract is larger than RustGIS reads at once: the rest is left out (use a smaller extract; on the desktop, files over 256 MB open through GDAL when it is installed)".into());
    }
    if missing > 0 {
        report.messages.push(format!("{missing} ways or relations refer to nodes or ways the file doesn't hold"));
    }
    // The notices are the file's: said once, with the first layer.
    let mut out: Vec<(Dataset, ReadReport)> =
        [points, lines, polys].into_iter().filter(|d| !d.features.is_empty()).map(|d| (d, ReadReport::default())).collect();
    if let Some((_, first)) = out.first_mut() {
        *first = report;
    }
    if out.is_empty() {
        return Err(DataError::Format(format!("{file_name}: nothing tagged to draw")));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn zz(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }
    fn field_bytes(f: u64, b: &[u8], out: &mut Vec<u8>) {
        varint(f << 3 | 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }
    fn field_varint(f: u64, v: u64, out: &mut Vec<u8>) {
        varint(f << 3, out);
        varint(v, out);
    }
    fn packed_of(vals: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        for v in vals {
            varint(*v, &mut out);
        }
        out
    }
    fn delta(vals: &[i64]) -> Vec<u8> {
        let mut prev = 0;
        packed_of(
            &vals
                .iter()
                .map(|v| {
                    let d = v - prev;
                    prev = *v;
                    zz(d)
                })
                .collect::<Vec<_>>(),
        )
    }
    fn blob(kind: &str, data: &[u8], zip: bool, out: &mut Vec<u8>) {
        let mut b = Vec::new();
        if zip {
            field_varint(2, data.len() as u64, &mut b);
            field_bytes(3, &miniz_oxide::deflate::compress_to_vec_zlib(data, 6), &mut b);
        } else {
            field_bytes(1, data, &mut b);
        }
        let mut h = Vec::new();
        field_bytes(1, kind.as_bytes(), &mut h);
        field_varint(3, b.len() as u64, &mut h);
        out.extend_from_slice(&(h.len() as u32).to_be_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(&b);
    }

    /// A small file: a tagged node, a road, a closed building, and a multipolygon with a hole made
    /// of two outer half-rings and an inner ring.
    pub(crate) fn sample() -> Vec<u8> {
        let mut out = Vec::new();
        let mut header = Vec::new();
        field_bytes(4, b"OsmSchema-V0.6", &mut header);
        field_bytes(4, b"DenseNodes", &mut header);
        blob("OSMHeader", &header, false, &mut out);
        let strings = [
            "",
            "name",
            "Cafe",
            "highway",
            "residential",
            "building",
            "yes",
            "type",
            "multipolygon",
            "outer",
            "inner",
            "landuse",
            "forest",
            "amenity",
        ];
        let mut st = Vec::new();
        for s in strings {
            field_bytes(1, s.as_bytes(), &mut st);
        }
        let s = |x: &str| strings.iter().position(|y| *y == x).unwrap_or(0) as u64;
        // Dense nodes: ids 1..=13, at lon = id/10, lat = id/20 (in 100-nanodegree units).
        let ids: Vec<i64> = (1..=13).collect();
        let deg = |d: f64| (d * 1e7) as i64;
        let mut lons: Vec<i64> = ids.iter().map(|i| deg(*i as f64 / 10.0)).collect();
        let mut lats: Vec<i64> = ids.iter().map(|i| deg(*i as f64 / 20.0)).collect();
        // The building, 4-7: a square; the multipolygon's outer 8-11, inner 12-13 with 8.
        let square = |c: (f64, f64), r: f64| [(c.0 - r, c.1 - r), (c.0 + r, c.1 - r), (c.0 + r, c.1 + r), (c.0 - r, c.1 + r)];
        for (k, (x, y)) in square((5.0, 5.0), 0.1).iter().enumerate() {
            lons[3 + k] = deg(*x);
            lats[3 + k] = deg(*y);
        }
        for (k, (x, y)) in square((8.0, 8.0), 1.0).iter().enumerate() {
            lons[7 + k] = deg(*x);
            lats[7 + k] = deg(*y);
        }
        for (k, (x, y)) in [(7.9, 7.9), (8.1, 7.9)].iter().enumerate() {
            lons[11 + k] = deg(*x);
            lats[11 + k] = deg(*y);
        }
        let mut kv = vec![s("name"), s("Cafe"), s("amenity"), s("Cafe"), 0];
        kv.extend(std::iter::repeat_n(0, 12));
        let mut dense = Vec::new();
        field_bytes(1, &delta(&ids), &mut dense);
        field_bytes(8, &delta(&lats), &mut dense);
        field_bytes(9, &delta(&lons), &mut dense);
        field_bytes(10, &packed_of(&kv), &mut dense);
        let way = |id: u64, refs: &[i64], keys: &[u64], vals: &[u64]| {
            let mut w = Vec::new();
            field_varint(1, id, &mut w);
            field_bytes(2, &packed_of(keys), &mut w);
            field_bytes(3, &packed_of(vals), &mut w);
            field_bytes(8, &delta(refs), &mut w);
            w
        };
        let mut group = Vec::new();
        field_bytes(2, &dense, &mut group);
        field_bytes(3, &way(100, &[1, 2, 3], &[s("highway")], &[s("residential")]), &mut group);
        field_bytes(3, &way(101, &[4, 5, 6, 7, 4], &[s("building")], &[s("yes")]), &mut group);
        field_bytes(3, &way(102, &[8, 9, 10], &[], &[]), &mut group);
        field_bytes(3, &way(103, &[10, 11, 8], &[], &[]), &mut group);
        // Tagged as its relation is (old-style): drawn as its part only.
        field_bytes(3, &way(104, &[8, 12, 13, 8], &[s("landuse")], &[s("forest")]), &mut group);
        // A way whose nodes the file doesn't hold, with their places on the way (LocationsOnWays).
        let mut onway = way(105, &[900, 901], &[s("highway")], &[s("residential")]);
        field_bytes(9, &delta(&[deg(1.0), deg(1.5)]), &mut onway);
        field_bytes(10, &delta(&[deg(2.0), deg(2.5)]), &mut onway);
        field_bytes(3, &onway, &mut group);
        let mut rel = Vec::new();
        field_varint(1, 200, &mut rel);
        field_bytes(2, &packed_of(&[s("type"), s("landuse")]), &mut rel);
        field_bytes(3, &packed_of(&[s("multipolygon"), s("forest")]), &mut rel);
        field_bytes(8, &packed_of(&[s("outer"), s("outer"), s("inner")]), &mut rel);
        field_bytes(9, &delta(&[102, 103, 104]), &mut rel);
        field_bytes(10, &packed_of(&[1, 1, 1]), &mut rel);
        field_bytes(4, &rel, &mut group);
        let mut block = Vec::new();
        field_bytes(1, &st, &mut block);
        field_bytes(2, &group, &mut block);
        blob("OSMData", &block, true, &mut out);
        out
    }

    #[test]
    fn reads_nodes_ways_and_relations() {
        let layers = read("town.osm.pbf", &sample()).unwrap();
        let names: Vec<&str> = layers.iter().map(|(d, _)| d.name.as_str()).collect();
        assert_eq!(names, ["town points", "town lines", "town multipolygons"]);
        let (pts, lines, polys) = (&layers[0].0, &layers[1].0, &layers[2].0);
        assert_eq!(pts.features.len(), 1);
        assert_eq!(pts.features[0].attrs[1], Value::Text("Cafe".into()), "name");
        let Some(Geometry::Point(c)) = &pts.features[0].geometry else { panic!() };
        assert!((c.x - 0.1).abs() < 1e-7 && (c.y - 0.05).abs() < 1e-7, "{c:?}");
        assert_eq!(lines.features.len(), 2, "the road, and the way carrying its own places");
        let Some(Geometry::Line(l)) = &lines.features[1].geometry else { panic!() };
        assert!((l[0][1].x - 2.5).abs() < 1e-7 && (l[0][1].y - 1.5).abs() < 1e-7, "{l:?}");
        assert!(looks_like_osm_pbf(&sample()) && !looks_like_osm_pbf(b"\x1a\x05water"));
        assert_eq!(polys.features.len(), 2, "the building and the forest");
        let forest = polys.features.iter().find(|f| f.attrs[0] == Value::Int(200)).unwrap();
        assert!(matches!(&forest.geometry, Some(Geometry::Polygon(p)) if p.len() == 1 && p[0].len() == 2), "{:?}", forest.geometry);
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
    }

    #[test]
    fn rings_join_by_their_ends() {
        // Two rings from pieces in any order and direction, and a piece that closes nothing.
        let rings = join_rings(vec![vec![3, 4, 1], vec![10, 11], vec![1, 2], vec![20, 21, 22, 20], vec![3, 2]]);
        assert_eq!(rings.len(), 2, "{rings:?}");
        assert!(rings.iter().any(|r| r.len() == 5 && r.first() == r.last() && [1, 2, 3, 4].iter().all(|n| r.contains(n))), "{rings:?}");
        // A long chain joins in linear time.
        let n = 50_000i64;
        let mut chain: Vec<Vec<i64>> = (0..n).map(|i| vec![i, (i + 1) % n]).collect();
        chain.reverse();
        let rings = join_rings(chain);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), n as usize + 1);
    }

    #[test]
    fn header_features_and_long_lists() {
        // History and metadata features don't change how the blocks decode; an unknown one does.
        let header = |feats: &[&str]| {
            let mut h = Vec::new();
            for f in feats {
                field_bytes(4, f.as_bytes(), &mut h);
            }
            let mut out = Vec::new();
            blob("OSMHeader", &h, false, &mut out);
            out
        };
        // (A file of only a header holds no features: refused for that, not for its features.)
        let e = read("h.osm.pbf", &header(&["OsmSchema-V0.6", "DenseNodes", "HistoricalInformation", "Has_Metadata"]))
            .err()
            .map(|e| e.to_string());
        assert!(e.as_deref().is_none_or(|e| !e.contains("feature '")), "{e:?}");
        let e = read("h.osm.pbf", &header(&["OsmSchema-V0.6", "Sideways_Nodes"])).unwrap_err();
        assert!(e.to_string().contains("Sideways_Nodes"), "{e}");
        // A packed list past the cap is refused before it grows.
        assert_eq!(packed(&packed_of(&[1, 2, 3])).unwrap(), vec![1, 2, 3]);
        assert!(packed(&vec![0u8; MAX_PACKED + 1]).is_err());
        assert_eq!(packed(&vec![0u8; MAX_PACKED]).map(|v| v.len()).ok(), Some(MAX_PACKED));
    }

    #[test]
    fn junk_is_refused() {
        let bytes = sample();
        for cut in (0..bytes.len()).step_by(13) {
            let _ = read("x.osm.pbf", &bytes[..cut]);
        }
        let mut flipped = bytes.clone();
        for k in (0..flipped.len()).step_by(29) {
            flipped[k] ^= 0x55;
        }
        let _ = read("x.osm.pbf", &flipped);
        assert!(read("x.osm.pbf", b"").is_err());
        assert!(read("x.osm.pbf", b"\0\0\0\x05hello").is_err());
    }
}
