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
/// Keys that make a closed way an area, for an OSM file and an Overpass answer alike.
const AREA_KEYS: &[&str] = &[
    "aeroway",
    "amenity",
    "boundary",
    "building",
    "craft",
    "geological",
    "historic",
    "landcover",
    "landuse",
    "leisure",
    "man_made",
    "military",
    "natural",
    "office",
    "parking",
    "place",
    "public_transport",
    "shop",
    "sport",
    "tourism",
    "water",
];

/// Lifecycle prefixes (OSM wiki, "Lifecycle prefix"): `destroyed:building=house` is the footprint
/// of a house that collapsed, with no `building=*` left.
const LIFECYCLE_PREFIXES: &[&str] = &[
    "abandoned:",
    "construction:",
    "demolished:",
    "destroyed:",
    "disused:",
    "planned:",
    "proposed:",
    "razed:",
    "removed:",
    "ruins:",
    "was:",
];

/// A key without its lifecycle prefix: `destroyed:building` → `building`.
fn unprefixed(k: &str) -> &str {
    LIFECYCLE_PREFIXES.iter().find_map(|p| k.strip_prefix(p)).unwrap_or(k)
}

/// Whether a closed way is an area by its tags: the one rule for a `.osm.pbf` file and an Overpass
/// answer, so both give the same layers. `area=yes|no` decides; else a closed road or barrier is
/// a line; else a key of `AREA_KEYS` makes it an area, except a line of nature (coastline, cliff,
/// tree row, ridge) and a waterway that isn't a water surface. A key is read without its
/// lifecycle prefix throughout: a destroyed building keeps a building's footprint, and a
/// destroyed wall a wall's line.
pub fn closed_way_is_area(tags: &[(String, String)]) -> bool {
    match tag(tags, "area") {
        Some("yes") => return true,
        Some("no") => return false,
        _ => {}
    }
    if tags.iter().any(|(k, _)| matches!(unprefixed(k), "highway" | "barrier")) {
        return false;
    }
    tags.iter().any(|(k, v)| {
        let k = unprefixed(k);
        match k {
            "natural" => !matches!(v.as_str(), "coastline" | "cliff" | "tree_row" | "ridge"),
            "waterway" => matches!(v.as_str(), "riverbank" | "dock" | "boatyard"),
            _ => AREA_KEYS.contains(&k),
        }
    })
}

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

/// The most values in one decoded list (the complete logical field for Relation arrays).
/// Writers put about 8,000 entities in a block, and a way
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

/// Appends one occurrence of a repeated relation field, bounded over the complete logical array.
/// Protobuf permits packed and unpacked occurrences to interleave; no intermediate value list is built.
fn append_repeated(out: &mut Vec<u64>, value: Wire<'_>) -> Result<()> {
    crate::progress::check()?;
    let mut push = |v| -> Result<()> {
        if out.len() >= MAX_PACKED {
            return Err(DataError::Format(format!("an OSM PBF block holds a list of more than {MAX_PACKED} values")));
        }
        if out.len().is_multiple_of(1024) {
            crate::progress::check()?;
        }
        out.push(v);
        Ok(())
    };
    match value {
        Wire::Varint(v) => push(v),
        Wire::Bytes(b) => {
            let mut p = Pb::new(b);
            while p.i < b.len() {
                push(p.varint().ok_or_else(|| bad("a packed relation value"))?)?;
            }
            Ok(())
        }
        Wire::Other => Ok(()),
    }
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

fn tag<'t>(t: &'t [(String, String)], k: &str) -> Option<&'t str> {
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
    /// Members omitted at the per-relation limit; wide enough for all bounded relations.
    omitted_members: u64,
}

