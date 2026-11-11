//! Mapbox Vector Tile (spec v2.1) decoder, and an encoder ([`encode`]).
//!
//! Hand-written protobuf reader; transparently inflates gzip and zlib input. Hostile input never panics: malformed
//! wire data fails with `Err`, malformed geometry drops the one feature, out-of-range tags are ignored.

use miniz_oxide::inflate;

/// Cap on inflated tile size.
const MAX_INFLATE: usize = 32 << 20;
/// Cap on layers per tile.
const MAX_LAYERS: usize = 4096;
/// Cap on keys, values and features per layer.
const MAX_ITEMS: usize = 1 << 20;
/// Cap on property bytes cloned per tile (tags may reference one long string many times).
const PROP_BUDGET: usize = 64 << 20;

/// A decoded tile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tile {
    pub layers: Vec<Layer>,
}

/// One tile layer; coordinates are in `0..extent` tile units.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub features: Vec<Feature>,
}

/// One feature. Points: one single-point part each. Lines: one part per linestring. Polygons: one closed ring per
/// part, in wire order (exterior then its holes; see [`ring_area`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    pub id: Option<u64>,
    pub kind: GeomKind,
    pub geometry: Vec<Vec<[f32; 2]>>,
    pub props: Vec<(String, Value)>,
}

impl Feature {
    /// Property by key.
    pub fn prop(&self, key: &str) -> Option<&Value> {
        self.props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// Geometry type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GeomKind {
    #[default]
    Unknown,
    Point,
    LineString,
    Polygon,
}

/// Property value; every numeric wire type becomes `Number`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Number(f64),
    Bool(bool),
}

/// Decodes a tile, inflating gzip (`1f 8b`) or zlib input first.
pub fn decode(bytes: &[u8]) -> Result<Tile, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return parse_tile(&gunzip(bytes)?);
    }
    if is_zlib(bytes) {
        let raw = inflate::decompress_to_vec_zlib_with_limit(bytes, MAX_INFLATE).map_err(|e| format!("zlib: {:?}", e.status))?;
        return parse_tile(&raw);
    }
    parse_tile(bytes)
}

/// Inflates gzip or zlib bytes (bounded); other bytes are returned as they are.
pub(crate) fn inflate(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return gunzip(bytes);
    }
    if is_zlib(bytes) {
        return inflate::decompress_to_vec_zlib_with_limit(bytes, MAX_INFLATE).map_err(|e| format!("zlib: {:?}", e.status));
    }
    Ok(bytes.to_vec())
}

/// Encodes a tile (uncompressed). Coordinates are rounded to whole tile units; keys and values
/// are shared within each layer. Parts with too few points for their kind are left out (a line
/// needs two, a ring three besides its closing point), and so are features left with none.
/// Rings are written as given: exterior rings must wind clockwise (y down) and holes the other
/// way, as decoding tells them apart by their signed area ([`ring_area`]).
pub fn encode(tile: &Tile) -> Vec<u8> {
    let mut out = Vec::new();
    for l in &tile.layers {
        put_bytes(&mut out, 3, &encode_layer(l));
    }
    out
}

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    put_varint(out, field << 3 | 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn put_uint(out: &mut Vec<u8>, field: u64, v: u64) {
    put_varint(out, field << 3);
    put_varint(out, v);
}

fn encode_layer(l: &Layer) -> Vec<u8> {
    let mut keys: Vec<&str> = Vec::new();
    let mut values: Vec<Vec<u8>> = Vec::new();
    let mut key_at: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut value_at: std::collections::HashMap<Vec<u8>, usize> = std::collections::HashMap::new();
    let mut features = Vec::new();
    for f in &l.features {
        let Some(geom) = encode_geometry(f.kind, &f.geometry) else { continue };
        let mut tags = Vec::new();
        for (k, v) in &f.props {
            let ki = *key_at.entry(k.as_str()).or_insert_with(|| {
                keys.push(k);
                keys.len() - 1
            });
            let bytes = encode_value(v);
            let vi = *value_at.entry(bytes.clone()).or_insert_with(|| {
                values.push(bytes);
                values.len() - 1
            });
            put_varint(&mut tags, ki as u64);
            put_varint(&mut tags, vi as u64);
        }
        let mut fb = Vec::new();
        if let Some(id) = f.id {
            put_uint(&mut fb, 1, id);
        }
        if !tags.is_empty() {
            put_bytes(&mut fb, 2, &tags);
        }
        let kind = match f.kind {
            GeomKind::Point => 1,
            GeomKind::LineString => 2,
            _ => 3,
        };
        put_uint(&mut fb, 3, kind);
        put_bytes(&mut fb, 4, &geom);
        features.push(fb);
    }
    let mut out = Vec::new();
    put_uint(&mut out, 15, 2);
    put_bytes(&mut out, 1, l.name.as_bytes());
    for f in &features {
        put_bytes(&mut out, 2, f);
    }
    for k in &keys {
        put_bytes(&mut out, 3, k.as_bytes());
    }
    for v in &values {
        put_bytes(&mut out, 4, v);
    }
    put_uint(&mut out, 5, u64::from(l.extent));
    out
}

/// Whether a number is whole and small enough to be written (and shown) as an integer exactly.
pub fn is_whole(n: f64) -> bool {
    n.fract() == 0.0 && n.abs() < 9.0e15
}

fn encode_value(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match v {
        Value::String(s) => put_bytes(&mut out, 1, s.as_bytes()),
        // Whole numbers as zigzag integers, the rest as doubles.
        Value::Number(n) if is_whole(*n) => {
            let i = *n as i64;
            put_uint(&mut out, 6, ((i << 1) ^ (i >> 63)) as u64);
        }
        Value::Number(n) => {
            put_varint(&mut out, 3 << 3 | 1);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::Bool(b) => put_uint(&mut out, 7, u64::from(*b)),
    }
    out
}

/// The command stream of a feature's parts (packed), `None` when nothing is left of them.
fn encode_geometry(kind: GeomKind, parts: &[Vec<[f32; 2]>]) -> Option<Vec<u8>> {
    let mut cmds: Vec<u32> = Vec::new();
    let (mut cx, mut cy) = (0i64, 0i64);
    let mut step = |cmds: &mut Vec<u32>, p: &[f32; 2]| {
        // Within what a zigzag 32-bit delta holds (tile units are far smaller).
        let (x, y) = (p[0].round().clamp(-1e9, 1e9) as i64, p[1].round().clamp(-1e9, 1e9) as i64);
        let zz = |d: i64| ((d << 1) ^ (d >> 63)) as u32;
        cmds.push(zz(x - cx));
        cmds.push(zz(y - cy));
        (cx, cy) = (x, y);
    };
    match kind {
        GeomKind::Unknown => return None,
        GeomKind::Point => {
            let pts: Vec<&[f32; 2]> = parts.iter().flatten().collect();
            if pts.is_empty() {
                return None;
            }
            cmds.push(1 | (pts.len() as u32) << 3);
            for p in pts {
                step(&mut cmds, p);
            }
        }
        GeomKind::LineString | GeomKind::Polygon => {
            let closed = kind == GeomKind::Polygon;
            for part in parts {
                // A ring's closing point is implied by ClosePath.
                let pts = match part.split_last() {
                    Some((last, rest)) if closed && part.first() == Some(last) => rest,
                    _ => part.as_slice(),
                };
                if pts.len() < if closed { 3 } else { 2 } {
                    continue;
                }
                let Some((first, rest)) = pts.split_first() else { continue };
                cmds.push(1 | 1 << 3);
                step(&mut cmds, first);
                cmds.push(2 | (rest.len() as u32) << 3);
                for p in rest {
                    step(&mut cmds, p);
                }
                if closed {
                    cmds.push(7 | 1 << 3);
                }
            }
        }
    }
    if cmds.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for c in cmds {
        put_varint(&mut out, u64::from(c));
    }
    Some(out)
}

/// Signed ring area by the shoelace formula, in tile units. Positive = exterior ring (clockwise with y down),
/// negative = hole. Works on closed and open rings.
pub fn ring_area(ring: &[[f32; 2]]) -> f64 {
    let mut sum = 0.0f64;
    for w in ring.windows(2) {
        if let [a, b] = w {
            sum += f64::from(a[0]) * f64::from(b[1]) - f64::from(b[0]) * f64::from(a[1]);
        }
    }
    if let (Some(a), Some(b)) = (ring.last(), ring.first())
        && a != b
    {
        sum += f64::from(a[0]) * f64::from(b[1]) - f64::from(b[0]) * f64::from(a[1]);
    }
    sum / 2.0
}

fn is_zlib(b: &[u8]) -> bool {
    match (b.first(), b.get(1)) {
        (Some(&c), Some(&f)) => c & 0x0f == 8 && c >> 4 <= 7 && (u16::from(c) << 8 | u16::from(f)) % 31 == 0,
        _ => false,
    }
}

/// Strips the gzip header (RFC 1952, all flags) and inflates the deflate body; the trailer is ignored.
fn gunzip(b: &[u8]) -> Result<Vec<u8>, String> {
    gunzip_within(b, MAX_INFLATE)
}

/// Inflates gzip bytes to at most `max` bytes (more is an error, not a truncation).
pub fn gunzip_within(b: &[u8], max: usize) -> Result<Vec<u8>, String> {
    const TRUNC: &str = "truncated gzip header";
    if !b.starts_with(&[0x1f, 0x8b]) {
        return Err("not gzip".into());
    }
    if b.get(2) != Some(&8) {
        return Err("unsupported gzip compression method".into());
    }
    let flg = *b.get(3).ok_or(TRUNC)?;
    let mut pos = 10usize;
    if flg & 0x04 != 0 {
        let lo = *b.get(pos).ok_or(TRUNC)?;
        let hi = *b.get(pos + 1).ok_or(TRUNC)?;
        pos += 2 + usize::from(u16::from_le_bytes([lo, hi]));
    }
    for bit in [0x08u8, 0x10] {
        if flg & bit != 0 {
            let rest = b.get(pos..).ok_or(TRUNC)?;
            pos += rest.iter().position(|&c| c == 0).ok_or(TRUNC)? + 1;
        }
    }
    if flg & 0x02 != 0 {
        pos += 2;
    }
    let body = b.get(pos..).ok_or(TRUNC)?;
    inflate::decompress_to_vec_with_limit(body, max).map_err(|e| format!("gzip: {:?}", e.status))
}

/// Minimal protobuf wire reader.
struct Pb<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Pb<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Pb { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Varint of at most 10 bytes.
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = *self.buf.get(self.pos)?;
            self.pos += 1;
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// Field key as (field number, wire type); field 0 is invalid.
    fn key(&mut self) -> Option<(u32, u8)> {
        let k = self.varint()?;
        let field = u32::try_from(k >> 3).ok().filter(|f| *f != 0)?;
        Some((field, (k & 7) as u8))
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.varint()?).ok()?;
        self.take(n)
    }

    fn fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn skip(&mut self, wire: u8) -> Option<()> {
        match wire {
            0 => self.varint().map(drop),
            1 => self.take(8).map(drop),
            2 => self.bytes().map(drop),
            5 => self.take(4).map(drop),
            _ => None,
        }
    }

    /// Appends a packed (wire 2) or single (wire 0) repeated uint32 field.
    fn repeated_u32(&mut self, wire: u8, out: &mut Vec<u32>) -> Option<()> {
        match wire {
            0 => out.push(self.varint()? as u32),
            2 => {
                let mut inner = Pb::new(self.bytes()?);
                while !inner.done() {
                    out.push(inner.varint()? as u32);
                }
            }
            _ => return None,
        }
        Some(())
    }
}

fn parse_tile(buf: &[u8]) -> Result<Tile, String> {
    let mut pb = Pb::new(buf);
    let mut tile = Tile::default();
    let mut budget = PROP_BUDGET;
    while !pb.done() {
        let (field, wire) = pb.key().ok_or("malformed field key")?;
        if field == 3 && wire == 2 {
            let data = pb.bytes().ok_or("truncated layer")?;
            if tile.layers.len() < MAX_LAYERS {
                tile.layers.push(parse_layer(data, &mut budget)?);
            }
        } else {
            pb.skip(wire).ok_or("malformed tile field")?;
        }
    }
    Ok(tile)
}

fn parse_layer(buf: &[u8], budget: &mut usize) -> Result<Layer, String> {
    let mut pb = Pb::new(buf);
    let mut name = String::new();
    let mut extent = 4096u32;
    let mut keys: Vec<String> = Vec::new();
    let mut values: Vec<Option<Value>> = Vec::new();
    let mut raw: Vec<&[u8]> = Vec::new();
    while !pb.done() {
        let (field, wire) = pb.key().ok_or("malformed layer field key")?;
        match (field, wire) {
            (1, 2) => name = lossy(pb.bytes().ok_or("truncated layer name")?),
            (2, 2) => {
                let f = pb.bytes().ok_or("truncated feature")?;
                if raw.len() < MAX_ITEMS {
                    raw.push(f);
                }
            }
            (3, 2) => {
                let k = pb.bytes().ok_or("truncated key")?;
                if keys.len() < MAX_ITEMS {
                    keys.push(lossy(k));
                }
            }
            (4, 2) => {
                let v = pb.bytes().ok_or("truncated value")?;
                if values.len() < MAX_ITEMS {
                    values.push(parse_value(v));
                }
            }
            (5, 0) => {
                let e = pb.varint().ok_or("truncated extent")?;
                extent = u32::try_from(e).ok().filter(|e| *e > 0).unwrap_or(4096);
            }
            _ => pb.skip(wire).ok_or("malformed layer field")?,
        }
    }
    let features = raw.into_iter().filter_map(|f| parse_feature(f, &keys, &values, budget)).collect();
    Ok(Layer { name, extent, features })
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn parse_value(buf: &[u8]) -> Option<Value> {
    let mut pb = Pb::new(buf);
    let mut out = None;
    while !pb.done() {
        let (field, wire) = pb.key()?;
        out = match (field, wire) {
            (1, 2) => Some(Value::String(lossy(pb.bytes()?))),
            (2, 5) => Some(Value::Number(f64::from(f32::from_le_bytes(pb.fixed()?)))),
            (3, 1) => Some(Value::Number(f64::from_le_bytes(pb.fixed()?))),
            (4, 0) => Some(Value::Number(pb.varint()? as i64 as f64)),
            (5, 0) => Some(Value::Number(pb.varint()? as f64)),
            (6, 0) => {
                let n = pb.varint()?;
                Some(Value::Number(((n >> 1) as i64 ^ -((n & 1) as i64)) as f64))
            }
            (7, 0) => Some(Value::Bool(pb.varint()? != 0)),
            _ => {
                pb.skip(wire)?;
                out
            }
        };
    }
    out
}

/// Decodes one feature; `None` for malformed or empty geometry.
fn parse_feature(buf: &[u8], keys: &[String], values: &[Option<Value>], budget: &mut usize) -> Option<Feature> {
    let mut pb = Pb::new(buf);
    let mut id = None;
    let mut kind = GeomKind::Unknown;
    let mut tags = Vec::new();
    let mut geom = Vec::new();
    while !pb.done() {
        let (field, wire) = pb.key()?;
        match (field, wire) {
            (1, 0) => id = Some(pb.varint()?),
            (2, _) => pb.repeated_u32(wire, &mut tags)?,
            (3, 0) => {
                kind = match pb.varint()? {
                    1 => GeomKind::Point,
                    2 => GeomKind::LineString,
                    3 => GeomKind::Polygon,
                    _ => GeomKind::Unknown,
                }
            }
            (4, _) => pb.repeated_u32(wire, &mut geom)?,
            _ => pb.skip(wire)?,
        }
    }
    let geometry = decode_geometry(kind, &geom)?;
    if geometry.is_empty() {
        return None;
    }
    let mut props = Vec::new();
    for pair in tags.as_chunks::<2>().0 {
        let (Some(&k), Some(&v)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let (Some(key), Some(Some(val))) = (keys.get(k as usize), values.get(v as usize)) else {
            continue;
        };
        let cost = key.len() + if let Value::String(s) = val { s.len() } else { 8 } + 16;
        if cost > *budget {
            break;
        }
        *budget -= cost;
        props.push((key.clone(), val.clone()));
    }
    Some(Feature { id, kind, geometry, props })
}

fn zigzag(n: u32) -> i64 {
    i64::from((n >> 1) as i32 ^ -((n & 1) as i32))
}

/// Runs the geometry command stream. `None` for malformed streams and unknown geometry types.
fn decode_geometry(kind: GeomKind, cmds: &[u32]) -> Option<Vec<Vec<[f32; 2]>>> {
    if kind == GeomKind::Unknown {
        return None;
    }
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let (mut x, mut y) = (0i64, 0i64);
    let mut it = cmds.iter().copied();
    while let Some(c) = it.next() {
        let (id, count) = (c & 7, c >> 3);
        match id {
            1 | 2 => {
                if id == 1 && kind != GeomKind::Point {
                    if count != 1 {
                        return None;
                    }
                    if kind == GeomKind::LineString && cur.len() >= 2 {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
                if id == 2 && (kind == GeomKind::Point || cur.is_empty()) {
                    return None;
                }
                for _ in 0..count {
                    x = x.saturating_add(zigzag(it.next()?));
                    y = y.saturating_add(zigzag(it.next()?));
                    let p = [x as f32, y as f32];
                    if kind == GeomKind::Point {
                        out.push(vec![p]);
                    } else {
                        cur.push(p);
                    }
                }
            }
            7 => {
                if kind != GeomKind::Polygon || count != 1 {
                    return None;
                }
                if cur.len() >= 3
                    && let Some(&first) = cur.first()
                {
                    cur.push(first);
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
            _ => return None,
        }
    }
    if kind == GeomKind::LineString && cur.len() >= 2 {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_tiles_decode_to_themselves() {
        let f = |id, kind, geometry: Vec<Vec<[f32; 2]>>, props: Vec<(&str, Value)>| Feature {
            id,
            kind,
            geometry,
            props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        };
        let tile = Tile {
            layers: vec![
                Layer {
                    name: "parks".into(),
                    extent: 4096,
                    features: vec![
                        // A polygon with a hole, its rings closed as decoding gives them.
                        f(
                            Some(1),
                            GeomKind::Polygon,
                            vec![
                                vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0], [0.0, 0.0]],
                                vec![[20.0, 20.0], [20.0, 80.0], [80.0, 80.0], [20.0, 20.0]],
                            ],
                            vec![("name", Value::String("Central".into())), ("area", Value::Number(1.5)), ("open", Value::Bool(true))],
                        ),
                        f(
                            None,
                            GeomKind::LineString,
                            vec![vec![[-10.0, 5.0], [4200.0, 5.0]], vec![[1.0, 1.0], [2.0, 3.0]]],
                            vec![("lanes", Value::Number(-2.0))],
                        ),
                    ],
                },
                Layer {
                    name: "pois".into(),
                    extent: 512,
                    features: vec![f(
                        Some(9),
                        GeomKind::Point,
                        vec![vec![[3.0, 4.0]], vec![[5.0, 6.0]]],
                        vec![("name", Value::String("Central".into()))],
                    )],
                },
            ],
        };
        assert_eq!(decode(&encode(&tile)).unwrap(), tile);
        // Parts too short for their kind go; a feature left with none goes too.
        let short = Tile {
            layers: vec![Layer {
                name: "x".into(),
                extent: 4096,
                features: vec![
                    f(None, GeomKind::Polygon, vec![vec![[0.0, 0.0], [1.0, 1.0], [0.0, 0.0]]], vec![]),
                    f(None, GeomKind::LineString, vec![vec![[0.0, 0.0]]], vec![]),
                ],
            }],
        };
        assert!(decode(&encode(&short)).unwrap().layers[0].features.is_empty());
    }

    // ---- tiny test-only encoder ----

    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn key(out: &mut Vec<u8>, field: u32, wire: u8) {
        varint(out, u64::from(field) << 3 | u64::from(wire));
    }

    fn len_field(out: &mut Vec<u8>, field: u32, data: &[u8]) {
        key(out, field, 2);
        varint(out, data.len() as u64);
        out.extend_from_slice(data);
    }

    fn packed(vals: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for &v in vals {
            varint(&mut out, u64::from(v));
        }
        out
    }

    fn zz(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }

    fn cmd(id: u32, count: u32) -> u32 {
        id | count << 3
    }

    /// MoveTo + LineTo… (+ ClosePath) for one part, with deltas from `cursor`.
    fn part(cursor: &mut [i32; 2], pts: &[[i32; 2]], close: bool) -> Vec<u32> {
        let mut g = Vec::new();
        for (i, p) in pts.iter().enumerate() {
            if i == 0 {
                g.push(cmd(1, 1));
            } else if i == 1 {
                g.push(cmd(2, pts.len() as u32 - 1));
            }
            g.push(zz(p[0] - cursor[0]));
            g.push(zz(p[1] - cursor[1]));
            *cursor = *p;
        }
        if close {
            g.push(cmd(7, 1));
        }
        g
    }

    enum V {
        S(&'static str),
        F(f32),
        D(f64),
        I(i64),
        U(u64),
        Z(i64),
        B(bool),
    }

    fn value(v: &V) -> Vec<u8> {
        let mut o = Vec::new();
        match v {
            V::S(s) => len_field(&mut o, 1, s.as_bytes()),
            V::F(f) => {
                key(&mut o, 2, 5);
                o.extend_from_slice(&f.to_le_bytes());
            }
            V::D(d) => {
                key(&mut o, 3, 1);
                o.extend_from_slice(&d.to_le_bytes());
            }
            V::I(i) => {
                key(&mut o, 4, 0);
                varint(&mut o, *i as u64);
            }
            V::U(u) => {
                key(&mut o, 5, 0);
                varint(&mut o, *u);
            }
            V::Z(z) => {
                key(&mut o, 6, 0);
                varint(&mut o, ((z << 1) ^ (z >> 63)) as u64);
            }
            V::B(b) => {
                key(&mut o, 7, 0);
                varint(&mut o, u64::from(*b));
            }
        }
        o
    }

    fn feature(id: Option<u64>, kind: u64, tags: &[u32], geom: &[u32]) -> Vec<u8> {
        let mut o = Vec::new();
        if let Some(id) = id {
            key(&mut o, 1, 0);
            varint(&mut o, id);
        }
        if !tags.is_empty() {
            len_field(&mut o, 2, &packed(tags));
        }
        key(&mut o, 3, 0);
        varint(&mut o, kind);
        len_field(&mut o, 4, &packed(geom));
        o
    }

    fn layer(name: &str, extent: Option<u32>, keys: &[&str], values: &[V], features: &[Vec<u8>]) -> Vec<u8> {
        let mut o = Vec::new();
        key(&mut o, 15, 0);
        varint(&mut o, 2);
        len_field(&mut o, 1, name.as_bytes());
        for f in features {
            len_field(&mut o, 2, f);
        }
        for k in keys {
            len_field(&mut o, 3, k.as_bytes());
        }
        for v in values {
            len_field(&mut o, 4, &value(v));
        }
        if let Some(e) = extent {
            key(&mut o, 5, 0);
            varint(&mut o, u64::from(e));
        }
        o
    }

    fn tile(layers: &[Vec<u8>]) -> Vec<u8> {
        let mut o = Vec::new();
        for l in layers {
            len_field(&mut o, 3, l);
        }
        o
    }

    fn sample_tile() -> Vec<u8> {
        let pts = feature(Some(7), 1, &[0, 0], &[cmd(1, 2), zz(5), zz(7), zz(3), zz(2)]);
        let mut c = [0, 0];
        let mut lines = part(&mut c, &[[2, 2], [2, 10], [10, 10]], false);
        lines.extend(part(&mut c, &[[1, 1], [3, 5]], false));
        let line = feature(None, 2, &[1, 1, 0, 2], &lines);
        c = [0, 0];
        let mut poly = part(&mut c, &[[0, 0], [10, 0], [10, 10], [0, 10]], true);
        poly.extend(part(&mut c, &[[2, 2], [2, 8], [8, 8], [8, 2]], true));
        let polygon = feature(Some(u64::MAX), 3, &[0, 3, 99, 0, 1, 99], &poly);
        let l = layer("water", None, &["name", "class"], &[V::S("Lake"), V::S("river"), V::B(true), V::I(-3)], &[pts, line, polygon]);
        tile(&[l])
    }

    #[test]
    fn points_lines_polygons() {
        let t = decode(&sample_tile()).unwrap();
        assert_eq!(t.layers.len(), 1);
        let l = &t.layers[0];
        assert_eq!((l.name.as_str(), l.extent), ("water", 4096));
        assert_eq!(l.features.len(), 3);

        let p = &l.features[0];
        assert_eq!((p.id, p.kind), (Some(7), GeomKind::Point));
        assert_eq!(p.geometry, vec![vec![[5.0, 7.0]], vec![[8.0, 9.0]]]);
        assert_eq!(p.prop("name"), Some(&Value::String("Lake".into())));

        let ln = &l.features[1];
        assert_eq!(ln.kind, GeomKind::LineString);
        assert_eq!(ln.geometry, vec![vec![[2.0, 2.0], [2.0, 10.0], [10.0, 10.0]], vec![[1.0, 1.0], [3.0, 5.0]]]);
        assert_eq!(ln.props, vec![("class".into(), Value::String("river".into())), ("name".into(), Value::Bool(true))]);

        let pg = &l.features[2];
        assert_eq!((pg.id, pg.kind), (Some(u64::MAX), GeomKind::Polygon));
        assert_eq!(pg.geometry.len(), 2);
        assert_eq!(pg.geometry[0].len(), 5);
        assert_eq!(pg.geometry[0].first(), pg.geometry[0].last());
        assert!(ring_area(&pg.geometry[0]) > 0.0, "exterior is positive");
        assert!(ring_area(&pg.geometry[1]) < 0.0, "hole is negative");
        assert_eq!(ring_area(&pg.geometry[0]), 100.0);
        assert_eq!(ring_area(&pg.geometry[1]), -36.0);
        // Out-of-range key/value indices are dropped.
        assert_eq!(pg.props, vec![("name".into(), Value::Number(-3.0))]);
    }

    #[test]
    fn all_value_types_and_extent() {
        let vals = [V::S("s"), V::F(1.5), V::D(-2.25), V::I(-42), V::U(u64::from(u32::MAX) + 1), V::Z(-17), V::B(false)];
        let keys = ["a", "b", "c", "d", "e", "f", "g"];
        let tags: Vec<u32> = (0..7).flat_map(|i| [i, i]).collect();
        let f = feature(None, 1, &tags, &[cmd(1, 1), zz(1), zz(1)]);
        let t = decode(&tile(&[layer("v", Some(512), &keys, &vals, &[f])])).unwrap();
        let l = &t.layers[0];
        assert_eq!(l.extent, 512);
        let p: Vec<&Value> = l.features[0].props.iter().map(|(_, v)| v).collect();
        assert_eq!(
            p,
            [
                &Value::String("s".into()),
                &Value::Number(1.5),
                &Value::Number(-2.25),
                &Value::Number(-42.0),
                &Value::Number(4294967296.0),
                &Value::Number(-17.0),
                &Value::Bool(false)
            ]
        );
    }

    #[test]
    fn unknown_fields_are_skipped() {
        let mut l = layer("x", None, &[], &[], &[feature(None, 1, &[], &[cmd(1, 1), 0, 0])]);
        key(&mut l, 9, 1);
        l.extend_from_slice(&[0; 8]);
        key(&mut l, 10, 5);
        l.extend_from_slice(&[0; 4]);
        let mut t = tile(&[l]);
        key(&mut t, 20, 0);
        varint(&mut t, 300);
        len_field(&mut t, 21, b"junk");
        let d = decode(&t).unwrap();
        assert_eq!(d.layers[0].features.len(), 1);
    }

    fn gzip(data: &[u8], flags: u8) -> Vec<u8> {
        let mut o = vec![0x1f, 0x8b, 8, flags, 0, 0, 0, 0, 0, 3];
        if flags & 4 != 0 {
            o.extend_from_slice(&[3, 0, 1, 2, 3]);
        }
        if flags & 8 != 0 {
            o.extend_from_slice(b"tile.pbf\0");
        }
        if flags & 16 != 0 {
            o.extend_from_slice(b"a comment\0");
        }
        if flags & 2 != 0 {
            o.extend_from_slice(&[0xab, 0xcd]);
        }
        o.extend(miniz_oxide::deflate::compress_to_vec(data, 6));
        o.extend_from_slice(&[0; 4]);
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o
    }

    #[test]
    fn gzip_and_zlib_wrapped() {
        let raw = sample_tile();
        let want = decode(&raw).unwrap();
        for flags in [0u8, 2, 4, 8, 16, 0x1e] {
            assert_eq!(decode(&gzip(&raw, flags)).unwrap(), want, "flags {flags:#x}");
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        assert_eq!(decode(&z).unwrap(), want);
        assert!(decode(&[0x1f, 0x8b]).is_err());
        assert!(decode(&[0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 0, b'a']).is_err());
        let g = gzip(&raw, 0);
        assert!(decode(&g[..g.len() / 2]).is_err());
    }

    #[test]
    fn malformed_geometry_skips_feature_only() {
        let bad = [
            feature(None, 2, &[], &[cmd(2, 1), 0, 0]),                             // LineTo before MoveTo
            feature(None, 1, &[], &[cmd(1, 2), 1, 1]),                             // missing params
            feature(None, 1, &[], &[cmd(7, 1)]),                                   // ClosePath on point
            feature(None, 3, &[], &[cmd(1, 1), 0, 0, cmd(5, 1)]),                  // unknown command
            feature(None, 0, &[], &[cmd(1, 1), 0, 0]),                             // unknown type
            feature(None, 3, &[], &[cmd(1, 1), 0, 0, cmd(2, 1), 2, 2, cmd(7, 1)]), // degenerate ring
        ];
        let mut fs = bad.to_vec();
        fs.push(feature(None, 1, &[], &[cmd(1, 1), 2, 2]));
        let t = decode(&tile(&[layer("x", None, &[], &[], &fs)])).unwrap();
        assert_eq!(t.layers[0].features.len(), 1);
        assert_eq!(t.layers[0].features[0].geometry, vec![vec![[1.0, 1.0]]]);
    }

    #[test]
    fn junk_and_truncated_never_panic() {
        assert_eq!(decode(&[]).unwrap(), Tile::default());
        assert!(decode(&[0x1a, 0x05, 1]).is_err());
        assert!(decode(&[0xff; 11]).is_err());
        assert!(decode(&[0x1a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]).is_err());
        let t = sample_tile();
        for n in 0..t.len() {
            let _ = decode(&t[..n]);
        }
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    #[test]
    fn fuzz_random_and_mutated() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..3000 {
            let n = rng.below(256) as usize;
            let mut b: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            if rng.below(4) == 0 {
                b.splice(0..0, [0x1a]);
            }
            let _ = decode(&b);
        }
        let base = sample_tile();
        let gz = gzip(&base, 0x1e);
        for _ in 0..3000 {
            let mut b = if rng.below(5) == 0 { gz.clone() } else { base.clone() };
            for _ in 0..=rng.below(4) {
                let len = b.len() as u64;
                match rng.below(4) {
                    0 => {
                        let i = rng.below(len) as usize;
                        if let Some(x) = b.get_mut(i) {
                            *x ^= 1 << rng.below(8);
                        }
                    }
                    1 => {
                        let i = rng.below(len) as usize;
                        if let Some(x) = b.get_mut(i) {
                            *x = rng.next() as u8;
                        }
                    }
                    2 => b.truncate(rng.below(len + 1) as usize),
                    _ => {
                        let i = rng.below(len + 1) as usize;
                        b.insert(i, rng.next() as u8);
                    }
                }
            }
            if let Ok(t) = decode(&b) {
                for f in t.layers.iter().flat_map(|l| &l.features) {
                    assert!(!f.geometry.is_empty());
                }
            }
        }
    }
}