/// Degrees from OSM's fixed point (1e-7°), and back.
fn to_fixed(nano: i64) -> i32 {
    (nano / 100).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Decodes a primitive block within its object/member limits and counts source members past the cap.
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
                            (2, value) => append_repeated(&mut k, value)?,
                            (3, value) => append_repeated(&mut vv, value)?,
                            (8, value) => append_repeated(&mut roles, value)?,
                            (9, value) => append_repeated(&mut mems, value)?,
                            (10, value) => append_repeated(&mut types, value)?,
                            _ => {}
                        }
                    }
                    // Member IDs are delta-coded across all field occurrences, not each payload.
                    let mut previous = 0i64;
                    let mems: Vec<i64> = mems
                        .into_iter()
                        .map(|v| {
                            previous = previous.wrapping_add(zigzag(v));
                            previous
                        })
                        .collect();
                    let t = tags(&strings, &k, &vv);
                    // Only the relations drawn are kept.
                    if osm.relations.len() >= MAX_RELATIONS {
                        osm.capped = true;
                    } else if matches!(tag(&t, "type"), Some("multipolygon" | "boundary")) {
                        if roles.len() != mems.len() || types.len() != mems.len() {
                            return Err(DataError::Format(format!(
                                "OSM relation {id} has mismatched member arrays (IDs: {}, roles: {}, types: {})",
                                mems.len(),
                                roles.len(),
                                types.len()
                            )));
                        }
                        let omitted = mems.len().saturating_sub(MAX_MEMBERS) as u64;
                        osm.omitted_members = osm.omitted_members.saturating_add(omitted);
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
        // Read ahead, off the UI thread: a large extract can be cancelled between blocks.
        crate::progress::check()?;
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

/// Joins ways (as node-id lists) by their ends, counting the member ways in discarded chains.
fn join_rings(ways: Vec<Vec<i64>>, unclosed: &mut usize) -> Vec<Vec<i64>> {
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
        let mut member_ways = 1;
        let mut guard = 0;
        while cur.first() != cur.last() && guard < MAX_MEMBERS {
            guard += 1;
            let Some(&end) = cur.last() else { break };
            let Some(list) = ends.get_mut(&end) else { break };
            let free = list.iter().position(|&k| ways.get(k).is_some_and(Option::is_some)).unwrap_or(list.len());
            list.drain(..free);
            let Some(mut next) = list.first().and_then(|&k| ways.get_mut(k)).and_then(Option::take) else { break };
            member_ways += 1;
            if next.first() != Some(&end) {
                next.reverse();
            }
            cur.extend(next.into_iter().skip(1));
        }
        if cur.len() >= 4 && cur.first() == cur.last() {
            rings.push(cur);
        } else {
            *unclosed += member_ways;
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

/// Reads an OSM PBF file into its points, lines and multipolygons layers. Cancel is looked at
/// between blocks and between the ways and relations built from them; decoding one block is one
/// step without a look.
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
    let (mut missing_members, mut missing_rings, mut unclosed, mut outside, mut containment_limited) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut unsupported_members = 0u64;
    let mut short_ways = 0usize;
    let mut no_closed_outer = 0usize;
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
        crate::progress::check()?;
        way_refs.insert(*id, k);
        if t.is_empty() || only_relations(id, t) {
            continue;
        }
        let Some(pts) = places.clone().or_else(|| line_of(refs)) else {
            missing += 1;
            continue;
        };
        let closed = pts.len() >= 4 && pts.first() == pts.last();
        if closed && closed_way_is_area(t) {
            polys.push_next(Some(Geometry::Polygon(vec![vec![pts]])), attrs(*id, t));
        } else if pts.len() >= 2 {
            lines.push_next(Some(Geometry::Line(vec![pts])), attrs(*id, t));
        } else {
            short_ways += 1;
        }
    }
    // Multipolygon and boundary relations: outer and inner member ways joined into rings.
    for (id, members, t) in &osm.relations {
        crate::progress::check()?;
        let mut outer_ways = Vec::new();
        let mut inner_ways = Vec::new();
        let (mut relation_missing_members, mut relation_missing_rings, mut relation_unclosed) = (0usize, 0usize, 0usize);
        let mut relation_subrelations = 0usize;
        let mut unavailable_outer = false;
        // Places the member ways store themselves (LocationsOnWays), by node id.
        let mut local: HashMap<i64, Coord> = HashMap::new();
        for (ty, m, role) in members {
            if *ty == 2 {
                relation_subrelations += 1;
                continue;
            }
            if *ty != 1 {
                continue;
            }
            let Some(w) = way_refs.get(m).and_then(|k| osm.ways.get(*k)) else {
                relation_missing_members += 1;
                unavailable_outer |= role != "inner";
                continue;
            };
            if let Some(places) = &w.3 {
                local.extend(w.1.iter().copied().zip(places.iter().copied()));
            }
            if role == "inner" { inner_ways.push(w.1.clone()) } else { outer_ways.push(w.1.clone()) }
        }
        let mut to_coords = |rings: Vec<Vec<i64>>| -> Vec<Vec<Coord>> {
            rings
                .iter()
                .filter_map(|r| {
                    let coords = r.iter().map(|n| local.get(n).copied().or_else(|| node(*n))).collect::<Option<Vec<Coord>>>();
                    if coords.is_none() {
                        relation_missing_rings += 1;
                    }
                    coords
                })
                .collect()
        };
        let outer_rings = join_rings(outer_ways, &mut relation_unclosed);
        // Closed outer rings with no usable coordinates refer to unavailable nodes.
        unavailable_outer |= !outer_rings.is_empty();
        let outers = to_coords(outer_rings);
        let inners = to_coords(join_rings(inner_ways, &mut relation_unclosed));
        if outers.is_empty() {
            if unavailable_outer {
                missing += 1;
            } else {
                no_closed_outer += 1;
            }
            continue;
        }
        unsupported_members = unsupported_members.saturating_add(relation_subrelations as u64);
        missing_members = missing_members.saturating_add(relation_missing_members as u64);
        missing_rings = missing_rings.saturating_add(relation_missing_rings as u64);
        unclosed = unclosed.saturating_add(relation_unclosed as u64);
        // Each inner ring goes to the outer ring holding it: a box test first, so most pairs cost
        // nothing, and at most MAX_MEMBERS full tests per relation.
        let boxes: Vec<[f64; 4]> = outers.iter().map(|o| bbox(o)).collect();
        let mut parts: Vec<Vec<Vec<Coord>>> = outers.into_iter().map(|o| vec![o]).collect();
        let mut budget = MAX_MEMBERS;
        for inner in inners {
            let p = inner.first().copied().unwrap_or_default();
            let mut untested_candidate = false;
            let host = (0..parts.len()).find(|&k| {
                if !boxes.get(k).is_some_and(|b| p.x >= b[0] && p.y >= b[1] && p.x <= b[2] && p.y <= b[3]) {
                    return false;
                }
                if budget == 0 {
                    untested_candidate = true;
                    return false;
                }
                budget -= 1;
                parts.get(k).and_then(|poly| poly.first()).is_some_and(|o| inside(p, o))
            });
            if let Some(poly) = host.and_then(|k| parts.get_mut(k)) {
                poly.push(inner);
            } else if untested_candidate {
                containment_limited += 1;
            } else {
                outside += 1;
            }
        }
        polys.push_next(Some(Geometry::Polygon(parts)), attrs(*id, t));
    }
    let mut report = ReadReport { skipped: missing + short_ways + no_closed_outer, ..ReadReport::default() };
    if osm.omitted_members > 0 {
        let members = if osm.omitted_members == 1 { "relation member" } else { "relation members" };
        report.messages.push(format!("{} {members} left out at the per-relation limit of {MAX_MEMBERS}", osm.omitted_members));
    }
    if short_ways > 0 {
        let subject = if short_ways == 1 { "tagged way left out because it has" } else { "tagged ways left out because they have" };
        report
            .messages
            .push(crate::loss::Note::loss(crate::loss::LossCode::DroppedFeature, format!("{short_ways} {subject} fewer than two nodes")));
    }
    if no_closed_outer > 0 {
        let subject = if no_closed_outer == 1 { "relation left out because it has" } else { "relations left out because they have" };
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{no_closed_outer} {subject} no usable closed outer ring"),
        ));
    }
    if osm.capped {
        report
            .messages
            .push(crate::loss::Note::loss(crate::loss::LossCode::DroppedOverLimit, "the extract is larger than RustGIS reads at once: the rest is left out (use a smaller extract; on the desktop, files over 256 MB open through GDAL when it is installed)"));
    }
    if missing > 0 {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{missing} ways or relations refer to nodes or ways the file doesn't hold"),
        ));
    }
    for (count, one, many) in [
        (
            unsupported_members,
            "OSM sub-relation member was omitted because nested relations are not supported",
            "OSM sub-relation members were omitted because nested relations are not supported",
        ),
        (
            missing_members,
            "OSM relation member way was omitted because it is unavailable",
            "OSM relation member ways were omitted because they are unavailable",
        ),
        (
            missing_rings,
            "OSM relation ring was omitted because its node coordinates are unavailable",
            "OSM relation rings were omitted because their node coordinates are unavailable",
        ),
        (
            unclosed,
            "OSM relation member way was omitted because it does not join into a closed ring of at least four coordinates",
            "OSM relation member ways were omitted because they do not join into closed rings of at least four coordinates",
        ),
        (
            outside,
            "OSM inner ring was omitted because no containing outer ring was found",
            "OSM inner rings were omitted because no containing outer ring was found",
        ),
    ] {
        if count > 0 {
            let subject = if count == 1 { one } else { many };
            report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedGeometry, format!("{count} {subject}")));
        }
    }
    if containment_limited > 0 {
        let subject = if containment_limited == 1 { "inner ring was" } else { "inner rings were" };
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedOverLimit,
            format!("{containment_limited} OSM {subject} omitted after the containment-test limit of {MAX_MEMBERS}"),
        ));
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

    /// A small file: a tagged node, a road, a closed building, the same footprint as a destroyed
    /// building (`destroyed:building`), and a multipolygon with a hole made of two outer
    /// half-rings and an inner ring.
    pub(crate) fn sample() -> Vec<u8> {
        sample_with_missing_outer(false)
    }

    /// A missing outer leaves the relation out entirely, rather than partially importing it.
    fn sample_with_missing_outer(missing_outer: bool) -> Vec<u8> {
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
            "destroyed:building",
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
        field_bytes(3, &way(106, &[4, 5, 6, 7, 4], &[s("destroyed:building")], &[s("yes")]), &mut group);
        if !missing_outer {
            field_bytes(3, &way(102, &[8, 9, 10], &[], &[]), &mut group);
            field_bytes(3, &way(103, &[10, 11, 8], &[], &[]), &mut group);
        }
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

    /// Native PBF builder for finite omission cases: a tagged point survives every bad way or
    /// relation, so successful imports must report the omitted data. All referenced nodes are present.
    fn finite_omission_file(ways: &[(u64, &[i64], bool)], members: Option<&[(i64, u64, u64)]>) -> Vec<u8> {
        if let Some(members) = members {
            let roles: Vec<_> = members.iter().map(|m| m.2).collect();
            let ids: Vec<_> = members.iter().map(|m| m.0).collect();
            let types: Vec<_> = members.iter().map(|m| m.1).collect();
            finite_member_arrays_file(ways, &[(200, &roles, &ids, &types)])
        } else {
            finite_member_arrays_file(ways, &[])
        }
    }

    /// Explicit parallel member arrays allow malformed input and actual nested relations in tests.
    fn finite_member_arrays_file(ways: &[(u64, &[i64], bool)], relations: &[(u64, &[u64], &[i64], &[u64])]) -> Vec<u8> {
        finite_member_fields_file(ways, relations, field_bytes)
    }

    /// Relation scalar fields can use packed, split or unpacked wire encodings of the same values.
    fn finite_member_fields_file(
        ways: &[(u64, &[i64], bool)],
        relations: &[(u64, &[u64], &[i64], &[u64])],
        encode: impl Fn(u64, &[u8], &mut Vec<u8>),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut header = Vec::new();
        field_bytes(4, b"OsmSchema-V0.6", &mut header);
        blob("OSMHeader", &header, false, &mut out);
        let mut st = Vec::new();
        for s in ["", "name", "marker", "highway", "service", "type", "multipolygon", "outer", "inner"] {
            field_bytes(1, s.as_bytes(), &mut st);
        }
        let mut group = Vec::new();
        for (id, x, y) in [
            (1, 0, 0),
            (2, 40_000_000, 0),
            (3, 40_000_000, 40_000_000),
            (4, 0, 40_000_000),
            (5, 30_000_000, 30_000_000),
            (6, 35_000_000, 30_000_000),
            (7, 30_000_000, 35_000_000),
        ] {
            let mut n = Vec::new();
            field_varint(1, zz(id), &mut n);
            field_varint(8, zz(y), &mut n);
            field_varint(9, zz(x), &mut n);
            if id == 1 {
                field_bytes(2, &packed_of(&[1]), &mut n);
                field_bytes(3, &packed_of(&[2]), &mut n);
            }
            field_bytes(1, &n, &mut group);
        }
        for (id, refs, tagged) in ways {
            let mut w = Vec::new();
            field_varint(1, *id, &mut w);
            field_bytes(8, &delta(refs), &mut w);
            if *tagged {
                field_bytes(2, &packed_of(&[3]), &mut w);
                field_bytes(3, &packed_of(&[4]), &mut w);
            }
            field_bytes(3, &w, &mut group);
        }
        for (id, roles, ids, types) in relations {
            let mut r = Vec::new();
            field_varint(1, *id, &mut r);
            encode(2, &packed_of(&[5]), &mut r);
            encode(3, &packed_of(&[6]), &mut r);
            encode(8, &packed_of(roles), &mut r);
            encode(9, &delta(ids), &mut r);
            encode(10, &packed_of(types), &mut r);
            field_bytes(4, &r, &mut group);
        }
        let mut block = Vec::new();
        field_bytes(1, &st, &mut block);
        field_bytes(2, &group, &mut block);
        blob("OSMData", &block, false, &mut out);
        out
    }

    /// Split member fields preserve delta continuity; tag arrays also concatenate across occurrences.
    #[test]
    fn relation_members_split_fields_keep_geometry_and_tags() {
        let ways: &[(u64, &[i64], bool)] = &[(100, &[1, 2, 3, 4, 1], false), (101, &[5, 6, 7, 5], false)];
        let file = |mode| {
            finite_member_fields_file(ways, &[(200, &[7, 8], &[100, 101], &[1, 1])], |field, bytes, out| {
                let mut bytes = bytes.to_vec();
                if field == 2 {
                    bytes.extend(packed_of(&[1]));
                }
                if field == 3 {
                    bytes.extend(packed_of(&[2]));
                }
                if mode == 0 || (mode == 1 && field != 8) {
                    field_bytes(field, &bytes, out);
                    return;
                }
                let mut values = Pb::new(&bytes);
                let mut index = 0;
                while let Some(value) = values.varint() {
                    if mode == 3 || (mode == 4 && index % 2 == 1) {
                        field_varint(field, value, out);
                    } else {
                        field_bytes(field, &packed_of(&[value]), out);
                    }
                    index += 1;
                }
            })
        };
        let clean = read("split.pbf", &file(0)).unwrap();
        assert_eq!(clean.len(), 2);
        assert!(clean.iter().all(|(_, report)| report.messages.is_empty() && report.skipped == 0));
        let Some(Geometry::Polygon(parts)) = &clean[1].0.features[0].geometry else { panic!("polygon") };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].len(), 2);
        assert_eq!(clean[1].0.features[0].attrs[1], Value::Text("marker".into()));
        for mode in 1..=4 {
            let decoded = read("split.pbf", &file(mode)).unwrap();
            assert_eq!(decoded.len(), clean.len());
            for ((actual, report), (expected, _)) in decoded.iter().zip(&clean) {
                assert_eq!(actual.fields, expected.fields);
                assert_eq!(actual.features.iter().collect::<Vec<_>>(), expected.features.iter().collect::<Vec<_>>());
                assert_eq!(actual.held_in, expected.held_in);
                assert!(report.messages.is_empty());
                assert_eq!(report.skipped, 0);
            }
        }
    }

    /// Splitting a logical member array cannot evade the existing packed-value allocation bound.
    #[test]
    fn relation_members_split_fields_share_the_value_limit() {
        let file = finite_member_fields_file(&[], &[(200, &[7], &[100], &[1])], |field, bytes, out| {
            if field == 8 {
                field_bytes(field, &vec![0; MAX_PACKED], out);
                field_varint(field, 0, out);
            } else {
                field_bytes(field, bytes, out);
            }
        });
        let err = read("split.pbf", &file).err().map(|e| e.to_string()).unwrap_or_default();
        assert_eq!(err, format!("an OSM PBF block holds a list of more than {MAX_PACKED} values"));
    }

    /// Unsupported sub-relations are counted only for parents whose direct ways produce geometry.
    #[test]
    fn relation_members_nested_geometry_is_reported() {
        let ways: &[(u64, &[i64], bool)] = &[(100, &[1, 2, 3, 4, 1], false), (101, &[5, 6, 7, 5], false)];
        let clean =
            read("nested.pbf", &finite_member_arrays_file(ways, &[(200, &[7, 0], &[100, 1], &[1, 0]), (201, &[7], &[101], &[1])])).unwrap();
        assert_eq!(clean.len(), 2);
        assert_eq!(clean[1].0.features.len(), 2);
        assert!(clean.iter().all(|(_, r)| r.messages.is_empty()));
        let lossy = read(
            "nested.pbf",
            &finite_member_arrays_file(ways, &[(200, &[7, 0, 8], &[100, 1, 201], &[1, 0, 2]), (201, &[7], &[101], &[1])]),
        )
        .unwrap();
        assert_eq!(lossy[1].0.features.iter().collect::<Vec<_>>(), clean[1].0.features.iter().collect::<Vec<_>>());
        assert_eq!(lossy[0].1.skipped, 0);
        assert_eq!(
            lossy[0].1.messages.texts().collect::<Vec<_>>(),
            ["1 OSM sub-relation member was omitted because nested relations are not supported"]
        );
        assert!(lossy[1].1.messages.is_empty());
        let omitted =
            read("nested.pbf", &finite_member_arrays_file(ways, &[(200, &[8], &[201], &[2]), (201, &[7], &[101], &[1])])).unwrap();
        assert_eq!(omitted[0].1.skipped, 1);
        assert_eq!(omitted[0].1.messages.texts().collect::<Vec<_>>(), ["1 relation left out because it has no usable closed outer ring"]);
    }

    /// Parallel member arrays must agree; valid geometry must not hide an unmatched tail.
    #[test]
    fn relation_members_mismatched_arrays_are_refused() {
        let ways: &[(u64, &[i64], bool)] = &[(100, &[1, 2, 3, 4, 1], false), (101, &[5, 6, 7, 5], false)];
        let clean = read("arrays.pbf", &finite_member_arrays_file(ways, &[(200, &[7, 8], &[100, 101], &[1, 1])])).unwrap();
        assert_eq!(clean[1].0.features.len(), 1);
        assert!(clean.iter().all(|(_, r)| r.messages.is_empty()));
        for (roles, ids, types) in
            [(&[7][..], &[100, 101][..], &[1, 1][..]), (&[7, 8][..], &[100, 101][..], &[1][..]), (&[7, 8][..], &[100][..], &[1][..])]
        {
            let err = read("arrays.pbf", &finite_member_arrays_file(ways, &[(200, roles, ids, types)]))
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert_eq!(
                err,
                format!(
                    "OSM relation 200 has mismatched member arrays (IDs: {}, roles: {}, types: {})",
                    ids.len(),
                    roles.len(),
                    types.len()
                )
            );
        }
    }

    /// Each hole needs two box-overlapping outer tests; the extra hole exceeds the real budget.
    #[test]
    fn containment_limit_has_its_own_omission_notice() {
        let ways: &[(u64, &[i64], bool)] = &[(100, &[1, 2, 4, 1], false), (101, &[1, 2, 3, 4, 1], false), (102, &[5, 6, 7, 5], false)];
        let mut members = vec![(100, 1, 7), (101, 1, 7)];
        members.extend(std::iter::repeat_n((102, 1, 8), MAX_MEMBERS / 2));
        let clean = read("budget.pbf", &finite_omission_file(ways, Some(&members))).unwrap();
        assert_eq!(clean.len(), 2);
        assert_eq!(clean[0].1.skipped, 0);
        assert!(clean.iter().all(|(_, report)| report.messages.is_empty()));
        let Some(Geometry::Polygon(parts)) = &clean[1].0.features[0].geometry else { panic!("relation") };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 1);
        assert_eq!(parts[1].len(), MAX_MEMBERS / 2 + 1);
        assert_eq!(parts[1][1], vec![Coord::new(3., 3.), Coord::new(3.5, 3.), Coord::new(3., 3.5), Coord::new(3., 3.)]);
        members.push((102, 1, 8));
        let lossy = read("budget.pbf", &finite_omission_file(ways, Some(&members))).unwrap();
        assert_eq!(lossy[1].0.features.iter().collect::<Vec<_>>(), clean[1].0.features.iter().collect::<Vec<_>>());
        assert_eq!(lossy[0].1.skipped, 0);
        assert_eq!(
            lossy[0].1.messages.texts().collect::<Vec<_>>(),
            ["1 OSM inner ring was omitted after the containment-test limit of 100000"]
        );
        assert!(lossy[1].1.messages.is_empty());
    }

    #[test]
    fn finite_omission_member_cap_counts_actual_tail() {
        let ways: &[(u64, &[i64], bool)] = &[(100, &[1, 2, 3, 4, 1], false), (101, &[1, 2, 3, 1], false)];
        // Non-way members in the prefix are intentional: they must not be called unavailable.
        let mut members = vec![(1, 0, 0); MAX_MEMBERS];
        members[0] = (100, 1, 7);
        let clean = read("finite.pbf", &finite_omission_file(ways, Some(&members))).unwrap();
        assert!(clean[0].1.messages.is_empty());
        assert_eq!(clean[0].1.skipped, 0);
        members.push((101, 1, 8));
        let layers = read("finite.pbf", &finite_omission_file(ways, Some(&members))).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].0.features.iter().collect::<Vec<_>>(), clean[1].0.features.iter().collect::<Vec<_>>());
        assert_eq!(layers[0].1.skipped, 0, "a member is not a whole skipped feature");
        assert_eq!(layers[0].1.messages.texts().collect::<Vec<_>>(), ["1 relation member left out at the per-relation limit of 100000"]);
        assert!(layers[1].1.messages.is_empty());
    }

    #[test]
    fn finite_omission_short_tagged_standalone_ways() {
        let ways: &[(u64, &[i64], bool)] = &[(100, &[], true), (101, &[1], true), (102, &[1], false), (103, &[1, 2], true)];
        let layers = read("finite.pbf", &finite_omission_file(ways, None)).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].0.features.len(), 1);
        assert_eq!(layers[1].0.features[0].attrs[0], Value::Int(103));
        assert_eq!(layers[0].1.skipped, 2);
        assert_eq!(layers[0].1.messages.texts().collect::<Vec<_>>(), ["2 tagged ways left out because they have fewer than two nodes"]);
    }

    #[test]
    fn finite_omission_whole_relation_reason_matches_source() {
        for (refs, members, reason) in [
            (&[1, 2, 3][..], vec![(100, 1, 7)], "1 relation left out because it has no usable closed outer ring"),
            (&[1][..], vec![(100, 1, 7)], "1 relation left out because it has no usable closed outer ring"),
            (&[1, 2, 3, 1][..], vec![(100, 1, 8)], "1 relation left out because it has no usable closed outer ring"),
            (&[][..], vec![], "1 relation left out because it has no usable closed outer ring"),
            (&[1, 2, 3, 1][..], vec![(999, 1, 7)], "1 ways or relations refer to nodes or ways the file doesn't hold"),
            (&[1, 2, 999, 1][..], vec![(100, 1, 7)], "1 ways or relations refer to nodes or ways the file doesn't hold"),
        ] {
            let layers = read("finite.pbf", &finite_omission_file(&[(100, refs, false)], Some(&members))).unwrap();
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].0.features.len(), 1);
            assert_eq!(layers[0].1.skipped, 1);
            assert_eq!(layers[0].1.messages.texts().collect::<Vec<_>>(), [reason], "refs={refs:?}, members={members:?}");
        }
    }

    /// A discarded relation is counted once; its missing members aren't losses of kept geometry.
    #[test]
    fn omitted_relations_are_not_reported_as_partial_losses() {
        let layers = read("missing-outer.pbf", &sample_with_missing_outer(true)).unwrap();
        assert_eq!(layers.iter().map(|(ds, _)| ds.features.len()).collect::<Vec<_>>(), [1, 2, 2]);
        assert_eq!(layers[0].1.skipped, 1);
        assert_eq!(layers[0].1.messages.texts().collect::<Vec<_>>(), ["1 ways or relations refer to nodes or ways the file doesn't hold"]);
        assert_eq!(layers[0].1.messages[0].loss, Some(crate::loss::LossCode::DroppedFeature));
        assert!(layers.iter().skip(1).all(|(_, report)| report.skipped == 0 && report.messages.is_empty()));
    }

    /// Pinned PBF files keep relation 50 and its outer ring when one hole cannot be read.
    #[test]
    fn omitted_relation_geometry_is_said() {
        let clean = include_bytes!("../../engine/testdata/reading-losses/hole-clean.osm.pbf");
        let cases: &[(&[u8], &str)] = &[
            (
                include_bytes!("../../engine/testdata/reading-losses/hole-missing-member.osm.pbf"),
                "1 OSM relation member way was omitted because it is unavailable",
            ),
            (
                include_bytes!("../../engine/testdata/reading-losses/hole-missing-node.osm.pbf"),
                "1 OSM relation ring was omitted because its node coordinates are unavailable",
            ),
            (
                include_bytes!("../../engine/testdata/reading-losses/hole-unclosed.osm.pbf"),
                "1 OSM relation member way was omitted because it does not join into a closed ring of at least four coordinates",
            ),
            (
                include_bytes!("../../engine/testdata/reading-losses/hole-outside.osm.pbf"),
                "1 OSM inner ring was omitted because no containing outer ring was found",
            ),
        ];
        let kept = |bytes: &[u8], rings| {
            let layers = read("holes.osm.pbf", bytes).unwrap();
            assert_eq!(layers.len(), 1);
            let (ds, report) = &layers[0];
            assert_eq!(ds.features.len(), 1);
            assert_eq!(ds.features[0].attrs[0], Value::Int(50));
            let Some(Geometry::Polygon(parts)) = &ds.features[0].geometry else { panic!() };
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].len(), rings);
            assert_eq!(parts[0][0], vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(0.0, 10.0), Coord::new(0.0, 0.0)]);
            assert_eq!(report.skipped, 0, "no feature was skipped");
            // Parts of a polygon that is kept are its geometry's loss, not a feature's.
            assert!(report.messages.iter().all(|m| m.loss == Some(crate::loss::LossCode::DroppedGeometry)), "{report:?}");
            report.messages.texts().map(str::to_string).collect::<Vec<_>>()
        };
        assert!(kept(clean, 2).is_empty());
        for (bytes, message) in cases {
            assert_eq!(kept(bytes, 1), [*message]);
        }
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
        assert_eq!(polys.features.len(), 3, "the building, the destroyed building and the forest");
        assert!(polys.features.iter().any(|f| f.attrs[0] == Value::Int(106)), "a lifecycle prefix keeps a footprint an area");
        let forest = polys.features.iter().find(|f| f.attrs[0] == Value::Int(200)).unwrap();
        assert!(matches!(&forest.geometry, Some(Geometry::Polygon(p)) if p.len() == 1 && p[0].len() == 2), "{:?}", forest.geometry);
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
    }

    #[test]
    fn closed_way_areas() {
        let t = |kv: &[(&str, &str)]| -> Tags { kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        for area in [
            t(&[("building", "yes")]),
            t(&[("destroyed:building", "house"), ("damage:event", "quake")]),
            t(&[("demolished:building", "yes")]),
            t(&[("ruins:building", "yes")]),
            t(&[("disused:amenity", "school")]),
            t(&[("man_made", "wastewater_plant")]),
            t(&[("sport", "soccer")]),
            t(&[("waterway", "riverbank")]),
            t(&[("highway", "pedestrian"), ("area", "yes")]),
            t(&[("name", "x"), ("area", "yes")]),
            // An islet drawn as one closed coastline: the place makes it an area, the coastline
            // doesn't keep it a line (Overpass read it as a line before both readers shared this).
            t(&[("natural", "coastline"), ("place", "islet")]),
            t(&[("waterway", "stream"), ("landuse", "meadow")]),
        ] {
            assert!(closed_way_is_area(&area), "{area:?}");
        }
        for line in [
            t(&[("highway", "residential")]),
            t(&[("barrier", "fence"), ("landuse", "meadow")]),
            t(&[("destroyed:barrier", "wall"), ("landuse", "meadow")]),
            t(&[("building", "yes"), ("area", "no")]),
            t(&[("natural", "coastline")]),
            t(&[("destroyed:natural", "ridge")]),
            t(&[("waterway", "stream")]),
            t(&[("destroyed:railway", "rail")]),
            t(&[("building:levels", "2")]),
            t(&[("name", "x")]),
        ] {
            assert!(!closed_way_is_area(&line), "{line:?}");
        }
    }

    #[test]
    fn rings_join_by_their_ends() {
        // Two rings from pieces in any order and direction, and a piece that closes nothing.
        let mut omitted = 0;
        let rings = join_rings(vec![vec![3, 4, 1], vec![10, 11], vec![1, 2], vec![20, 21, 22, 20], vec![3, 2]], &mut omitted);
        assert_eq!(omitted, 1);
        assert_eq!(rings.len(), 2, "{rings:?}");
        assert!(rings.iter().any(|r| r.len() == 5 && r.first() == r.last() && [1, 2, 3, 4].iter().all(|n| r.contains(n))), "{rings:?}");
        // Count source ways, independently of how their incomplete chains are visited.
        for pieces in [vec![vec![2, 3], vec![1, 2], vec![3, 4]], vec![vec![3, 4], vec![2, 3], vec![1, 2]]] {
            let mut omitted = 0;
            assert!(join_rings(pieces, &mut omitted).is_empty());
            assert_eq!(omitted, 3);
        }
        // A long chain joins in linear time.
        let n = 50_000i64;
        let mut chain: Vec<Vec<i64>> = (0..n).map(|i| vec![i, (i + 1) % n]).collect();
        chain.reverse();
        let rings = join_rings(chain, &mut 0);
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

    /// Read ahead, off the UI thread, the read stops when it is cancelled.
    #[test]
    fn a_cancelled_read_stops() {
        let probe = std::sync::Arc::new(crate::progress::tests::Counted::default());
        probe.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let _g = crate::progress::install(Some(probe));
        assert!(matches!(read("a.osm.pbf", &sample()), Err(DataError::Cancelled)));
    }
}
