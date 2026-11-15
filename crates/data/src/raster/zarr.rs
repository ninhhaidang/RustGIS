//! Zarr stores (format 2 and 3): a variable's 2D slice — latitude/longitude or y/x, its other
//! dimensions (time, level) picked by index — read as a raster.
//!
//! The store is read in steps so a caller can fetch its files however it reaches them (a folder,
//! the web): [`METADATA_KEYS`] and [`parse_store`], then [`plan`] names the chunk files a slice
//! needs and [`assemble`] decodes them. Chunks are decoded in pure Rust: blosc (LZ4, zlib, zstd,
//! with byte shuffle), zlib, gzip, zstd and LZ4. Every count, size and length in a store is
//! hostile input and bounded.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::{ColorHint, Crs, Exact, GeoTransform, Raster, max_samples};
use crate::date::{civil_from_days, days_from_civil};
use crate::{DataError, Result};

/// The metadata files a store may have at its root: Zarr 3's `zarr.json`, Zarr 2's consolidated
/// `.zmetadata`, and a lone Zarr 2 array's or group's own files.
pub const METADATA_KEYS: [&str; 5] = ["zarr.json", ".zmetadata", ".zarray", ".zattrs", ".zgroup"];

/// Largest metadata document read.
pub const MAX_METADATA: usize = 32 << 20;
/// Most arrays a store may hold.
const MAX_ARRAYS: usize = 10_000;
/// Most dimensions of an array.
const MAX_DIMS: usize = 32;
/// Most chunk files a read fetches.
pub const MAX_CHUNKS: usize = 4096;
/// Most bytes a chunk may hold, decoded (a slice reads only its values out of them).
#[cfg(not(target_arch = "wasm32"))]
const MAX_CHUNK_BYTES: u64 = 1 << 30;
#[cfg(target_arch = "wasm32")]
const MAX_CHUNK_BYTES: u64 = 256 << 20;
/// Longest coordinate array read (the labels of a time dimension, say).
const MAX_COORDS: u64 = 4_000_000;
/// Largest chunk file read from a folder (stored, before decoding).
pub const MAX_CHUNK_FILE: u64 = MAX_CHUNK_BYTES;
/// Most bytes of chunk files one read takes from a folder.
pub const MAX_READ: u64 = 4 << 30;

fn bad(m: impl Into<String>) -> DataError {
    DataError::Format(m.into())
}

/// How a value is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    UInt,
    Float,
    Bool,
}

/// A data type: its kind, size in bytes and byte order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DType {
    pub kind: Kind,
    pub size: usize,
    pub little: bool,
}

impl DType {
    /// Zarr 2's `<f4`, `|u1`, `>i2`…
    fn parse_v2(s: &str) -> Option<DType> {
        let b = s.as_bytes();
        let (order, kind, size) = (*b.first()?, *b.get(1)?, s.get(2..)?.parse::<usize>().ok()?);
        let little = order != b'>';
        let kind = match kind {
            b'i' => Kind::Int,
            b'u' => Kind::UInt,
            b'f' => Kind::Float,
            b'b' => Kind::Bool,
            _ => return None,
        };
        DType { kind, size, little }.valid()
    }

    /// Zarr 3's `float32`, `uint8`…
    fn parse_v3(s: &str) -> Option<DType> {
        let (kind, bits) = if let Some(b) = s.strip_prefix("uint") {
            (Kind::UInt, b)
        } else if let Some(b) = s.strip_prefix("int") {
            (Kind::Int, b)
        } else if let Some(b) = s.strip_prefix("float") {
            (Kind::Float, b)
        } else if s == "bool" {
            (Kind::Bool, "8")
        } else {
            return None;
        };
        DType { kind, size: bits.parse::<usize>().ok()? / 8, little: true }.valid()
    }

    /// `v` stored as this type, in the byte order given (a fill value).
    fn encode(self, v: f64, little: bool) -> Vec<u8> {
        let mut b = match (self.kind, self.size) {
            (Kind::Float, 8) => v.to_le_bytes().to_vec(),
            (Kind::Float, 4) => (v as f32).to_le_bytes().to_vec(),
            (Kind::Float, _) => to_f16(v).to_le_bytes().to_vec(),
            (Kind::Int, n) => (v as i64).to_le_bytes().get(..n).unwrap_or_default().to_vec(),
            (Kind::UInt | Kind::Bool, n) => (v as u64).to_le_bytes().get(..n).unwrap_or_default().to_vec(),
        };
        if !little {
            b.reverse();
        }
        b
    }

    fn valid(self) -> Option<DType> {
        let ok = match self.kind {
            Kind::Int | Kind::UInt => matches!(self.size, 1 | 2 | 4 | 8),
            Kind::Float => matches!(self.size, 2 | 4 | 8),
            Kind::Bool => self.size == 1,
        };
        ok.then_some(self)
    }

    /// A value from its bytes (exactly `size` of them).
    fn value(self, b: &[u8]) -> f64 {
        let mut a = [0u8; 8];
        let n = self.size.min(8);
        if let Some(src) = b.get(..n) {
            if self.little {
                a[..n].copy_from_slice(src);
            } else {
                for (i, v) in src.iter().rev().enumerate() {
                    a[i] = *v;
                }
            }
        }
        // Sign extension for the signed kinds, from the little-endian bytes.
        let sign = |bits: u32| {
            let u = u64::from_le_bytes(a);
            let shift = 64 - bits;
            ((u << shift) as i64 >> shift) as f64
        };
        match (self.kind, self.size) {
            (Kind::Bool, _) | (Kind::UInt, 1) => f64::from(a[0]),
            (Kind::UInt, 2) => f64::from(u16::from_le_bytes([a[0], a[1]])),
            (Kind::UInt, 4) => f64::from(u32::from_le_bytes([a[0], a[1], a[2], a[3]])),
            (Kind::UInt, _) => u64::from_le_bytes(a) as f64,
            (Kind::Int, n) => sign((n * 8) as u32),
            (Kind::Float, 2) => f16(u16::from_le_bytes([a[0], a[1]])),
            (Kind::Float, 4) => f64::from(f32::from_le_bytes([a[0], a[1], a[2], a[3]])),
            (Kind::Float, _) => f64::from_le_bytes(a),
        }
    }

    /// A whole number's exact value from its bytes (exactly `size` of them): past 2^53,
    /// [`DType::value`]'s `f64` has already rounded 64-bit ones. `None` for floats and booleans.
    fn whole(self, b: &[u8]) -> Option<i128> {
        if !matches!(self.kind, Kind::Int | Kind::UInt) {
            return None;
        }
        let n = self.size.min(8);
        let src = b.get(..n)?;
        let mut a = [0u8; 8];
        for (i, v) in src.iter().enumerate() {
            if let Some(slot) = a.get_mut(if self.little { i } else { n - 1 - i }) {
                *slot = *v;
            }
        }
        let u = u64::from_le_bytes(a);
        let shift = 64 - 8 * n as u32;
        Some(if self.kind == Kind::Int { i128::from(((u << shift) as i64) >> shift) } else { i128::from(u) })
    }

    /// Whether `f32` holds the value in `b` (exactly `size` bytes) exactly; whole numbers are
    /// compared as integers.
    fn held_as_f32(self, b: &[u8]) -> bool {
        if let Some(i) = self.whole(b) {
            return i as f32 as i128 == i;
        }
        let v = self.value(b);
        v.is_nan() || f64::from(v as f32) == v
    }
}

/// A half-precision float's value.
fn f16(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f64::from(h & 0x3ff);
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 if frac == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1.0 + frac / 1024.0) * 2f64.powi(e - 15),
    }
}

/// A half-precision float's bits (rounded towards zero).
fn to_f16(v: f64) -> u16 {
    let x = v as f32;
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    if x.is_nan() {
        return sign | 0x7e00;
    }
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let m = b & 0x7f_ffff;
    if e >= 31 {
        sign | 0x7c00
    } else if e <= 0 {
        if e < -10 { sign } else { sign | ((m | 0x80_0000) >> (14 - e)) as u16 }
    } else {
        sign | ((e as u16) << 10) | (m >> 13) as u16
    }
}

/// A step of a chunk's encoding, undone in reverse order.
#[derive(Clone, Debug, PartialEq)]
pub enum Codec {
    Blosc,
    Zlib,
    Gzip,
    Zstd,
    /// numcodecs' LZ4: a little-endian size, then an LZ4 block.
    Lz4,
    /// numcodecs' byte shuffle of `n`-byte elements.
    Shuffle(usize),
    /// Zarr 3's checksum after the data, dropped.
    Crc32c,
    /// Zarr 3's `bytes` codec: the values' byte order.
    Bytes {
        little: bool,
    },
    /// Zarr 3's sharding: the chunk (a shard) holds smaller chunks of `inner` values, each
    /// encoded with `codecs`, and an index of where they are (`index` its encoding, at the end or
    /// the start).
    Sharding {
        inner: Vec<u64>,
        codecs: Vec<Codec>,
        index: Vec<Codec>,
        index_at_end: bool,
    },
    /// A codec this reader doesn't know: the array can't be read, and this says why.
    Unsupported(String),
}

/// An array of a store.
#[derive(Clone, Debug)]
pub struct Array {
    /// Its path in the store (`2m_temperature`, `group/var`; empty for a store that is one array).
    pub path: String,
    pub zarr_format: u8,
    pub shape: Vec<u64>,
    pub chunks: Vec<u64>,
    pub dtype: DType,
    pub fill: Option<f64>,
    /// `fill` exactly as the metadata writes it (a whole number past 2^53 stays whole).
    pub(crate) fill_exact: Option<Exact>,
    /// Fortran order (the first dimension varies fastest).
    pub fortran: bool,
    /// The encoding, in the order it was applied.
    pub codecs: Vec<Codec>,
    /// Its dimensions' names (`_ARRAY_DIMENSIONS`, `dimension_names`), when it says.
    pub dims: Vec<String>,
    pub attrs: Map<String, Value>,
    /// Chunk keys: a prefix (`c/` in Zarr 3) and the separator between indexes.
    key_prefix: String,
    separator: char,
}

impl Array {
    /// Its name: the last part of its path.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Its dimension names, made up (`dim_0`…) where it doesn't say.
    pub fn dim_names(&self) -> Vec<String> {
        (0..self.shape.len()).map(|i| self.dims.get(i).filter(|d| !d.is_empty()).cloned().unwrap_or_else(|| format!("dim_{i}"))).collect()
    }

    /// Why it can't be read, if it can't.
    pub fn unsupported(&self) -> Option<String> {
        unsupported(&self.codecs)
    }

    /// The key of a chunk, relative to the store's root.
    pub fn chunk_key(&self, idx: &[u64]) -> String {
        let parts: Vec<String> = idx.iter().map(u64::to_string).collect();
        // A 0-dimensional array's one chunk is `0` in Zarr 2 and `c` in Zarr 3.
        let body = if parts.is_empty() && self.zarr_format == 2 { "0".to_string() } else { parts.join(&self.separator.to_string()) };
        let key = format!("{}{}", self.key_prefix, body).trim_end_matches('/').to_string();
        if self.path.is_empty() { key } else { format!("{}/{key}", self.path) }
    }

    /// How many chunks lie along dimension `d`.
    fn grid(&self, d: usize) -> u64 {
        let (s, c) = (self.shape.get(d).copied().unwrap_or(0), self.chunks.get(d).copied().unwrap_or(1).max(1));
        s.div_ceil(c)
    }

    fn chunk_values(&self) -> Result<u64> {
        let n = self.chunks.iter().try_fold(1u64, |a, &c| a.checked_mul(c)).ok_or_else(|| bad("a chunk too large"))?;
        if n.saturating_mul(self.dtype.size as u64) > MAX_CHUNK_BYTES {
            return Err(bad(format!("{}: chunks of {n} values are too large to read", self.path)));
        }
        Ok(n)
    }

    /// Decodes a chunk's file into its values (row-major or column-major, as stored).
    pub fn decode(&self, bytes: &[u8]) -> Result<Vec<f64>> {
        let (data, dt) = self.decode_bytes(bytes)?;
        Ok(data.chunks_exact(dt.size).map(|b| dt.value(b)).collect())
    }

    /// Decodes a chunk's file into its values' bytes and their type (byte order included):
    /// value `i` is at `i * size`.
    pub fn decode_bytes(&self, bytes: &[u8]) -> Result<(Vec<u8>, DType)> {
        self.chunk_values()?;
        let (data, little) = decode_chunk(&self.codecs, bytes, &self.chunks, self.dtype, self.fill).map_err(|e| match e {
            e if self.path.is_empty() => e,
            DataError::Format(m) => bad(format!("{}: {m}", self.path)),
            e => bad(format!("{}: {e}", self.path)),
        })?;
        Ok((data, DType { little, ..self.dtype }))
    }
}

/// Why chunks encoded with `codecs` can't be read, if they can't.
fn unsupported(codecs: &[Codec]) -> Option<String> {
    codecs.iter().find_map(|c| match c {
        Codec::Unsupported(why) => Some(why.clone()),
        Codec::Sharding { codecs, index, .. } => unsupported(codecs).or_else(|| unsupported(index)),
        _ => None,
    })
}

/// Undoes `codecs` on a chunk of `shape` values of type `dtype`: its values' bytes (as many as
/// the shape holds, never more) and their byte order. `fill` stands for a shard's missing chunks.
fn decode_chunk(codecs: &[Codec], bytes: &[u8], shape: &[u64], dtype: DType, fill: Option<f64>) -> Result<(Vec<u8>, bool)> {
    let n = shape.iter().try_fold(1u64, |a, &c| a.checked_mul(c)).ok_or_else(|| bad("a chunk too large"))?;
    let want = usize::try_from(n).ok().and_then(|n| n.checked_mul(dtype.size)).ok_or_else(|| bad("a chunk too large"))?;
    let mut data = bytes.to_vec();
    let mut little = dtype.little;
    for c in codecs.iter().rev() {
        data = match c {
            Codec::Blosc => blosc(&data, want)?,
            Codec::Zlib => inflate(&data, true, want)?,
            Codec::Gzip => gunzip(&data, want)?,
            Codec::Zstd => unzstd(&data, want)?,
            Codec::Lz4 => numcodecs_lz4(&data, want)?,
            Codec::Shuffle(e) => unshuffle(&data, *e),
            Codec::Crc32c => data.get(..data.len().saturating_sub(4)).unwrap_or_default().to_vec(),
            Codec::Bytes { little: l } => {
                little = *l;
                data
            }
            Codec::Sharding { inner, codecs, index, index_at_end } => {
                let (d, l) = unshard(&data, shape, inner, codecs, index, *index_at_end, dtype, fill)?;
                little = l;
                d
            }
            Codec::Unsupported(why) => return Err(bad(why.clone())),
        };
    }
    if data.len() < want {
        return Err(bad(format!("a chunk holds {} bytes, not {want}", data.len())));
    }
    data.truncate(want);
    Ok((data, little))
}

/// A store's arrays.
#[derive(Clone, Debug, Default)]
pub struct Store {
    pub arrays: Vec<Array>,
}

impl Store {
    pub fn array(&self, path: &str) -> Option<&Array> {
        self.arrays.iter().find(|a| a.path == path).or_else(|| self.arrays.iter().find(|a| a.name() == path))
    }

    /// The arrays that are dimensions' coordinates (`lat`, `time`): 1D arrays named like a
    /// dimension of another array.
    fn is_coordinate(&self, a: &Array) -> bool {
        a.shape.len() == 1 && self.arrays.iter().any(|o| o.shape.len() > 1 && o.dim_names().iter().any(|d| d == a.name()))
    }

    /// Cell bounds (`lat_bnds`, CF's `bounds` attribute): coordinates' edges, not maps.
    fn is_bounds(&self, a: &Array) -> bool {
        let name = a.name().to_ascii_lowercase();
        name.ends_with("_bnds")
            || name.ends_with("_bounds")
            || a.dim_names()
                .iter()
                .any(|d| matches!(d.to_ascii_lowercase().as_str(), "bnds" | "nbnd" | "nbnds" | "bounds" | "nv" | "vertices"))
            || self.arrays.iter().any(|o| o.attrs.get("bounds").and_then(Value::as_str) == Some(a.name()))
    }

    /// The arrays that can be shown: two or more dimensions, not coordinates or their bounds.
    pub fn variables(&self) -> Vec<&Array> {
        self.arrays.iter().filter(|a| a.shape.len() >= 2 && !self.is_coordinate(a) && !self.is_bounds(a)).collect()
    }

    /// A dimension's coordinate array, for `of` (in its group first).
    fn coordinate(&self, of: &Array, dim: &str) -> Option<&Array> {
        let group = of.path.rsplit_once('/').map(|(g, _)| g.to_string());
        let in_group = group.as_ref().map(|g| format!("{g}/{dim}"));
        self.arrays
            .iter()
            .find(|a| a.shape.len() == 1 && Some(&a.path) == in_group.as_ref())
            .or_else(|| self.arrays.iter().find(|a| a.shape.len() == 1 && a.path == dim))
    }
}

/// Reads a store's metadata: `docs` are the metadata files found, by key from the root
/// (`zarr.json`, `.zmetadata`, or each array's `.zarray` and `.zattrs`, or `zarr.json`).
/// Whether a path from a store's metadata stays inside the store: names joined by `/`, none of
/// them `.`, `..` or empty, with no root, backslash or drive (`C:`). Paths name files under a
/// folder and addresses under a web store, so one that climbs out is refused.
fn safe_path(p: &str) -> bool {
    p.is_empty() || (!p.starts_with('/') && !p.contains(['\\', ':', '\0']) && p.split('/').all(|s| !s.is_empty() && s != "." && s != ".."))
}

pub fn parse_store(docs: &[(String, Vec<u8>)]) -> Result<Store> {
    let mut json: HashMap<String, Value> = HashMap::new();
    for (k, b) in docs.iter().take(4 * MAX_ARRAYS) {
        if b.len() > MAX_METADATA {
            return Err(bad(format!("{k} is too large")));
        }
        if let Ok(v) = serde_json::from_slice::<Value>(b) {
            json.insert(k.trim_start_matches('/').to_string(), v);
        }
    }
    // Zarr 2's consolidated metadata holds every array's files.
    if let Some(Value::Object(m)) = json.get(".zmetadata").and_then(|z| z.get("metadata")).cloned() {
        for (k, v) in m.into_iter().take(4 * MAX_ARRAYS) {
            json.entry(k).or_insert(v);
        }
    }
    // Zarr 3's consolidated metadata, in the root group's zarr.json.
    if let Some(Value::Object(m)) = json.get("zarr.json").and_then(|z| z.pointer("/consolidated_metadata/metadata")).cloned() {
        for (k, v) in m.into_iter().take(MAX_ARRAYS) {
            json.entry(format!("{}/zarr.json", k.trim_matches('/'))).or_insert(v);
        }
    }
    let mut arrays = Vec::new();
    let mut keys: Vec<&String> = json.keys().collect();
    keys.sort();
    for k in keys {
        if arrays.len() >= MAX_ARRAYS {
            break;
        }
        let Some(v) = json.get(k) else { continue };
        // A key that would lead out of the store (`../x/.zarray`, `/etc/x/.zarray`) is skipped.
        let dir = k.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        if !safe_path(dir) {
            continue;
        }
        let a = if let Some(path) = k.strip_suffix(".zarray") {
            let path = path.trim_end_matches('/');
            let attrs = json.get(&format!("{}.zattrs", if path.is_empty() { String::new() } else { format!("{path}/") }));
            array_v2(path, v, attrs)
        } else if let Some(path) = k.strip_suffix("zarr.json") {
            if v.get("node_type").and_then(Value::as_str) != Some("array") {
                continue;
            }
            array_v3(path.trim_end_matches('/'), v)
        } else {
            continue;
        };
        match a {
            Some(a) => arrays.push(a),
            None => continue,
        }
    }
    if arrays.is_empty() {
        return Err(bad(
            "no Zarr arrays found: give a store with consolidated metadata (.zmetadata, or zarr.json at its root), a folder on this computer, or the address of one array",
        ));
    }
    Ok(Store { arrays })
}

fn u64s(v: Option<&Value>) -> Option<Vec<u64>> {
    let a = v?.as_array()?;
    if a.len() > MAX_DIMS {
        return None;
    }
    a.iter().map(Value::as_u64).collect()
}

fn fill_of(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        },
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn dims_of(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().take(MAX_DIMS).map(|d| d.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
}

fn array_v2(path: &str, z: &Value, attrs: Option<&Value>) -> Option<Array> {
    let shape = u64s(z.get("shape"))?;
    let chunks = u64s(z.get("chunks"))?;
    if chunks.len() != shape.len() || chunks.contains(&0) {
        return None;
    }
    let dtype = DType::parse_v2(z.get("dtype")?.as_str()?);
    let mut codecs = Vec::new();
    for f in z.get("filters").and_then(Value::as_array).into_iter().flatten().take(8) {
        codecs.push(numcodec(f));
    }
    if let Some(c) = z.get("compressor").filter(|c| !c.is_null()) {
        codecs.push(numcodec(c));
    }
    let dtype = match dtype {
        Some(d) => d,
        None => {
            codecs.push(Codec::Unsupported(format!("{path}: its data type {} isn't read", z.get("dtype").unwrap_or(&Value::Null))));
            DType { kind: Kind::UInt, size: 1, little: true }
        }
    };
    let attrs = attrs.and_then(Value::as_object).cloned().unwrap_or_default();
    Some(Array {
        path: path.to_string(),
        zarr_format: 2,
        dims: dims_of(attrs.get("_ARRAY_DIMENSIONS")),
        shape,
        chunks,
        dtype,
        fill: fill_of(z.get("fill_value")),
        fill_exact: exact_fill(z.get("fill_value")),
        fortran: z.get("order").and_then(Value::as_str) == Some("F"),
        codecs,
        attrs,
        key_prefix: String::new(),
        separator: if z.get("dimension_separator").and_then(Value::as_str) == Some("/") { '/' } else { '.' },
    })
}

/// A Zarr 3 array's codecs, for chunks of `chunks` values; a shard's (not `outer`) can't be
/// sharded again.
fn codecs_v3(path: &str, list: Option<&Value>, chunks: &[u64], outer: bool) -> Vec<Codec> {
    let mut codecs = Vec::new();
    for c in list.and_then(Value::as_array).into_iter().flatten().take(8) {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        codecs.push(match name {
            "bytes" => Codec::Bytes { little: c.pointer("/configuration/endian").and_then(Value::as_str) != Some("big") },
            "gzip" => Codec::Gzip,
            "zstd" => Codec::Zstd,
            "blosc" => Codec::Blosc,
            "crc32c" => Codec::Crc32c,
            "sharding_indexed" if outer => sharding(path, c, chunks),
            "sharding_indexed" => Codec::Unsupported(format!("{path}: shards inside shards aren't read")),
            "transpose" => Codec::Unsupported(format!("{path}: transposed chunks aren't read")),
            other => Codec::Unsupported(format!("{path}: the {other} codec isn't read")),
        });
    }
    // Sharding turns the values into bytes, so only it can come first; anything after it would
    // encode the whole shard, which isn't read.
    if codecs.len() > 1 && codecs.iter().any(|c| matches!(c, Codec::Sharding { .. })) {
        codecs.push(Codec::Unsupported(format!("{path}: only shards with nothing encoded after them are read")));
    }
    codecs
}

/// The `sharding_indexed` codec of an array whose chunks (shards) hold `shard` values.
fn sharding(path: &str, c: &Value, shard: &[u64]) -> Codec {
    let cfg = c.get("configuration");
    let inner = u64s(cfg.and_then(|c| c.get("chunk_shape"))).unwrap_or_default();
    // The inner chunks tile the shard exactly.
    if inner.len() != shard.len() || inner.iter().zip(shard).any(|(&i, &s)| i == 0 || !s.is_multiple_of(i)) {
        return Codec::Unsupported(format!("{path}: its shards' chunks don't tile its shards"));
    }
    let codecs = codecs_v3(path, cfg.and_then(|c| c.get("codecs")), &inner, false);
    let index = codecs_v3(path, cfg.and_then(|c| c.get("index_codecs")), &[], false);
    // The index's size is known only when it is stored as it is (with a checksum, or not).
    if !index.iter().all(|c| matches!(c, Codec::Bytes { .. } | Codec::Crc32c)) {
        return Codec::Unsupported(format!("{path}: its shards' index is encoded in a way that isn't read"));
    }
    let index_at_end = cfg.and_then(|c| c.get("index_location")).and_then(Value::as_str) != Some("start");
    Codec::Sharding { inner, codecs, index, index_at_end }
}

/// A Zarr 2 (numcodecs) codec.
fn numcodec(c: &Value) -> Codec {
    match c.get("id").and_then(Value::as_str).unwrap_or("") {
        "blosc" => Codec::Blosc,
        "zlib" => Codec::Zlib,
        "gzip" => Codec::Gzip,
        "zstd" => Codec::Zstd,
        "lz4" => Codec::Lz4,
        "shuffle" => {
            Codec::Shuffle(c.get("elementsize").and_then(Value::as_u64).and_then(|e| usize::try_from(e).ok()).unwrap_or(4).clamp(1, 16))
        }
        other => Codec::Unsupported(format!("the {other} codec isn't read")),
    }
}

fn array_v3(path: &str, z: &Value) -> Option<Array> {
    let shape = u64s(z.get("shape"))?;
    let chunks = u64s(z.pointer("/chunk_grid/configuration/chunk_shape"))?;
    if chunks.len() != shape.len() || chunks.contains(&0) {
        return None;
    }
    let mut codecs = Vec::new();
    let dtype = match z.get("data_type").and_then(Value::as_str).and_then(DType::parse_v3) {
        Some(d) => d,
        None => {
            codecs.push(Codec::Unsupported(format!("{path}: its data type {} isn't read", z.get("data_type").unwrap_or(&Value::Null))));
            DType { kind: Kind::UInt, size: 1, little: true }
        }
    };
    if z.pointer("/chunk_grid/name").and_then(Value::as_str) != Some("regular") {
        codecs.push(Codec::Unsupported(format!("{path}: only regular chunk grids are read")));
    }
    codecs.extend(codecs_v3(path, z.get("codecs"), &chunks, true));
    let encoding = z.pointer("/chunk_key_encoding/name").and_then(Value::as_str).unwrap_or("default");
    let sep = z.pointer("/chunk_key_encoding/configuration/separator").and_then(Value::as_str);
    let (key_prefix, separator) = if encoding == "v2" {
        (String::new(), if sep == Some("/") { '/' } else { '.' })
    } else {
        ("c/".to_string(), if sep == Some(".") { '.' } else { '/' })
    };
    Some(Array {
        path: path.to_string(),
        zarr_format: 3,
        dims: dims_of(z.get("dimension_names")),
        shape,
        chunks,
        dtype,
        fill: fill_of(z.get("fill_value")),
        fill_exact: exact_fill(z.get("fill_value")),
        fortran: false,
        codecs,
        attrs: z.get("attributes").and_then(Value::as_object).cloned().unwrap_or_default(),
        key_prefix,
        separator,
    })
}

// --- Decompression -------------------------------------------------------------------------------

fn inflate(data: &[u8], zlib: bool, want: usize) -> Result<Vec<u8>> {
    let r = if zlib {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, want.max(1))
    } else {
        miniz_oxide::inflate::decompress_to_vec_with_limit(data, want.max(1))
    };
    r.map_err(|_| bad("a chunk doesn't inflate"))
}

/// gzip: its header (with the optional parts), then deflate.
fn gunzip(d: &[u8], want: usize) -> Result<Vec<u8>> {
    if d.get(..3) != Some(&[0x1f, 0x8b, 8]) {
        return Err(bad("a chunk isn't gzip"));
    }
    let flags = d.get(3).copied().unwrap_or(0);
    let mut at = 10usize;
    if flags & 4 != 0 {
        let n = d.get(at..at + 2).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]]))).ok_or_else(|| bad("a cut gzip header"))?;
        at += 2 + n;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            at += d.get(at..).and_then(|r| r.iter().position(|&b| b == 0)).ok_or_else(|| bad("a cut gzip header"))? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    inflate(d.get(at..).ok_or_else(|| bad("a cut gzip header"))?, false, want)
}

fn unzstd(d: &[u8], want: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let dec = ruzstd::decoding::StreamingDecoder::new(d).map_err(|e| bad(format!("a zstd chunk: {e}")))?;
    let mut out = Vec::with_capacity(want.min(64 << 20));
    dec.take(want as u64 + 1).read_to_end(&mut out).map_err(|e| bad(format!("a zstd chunk: {e}")))?;
    Ok(out)
}

fn numcodecs_lz4(d: &[u8], want: usize) -> Result<Vec<u8>> {
    let n = d.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(|| bad("a cut LZ4 chunk"))?;
    if n > want.max(1) {
        return Err(bad("an LZ4 chunk larger than its array's chunks"));
    }
    let mut out = vec![0u8; n];
    let got = lz4_flex::block::decompress_into(d.get(4..).unwrap_or_default(), &mut out).map_err(|e| bad(format!("an LZ4 chunk: {e}")))?;
    out.truncate(got);
    Ok(out)
}

/// Byte unshuffle: element `j`'s byte `i` was stored at `i * n + j`.
fn unshuffle(d: &[u8], size: usize) -> Vec<u8> {
    if size <= 1 {
        return d.to_vec();
    }
    let n = d.len() / size;
    let mut out = vec![0u8; d.len()];
    for i in 0..size {
        for j in 0..n {
            if let (Some(o), Some(v)) = (out.get_mut(j * size + i), d.get(i * n + j)) {
                *o = *v;
            }
        }
    }
    // Bytes past the last whole element are as they were.
    let rest = n * size;
    if let (Some(o), Some(v)) = (out.get_mut(rest..), d.get(rest..)) {
        o.copy_from_slice(v);
    }
    out
}

/// Decodes a blosc (version 1) frame of at most `want` bytes (a sharded store's inner chunk read
/// by itself, such as Tessera's).
pub fn blosc_decode(d: &[u8], want: usize) -> Result<Vec<u8>> {
    blosc(d, want)
}

/// A blosc (version 1) frame: a 16-byte header, then blocks compressed with LZ4, zlib or zstd,
/// each byte-shuffled and possibly split into a stream per byte of the type.
fn blosc(d: &[u8], want: usize) -> Result<Vec<u8>> {
    let h = d.get(..16).ok_or_else(|| bad("a cut blosc chunk"))?;
    let flags = h[2];
    let typesize = usize::from(h[3]).max(1);
    let le = |o: usize| u32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]) as usize;
    let (nbytes, blocksize) = (le(4), le(8));
    if nbytes > want.max(1) {
        return Err(bad("a blosc chunk larger than its array's chunks"));
    }
    // Stored as it is.
    if flags & 0x2 != 0 {
        return Ok(d.get(16..16 + nbytes).ok_or_else(|| bad("a cut blosc chunk"))?.to_vec());
    }
    if blocksize == 0 || nbytes == 0 {
        return Ok(Vec::new());
    }
    let codec = flags >> 5;
    let nblocks = nbytes.div_ceil(blocksize);
    let mut out = Vec::with_capacity(nbytes);
    for b in 0..nblocks {
        let start = d
            .get(16 + 4 * b..20 + 4 * b)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
            .ok_or_else(|| bad("a cut blosc chunk"))?;
        let bsize = if b + 1 == nblocks { nbytes - b * blocksize } else { blocksize };
        // A stream per byte of the type, unless the block wasn't split; the last, shorter block
        // never is.
        let leftover = bsize < blocksize;
        let split = !leftover && flags & 0x10 == 0 && typesize <= 16 && blocksize / typesize >= 128;
        let streams = if split { typesize } else { 1 };
        let ssize = bsize / streams;
        let mut block = Vec::with_capacity(bsize);
        let mut at = start;
        for s in 0..streams {
            let this = if s + 1 == streams { bsize - ssize * (streams - 1) } else { ssize };
            let cbytes = d.get(at..at + 4).map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| bad("a cut blosc chunk"))?;
            at += 4;
            let cb = usize::try_from(cbytes).map_err(|_| bad("a blosc stream of special values isn't read"))?;
            let src = d.get(at..at + cb).ok_or_else(|| bad("a cut blosc chunk"))?;
            at += cb;
            if cb == this {
                block.extend_from_slice(src);
                continue;
            }
            let part = match codec {
                1 => {
                    let mut o = vec![0u8; this];
                    let n = lz4_flex::block::decompress_into(src, &mut o).map_err(|e| bad(format!("a blosc LZ4 block: {e}")))?;
                    o.truncate(n);
                    o
                }
                3 => inflate(src, true, this)?,
                4 => unzstd(src, this)?,
                0 => return Err(bad("blosc's own codec (blosclz) isn't read: use LZ4, zlib or zstd")),
                2 => return Err(bad("snappy blosc chunks aren't read")),
                c => return Err(bad(format!("blosc codec {c} isn't read"))),
            };
            block.extend_from_slice(part.get(..this.min(part.len())).unwrap_or_default());
        }
        if flags & 0x4 != 0 {
            block = bitunshuffle(&block, typesize, h[0]);
        } else if flags & 0x1 != 0 && typesize > 1 {
            block = unshuffle(&block, typesize);
        }
        out.extend_from_slice(&block);
    }
    Ok(out)
}

/// Bit unshuffle of a blosc block of `size`-byte elements: bit `k` of byte `j` of element `i` was
/// stored in bit-plane `8j + k`, at bit `i % 8` of its byte `i / 8`. Blosc format 2 shuffles only
/// blocks of a multiple of 8 elements; later formats shuffle the first multiple of 8 and leave the
/// rest as it was.
fn bitunshuffle(d: &[u8], size: usize, format: u8) -> Vec<u8> {
    let n = d.len() / size.max(1);
    let shuffled = if format <= 2 { if n.is_multiple_of(8) { n } else { 0 } } else { n - n % 8 };
    let mut out = d.to_vec();
    if shuffled == 0 {
        return out;
    }
    let plane = shuffled / 8;
    for (i, o) in out.iter_mut().take(shuffled * size).enumerate() {
        let (e, j) = (i / size, i % size);
        let mut byte = 0u8;
        for k in 0..8 {
            let bit = d.get((j * 8 + k) * plane + e / 8).map_or(0, |b| (b >> (e % 8)) & 1);
            byte |= bit << k;
        }
        *o = byte;
    }
    out
}

/// The length of a shard's index of `count` chunks: 16 bytes a chunk, and a checksum after them.
fn index_len(count: usize, index: &[Codec]) -> Option<usize> {
    count.checked_mul(16)?.checked_add(if index.contains(&Codec::Crc32c) { 4 } else { 0 })
}

/// A shard's index (`raw`, encoded with `index`): each of its `count` chunks' (offset, size),
/// `(u64::MAX, u64::MAX)` for one it doesn't hold.
fn shard_index(raw: &[u8], count: usize, index: &[Codec]) -> Result<Vec<(u64, u64)>> {
    if Some(raw.len()) != index_len(count, index) {
        return Err(bad("a shard's index is cut"));
    }
    let (idx, little) = decode_chunk(index, raw, &[count as u64, 2], DType { kind: Kind::UInt, size: 8, little: true }, None)?;
    let word = |b: &[u8]| -> u64 {
        let b: [u8; 8] = b.try_into().unwrap_or([0xff; 8]);
        if little { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) }
    };
    Ok(idx.as_chunks::<16>().0.iter().map(|p| p.split_at(8)).map(|(o, l)| (word(o), word(l))).collect())
}

/// A shard of `shard` values: its chunks of `inner` values, each decoded with `codecs`, put in
/// place (row-major), with its index of (offset, size) pairs (`index` its encoding) at the end
/// or the start. Chunks it doesn't hold are `fill`. Returns the values' bytes and byte order.
fn unshard(
    d: &[u8],
    shard: &[u64],
    inner: &[u64],
    codecs: &[Codec],
    index: &[Codec],
    index_at_end: bool,
    dtype: DType,
    fill: Option<f64>,
) -> Result<(Vec<u8>, bool)> {
    let nd = shard.len();
    if inner.len() != nd || inner.contains(&0) {
        return Err(bad("a shard's chunks don't fit it"));
    }
    let grid: Vec<u64> = shard.iter().zip(inner).map(|(&s, &i)| s / i).collect();
    let count = grid
        .iter()
        .try_fold(1u64, |a, &g| a.checked_mul(g))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| bad("a shard too large"))?;
    // The index: 16 bytes a chunk, and a checksum after them. It must lie in the file, so its size
    // is bounded by the file's before anything is made for it.
    let ilen = index_len(count, index).filter(|&n| n <= d.len()).ok_or_else(|| bad("a shard's index is cut"))?;
    let raw = if index_at_end { d.get(d.len() - ilen..) } else { d.get(..ilen) }.ok_or_else(|| bad("a shard's index is cut"))?;
    let idx = shard_index(raw, count, index)?;
    let word = |i: usize| -> u64 { idx.get(i / 2).map_or(u64::MAX, |&(o, l)| if i.is_multiple_of(2) { o } else { l }) };
    let little = codecs.iter().find_map(|c| if let Codec::Bytes { little } = c { Some(*little) } else { None }).unwrap_or(dtype.little);
    let size = dtype.size;
    let total = shard
        .iter()
        .try_fold(1u64, |a, &c| a.checked_mul(c))
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| n.checked_mul(size))
        .ok_or_else(|| bad("a shard too large"))?;
    let fill_bytes = dtype.encode(fill.unwrap_or(0.0), little);
    let mut out: Vec<u8> = fill_bytes.iter().copied().cycle().take(total).collect();
    // Row-major strides of the shard, in values.
    let mut strides = vec![1u64; nd];
    for k in (0..nd.saturating_sub(1)).rev() {
        strides[k] = strides[k + 1].saturating_mul(shard[k + 1]);
    }
    let row = inner.last().copied().unwrap_or(1) as usize * size;
    let rows = inner.iter().take(nd.saturating_sub(1)).product::<u64>() as usize;
    for c in 0..count {
        let (off, len) = (word(2 * c), word(2 * c + 1));
        if off == u64::MAX && len == u64::MAX {
            continue;
        }
        let src = usize::try_from(off)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(o, l)| d.get(o..o.checked_add(l)?))
            .ok_or_else(|| bad("a shard's chunk lies outside it"))?;
        let (vals, _) = decode_chunk(codecs, src, inner, dtype, fill)?;
        // The chunk's corner in the shard, then each of its rows (all but the last dimension)
        // copied to where it goes.
        let mut at = vec![0u64; nd];
        let mut rest = c as u64;
        for k in (0..nd).rev() {
            at[k] = rest % grid[k] * inner[k];
            rest /= grid[k];
        }
        for r in 0..rows {
            let mut rest = r as u64;
            let mut start = at.last().copied().unwrap_or(0);
            for k in (0..nd.saturating_sub(1)).rev() {
                start += (at[k] + rest % inner[k]) * strides[k];
                rest /= inner[k];
            }
            let to = start as usize * size;
            if let (Some(o), Some(v)) = (out.get_mut(to..to + row), vals.get(r * row..r * row + row)) {
                o.copy_from_slice(v);
            }
        }
    }
    Ok((out, little))
}

// --- Slices --------------------------------------------------------------------------------------

/// Which slice of which variable: its other dimensions' indexes (0 when not given), and the window
/// of its map to read (all of it when not given).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub variable: String,
    pub index: Vec<(String, u64)>,
    pub window: Option<Window>,
}

/// A window of a variable's map: columns `x0..x1` and rows `y0..y1` of its cells as stored (the
/// ends not included).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub x0: u64,
    pub y0: u64,
    pub x1: u64,
    pub y1: u64,
}

impl Selection {
    /// `t2m[time=3,level=0]` (`cells=x0:y0:x1:y1` after them for a window), as projects save it.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = self.index.iter().map(|(d, i)| format!("{d}={i}")).collect();
        parts.extend(self.window.map(|w| format!("cells={}:{}:{}:{}", w.x0, w.y0, w.x1, w.y1)));
        if parts.is_empty() { self.variable.clone() } else { format!("{}[{}]", self.variable, parts.join(",")) }
    }

    pub fn parse(s: &str) -> Selection {
        Selection::read(s).0
    }

    /// [`Selection::parse`] for a slice about to be read: a `cells=` part that isn't four whole
    /// numbers is refused, not read as the whole slice.
    pub fn parse_strict(s: &str) -> Result<Selection> {
        match Selection::read(s) {
            (_, true) => Err(bad(format!("{s}: its cells aren't four whole numbers (first column, first row, end column, end row)"))),
            (sel, false) => Ok(sel),
        }
    }

    /// The selection, and whether it has a `cells=` part that isn't a window.
    fn read(s: &str) -> (Selection, bool) {
        let s = s.trim();
        let Some((v, rest)) = s.split_once('[') else { return (Selection { variable: s.to_string(), ..Selection::default() }, false) };
        let (mut sel, mut malformed) = (Selection { variable: v.to_string(), ..Selection::default() }, false);
        for (k, v) in rest.trim_end_matches(']').split(',').filter_map(|kv| kv.split_once('=')).take(MAX_DIMS + 1) {
            let (k, v) = (k.trim(), v.trim());
            // A window is four `:`-separated whole numbers; a dimension named `cells` has a step.
            if k == "cells" && v.contains(':') {
                let n: Vec<Option<u64>> = v.split(':').map(|n| n.trim().parse().ok()).collect();
                match n[..] {
                    [Some(x0), Some(y0), Some(x1), Some(y1)] => sel.window = Some(Window { x0, y0, x1, y1 }),
                    _ => malformed = true,
                }
            } else if let Ok(i) = v.parse() {
                sel.index.push((k.to_string(), i));
            } else if k == "cells" {
                malformed = true;
            }
        }
        (sel, malformed)
    }
}

/// Bytes of a store's file that a [`Plan`] with a window reads, handed back under `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub key: String,
    pub span: Span,
    pub id: String,
}

/// Which bytes of a file a [`Part`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    /// Bytes `first..=last`.
    Bytes(u64, u64),
    /// The file's last `n` bytes.
    Tail(u64),
}

/// A shard a window of a sharded variable reads: where its index is, and which of its chunks.
#[derive(Clone, Debug)]
struct ShardRead {
    key: String,
    count: usize,
    chunks: Vec<usize>,
}

/// What a slice needs: the chunk files to fetch (`keys`, whole), the bytes of files to fetch
/// (`parts`, for a window: shards' indexes, coordinates), and how they make the raster. A window
/// of a sharded variable then reads the shards' chunks it needs ([`inner_parts`]).
#[derive(Clone, Debug)]
pub struct Plan {
    pub keys: Vec<String>,
    pub parts: Vec<Part>,
    var: String,
    y: usize,
    x: usize,
    fixed: Vec<u64>,
    ycoord: Option<String>,
    xcoord: Option<String>,
    window: Window,
    windowed: bool,
    shards: Vec<ShardRead>,
}

/// The id a shard's chunk `c` (in the shard's row-major order) is handed back under.
fn inner_id(shard: &str, c: usize) -> String {
    format!("{shard}#{c}")
}

/// Whether a 1D array's chunks are its values as they are (no compression), so a window reads
/// only its own values' bytes of them.
fn plain(c: &Array) -> bool {
    c.shape.len() == 1 && c.codecs.iter().all(|k| matches!(k, Codec::Bytes { .. } | Codec::Crc32c))
}

/// The bytes of chunk `i` of a plain 1D array ([`plain`]) holding its values `a..b`; an error
/// when they lie past what a byte offset holds (a chunk size no store has).
fn coord_part(c: &Array, i: u64, a: u64, b: u64) -> Result<Part> {
    let (cs, size) = (c.chunks.first().copied().unwrap_or(1).max(1), c.dtype.size as u64);
    let at = |v: u64| i.checked_mul(cs).and_then(|start| v.checked_sub(start)).and_then(|n| n.checked_mul(size));
    let (first, last) = at(a)
        .zip(at(b).and_then(|e| e.checked_sub(1)))
        .filter(|(f, l)| f <= l)
        .ok_or_else(|| bad(format!("{}: its chunks are too large to read", c.path)))?;
    let key = c.chunk_key(&[i]);
    let id = format!("{key}@{first}-{last}");
    Ok(Part { key, span: Span::Bytes(first, last), id })
}

/// Values `start..end` of a 1D array, from its chunks or, for a [`plain`] one in a window, the bytes
/// of them [`coord_part`] names; missing ones are its fill value.
fn coord_values(c: &Array, start: u64, end: u64, windowed: bool, chunks: &HashMap<String, Vec<u8>>) -> Result<Vec<f64>> {
    if !windowed {
        return values_1d(c, chunks);
    }
    if end.saturating_sub(start) > MAX_COORDS {
        return Err(bad(format!("{} is too long", c.path)));
    }
    let cs = c.chunks.first().copied().unwrap_or(1).max(1);
    let little = c.codecs.iter().find_map(|k| if let Codec::Bytes { little } = k { Some(*little) } else { None });
    let dt = DType { little: little.unwrap_or(c.dtype.little), ..c.dtype };
    let mut out = Vec::new();
    for i in start / cs..end.div_ceil(cs) {
        let (a, b) = ((i * cs).max(start), (i + 1).saturating_mul(cs).min(end));
        if plain(c) {
            match chunks.get(&coord_part(c, i, a, b)?.id) {
                Some(bytes) if bytes.len() as u64 == (b - a) * dt.size as u64 => {
                    out.extend(bytes.chunks_exact(dt.size).map(|v| dt.value(v)))
                }
                Some(_) => return Err(bad(format!("{}: a chunk is cut", c.path))),
                None => out.extend(std::iter::repeat_n(c.fill.unwrap_or(f64::NAN), (b - a) as usize)),
            }
        } else {
            match chunks.get(&c.chunk_key(&[i])) {
                Some(bytes) => out.extend(c.decode(bytes)?.into_iter().skip((a - i * cs) as usize).take((b - a) as usize)),
                None => out.extend(std::iter::repeat_n(c.fill.unwrap_or(f64::NAN), (b - a) as usize)),
            }
        }
    }
    if out.len() as u64 != end - start {
        return Err(bad(format!("{}: a chunk is cut", c.path)));
    }
    Ok(out)
}

fn is_y(d: &str) -> bool {
    matches!(d.to_ascii_lowercase().as_str(), "lat" | "latitude" | "y" | "rlat" | "nav_lat" | "south_north" | "j" | "yc" | "northing")
}

fn is_x(d: &str) -> bool {
    matches!(d.to_ascii_lowercase().as_str(), "lon" | "longitude" | "x" | "rlon" | "nav_lon" | "west_east" | "i" | "xc" | "easting")
}

/// The dimensions that make the map: those named like y and x, else the last two.
fn map_dims(a: &Array) -> (usize, usize) {
    let names = a.dim_names();
    let y = names.iter().rposition(|d| is_y(d));
    let x = names.iter().rposition(|d| is_x(d));
    match (y, x) {
        (Some(y), Some(x)) if y != x => (y, x),
        _ => (a.shape.len().saturating_sub(2), a.shape.len().saturating_sub(1)),
    }
}

/// The chunk keys of all of a 1D array.
fn all_keys(a: &Array) -> Vec<String> {
    (0..a.grid(0).min(MAX_CHUNKS as u64)).map(|i| a.chunk_key(&[i])).collect()
}

/// The chunks a slice needs.
pub fn plan(store: &Store, sel: &Selection) -> Result<Plan> {
    let a = store.array(&sel.variable).ok_or_else(|| bad(format!("no variable named {}", sel.variable)))?;
    if let Some(why) = a.unsupported() {
        return Err(bad(why));
    }
    if a.shape.len() < 2 {
        return Err(bad(format!("{} has fewer than two dimensions", a.path)));
    }
    let names = a.dim_names();
    let (y, x) = map_dims(a);
    let (h, w) = (a.shape.get(y).copied().unwrap_or(0), a.shape.get(x).copied().unwrap_or(0));
    let window = sel.window.unwrap_or(Window { x0: 0, y0: 0, x1: w, y1: h });
    if let Some(v) = sel.window
        && (v.x1 > w || v.y1 > h || v.x1 < v.x0.saturating_add(2) || v.y1 < v.y0.saturating_add(2))
    {
        return Err(bad(format!(
            "{}: the window of columns {}–{} and rows {}–{} isn't within its {w} × {h} cells, or is less than 2 × 2 cells",
            a.path, v.x0, v.x1, v.y0, v.y1
        )));
    }
    let (ww, wh) = (window.x1 - window.x0, window.y1 - window.y0);
    let cells = wh.checked_mul(ww).ok_or_else(|| bad("too large"))?;
    let max = max_samples();
    if cells == 0 || cells > max as u64 {
        return Err(bad(match sel.window {
            Some(_) => format!(
                "{}: the window is {ww} × {wh} cells, more than the {max} a raster holds on this computer: take a smaller one",
                a.path
            ),
            None => format!("{} is {w} × {h}, more than the {max} cells a raster holds on this computer: take a window of it", a.path),
        }));
    }
    a.chunk_values()?;
    let mut fixed = vec![0u64; a.shape.len()];
    for (d, i) in &sel.index {
        let Some(k) = names.iter().position(|n| n == d) else { return Err(bad(format!("{} has no dimension {d}", a.path))) };
        if k != y && k != x {
            if *i >= a.shape.get(k).copied().unwrap_or(0) {
                return Err(bad(format!("{d} has {} steps; {i} is past them", a.shape.get(k).copied().unwrap_or(0))));
            }
            if let Some(f) = fixed.get_mut(k) {
                *f = *i;
            }
        }
    }
    // The units of `unit` the window covers along y and x, and how many they are.
    let cover = |unit: &[u64]| {
        let size = |d: usize| unit.get(d).copied().unwrap_or(1).max(1);
        let (ry, rx) = (window.y0 / size(y)..window.y1.div_ceil(size(y)), window.x0 / size(x)..window.x1.div_ceil(size(x)));
        let n = (ry.end - ry.start).saturating_mul(rx.end - rx.start);
        (ry, rx, n)
    };
    // A window of a sharded variable reads the shards' chunks it needs, by range, while they are at
    // most MAX_CHUNKS. Past that, and outside a window, it reads whole chunk files (whole shards),
    // as before, so the limit still counts what is fetched.
    let sharding = match a.codecs.first() {
        Some(Codec::Sharding { inner, index, .. }) if sel.window.is_some() && cover(inner).2 <= MAX_CHUNKS as u64 => Some((inner, index)),
        _ => None,
    };
    let unit = sharding.map_or(&a.chunks, |(inner, _)| inner);
    let size = |d: usize| unit.get(d).copied().unwrap_or(1).max(1);
    let (ry, rx, n) = cover(unit);
    // A shard's chunks along each dimension, the same for every shard.
    let per: Vec<u64> = sharding.map_or_else(Vec::new, |(inner, _)| a.chunks.iter().zip(inner).map(|(s, i)| s / i.max(&1)).collect());
    // Each shard read, by key: its place in `shards`.
    let mut shard_at: HashMap<String, usize> = HashMap::new();
    if n > MAX_CHUNKS as u64 {
        let what = if sel.window.is_some() { "the window" } else { "one slice" };
        return Err(bad(format!("{} needs {n} chunks for {what}, more than {MAX_CHUNKS}: take a smaller window", a.path)));
    }
    let (mut keys, mut parts, mut shards) = (Vec::new(), Vec::new(), Vec::<ShardRead>::new());
    for cy in ry {
        for cx in rx.clone() {
            let idx: Vec<u64> = (0..a.shape.len())
                .map(|d| match d {
                    d if d == y => cy,
                    d if d == x => cx,
                    d => fixed.get(d).copied().unwrap_or(0) / size(d),
                })
                .collect();
            let Some((_, index)) = sharding else {
                keys.push(a.chunk_key(&idx));
                continue;
            };
            // The shard holding this chunk, and the chunk's place in the shard's row-major order.
            let shard: Vec<u64> = idx.iter().zip(&per).map(|(u, p)| u / p.max(&1)).collect();
            let c = idx.iter().zip(&per).fold(0u64, |c, (u, p)| c * p + u % p.max(&1));
            let key = a.chunk_key(&shard);
            match shard_at.get(&key).and_then(|&i| shards.get_mut(i)) {
                Some(s) => s.chunks.push(c as usize),
                None => {
                    shard_at.insert(key.clone(), shards.len());
                    let count = per.iter().product::<u64>() as usize;
                    let len = index_len(count, index).ok_or_else(|| bad("a shard too large"))? as u64;
                    let span = if matches!(a.codecs.first(), Some(Codec::Sharding { index_at_end: false, .. })) {
                        Span::Bytes(0, len - 1)
                    } else {
                        Span::Tail(len)
                    };
                    parts.push(Part { key: key.clone(), span, id: format!("{key}#index") });
                    shards.push(ShardRead { key, count, chunks: vec![c as usize] });
                }
            }
        }
    }
    let coord = |d: usize| names.get(d).and_then(|n| store.coordinate(a, n)).filter(|c| c.shape.first() == a.shape.get(d));
    let (yc, xc) = (coord(y), coord(x));
    for (c, start, end) in [(yc, window.y0, window.y1), (xc, window.x0, window.x1)] {
        let Some(c) = c else { continue };
        if sel.window.is_none() {
            keys.extend(all_keys(c));
            continue;
        }
        // A window's coordinates: the chunks holding them, only their bytes when stored plain.
        let cs = c.chunks.first().copied().unwrap_or(1).max(1);
        if end.div_ceil(cs) - start / cs > MAX_CHUNKS as u64 {
            return Err(bad(format!("{}: the window needs more than {MAX_CHUNKS} chunks of it: take a smaller window", c.path)));
        }
        for i in start / cs..end.div_ceil(cs) {
            if plain(c) {
                parts.push(coord_part(c, i, (i * cs).max(start), (i + 1).saturating_mul(cs).min(end))?);
            } else {
                keys.push(c.chunk_key(&[i]));
            }
        }
    }
    Ok(Plan {
        keys,
        parts,
        var: a.path.clone(),
        y,
        x,
        fixed,
        ycoord: yc.map(|c| c.path.clone()),
        xcoord: xc.map(|c| c.path.clone()),
        window,
        windowed: sel.window.is_some(),
        shards,
    })
}

/// The shards' chunks a window of a sharded variable reads, from their indexes (`got`, by the ids
/// of the plan's index [`Part`]s): each chunk's bytes. A shard that isn't stored, and a chunk its
/// index says it doesn't hold, are left out: [`assemble`] leaves the first empty, as a chunk never
/// written, and gives the second the fill value, as a shard read whole does.
pub fn inner_parts(store: &Store, plan: &Plan, got: &HashMap<String, Vec<u8>>) -> Result<Vec<Part>> {
    let Some(a) = store.array(&plan.var) else { return Ok(Vec::new()) };
    let Some(Codec::Sharding { index, .. }) = a.codecs.first() else { return Ok(Vec::new()) };
    let (mut parts, mut total) = (Vec::new(), 0u64);
    for s in &plan.shards {
        let Some(raw) = got.get(&format!("{}#index", s.key)) else { continue };
        let idx = shard_index(raw, s.count, index).map_err(|e| bad(format!("{}: {e}", s.key)))?;
        for &c in &s.chunks {
            let Some(&(off, len)) = idx.get(c) else { continue };
            if off == u64::MAX && len == u64::MAX {
                continue;
            }
            let last = off
                .checked_add(len)
                .filter(|_| len > 0 && len <= MAX_CHUNK_FILE)
                .ok_or_else(|| bad(format!("{}: a chunk's place in it is wrong", s.key)))?;
            total = total.saturating_add(len);
            if total > MAX_READ {
                return Err(bad(format!("{}: the window's chunks are more than {} GB: take a smaller window", a.path, MAX_READ >> 30)));
            }
            parts.push(Part { key: s.key.clone(), span: Span::Bytes(off, last - 1), id: inner_id(&s.key, c) });
        }
    }
    Ok(parts)
}

/// A 1D array's values, from its chunks (missing ones are its fill value).
fn values_1d(a: &Array, chunks: &HashMap<String, Vec<u8>>) -> Result<Vec<f64>> {
    let n = a.shape.first().copied().unwrap_or(0);
    if n > MAX_COORDS {
        return Err(bad(format!("{} is too long", a.path)));
    }
    let c = a.chunks.first().copied().unwrap_or(1).max(1);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..a.grid(0) {
        let take = (n - i * c).min(c) as usize;
        match chunks.get(&a.chunk_key(&[i])) {
            Some(b) => out.extend(a.decode(b)?.into_iter().take(take)),
            None => out.extend(std::iter::repeat_n(a.fill.unwrap_or(f64::NAN), take)),
        }
    }
    Ok(out)
}

/// A JSON number exactly: a whole one as one.
fn exact_number(n: &serde_json::Number) -> Option<Exact> {
    n.as_i64()
        .map(|i| Exact::Whole(i.into()))
        .or_else(|| n.as_u64().map(|u| Exact::Whole(u.into())))
        .or_else(|| n.as_f64().map(Exact::Float))
}

/// [`fill_of`], with a whole number kept exact.
fn exact_fill(v: Option<&Value>) -> Option<Exact> {
    match v? {
        Value::Number(n) => exact_number(n),
        other => fill_of(Some(other)).map(Exact::Float),
    }
}

/// [`attr_f64`], with a whole number kept exact.
fn attr_exact(a: &Array, k: &str) -> Option<Exact> {
    match a.attrs.get(k)? {
        Value::Number(n) => exact_number(n),
        Value::Array(v) => v.first().and_then(Value::as_number).and_then(exact_number),
        Value::String(s) => Exact::parse(s),
        _ => None,
    }
}

/// A number attribute (`scale_factor`, `_FillValue`).
fn attr_f64(a: &Array, k: &str) -> Option<f64> {
    match a.attrs.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::Array(v) => v.first().and_then(Value::as_f64),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn attr_str<'a>(a: &'a Array, k: &str) -> Option<&'a str> {
    a.attrs.get(k).and_then(Value::as_str)
}

/// Whether a coordinate array is longitude/latitude, by its name, units or standard name.
fn geographic(c: &Array) -> bool {
    let units = attr_str(c, "units").unwrap_or("").to_ascii_lowercase();
    let std = attr_str(c, "standard_name").unwrap_or("").to_ascii_lowercase();
    units.starts_with("degree")
        || matches!(std.as_str(), "latitude" | "longitude")
        || matches!(c.name().to_ascii_lowercase().as_str(), "lat" | "lon" | "latitude" | "longitude")
}

/// The coordinate system a variable's attributes (or its grid mapping's) say, and what reading its
/// WKT said, as for a `.prj` ([`super::wkt::read_wkt`]).
fn crs_of(store: &Store, a: &Array) -> Option<(Crs, Option<String>)> {
    let read = |t: &str| {
        let t = t.trim();
        if t.contains('[') { super::wkt::read_wkt(t).ok() } else { Crs::parse(t).ok().map(|c| (c, None)) }
    };
    let from = |x: &Array| {
        for k in ["crs_wkt", "spatial_ref", "crs", "proj4", "proj4text", "proj4_params"] {
            if let Some(read) = attr_str(x, k).and_then(read) {
                return Some(read);
            }
        }
        for k in ["epsg_code", "epsg"] {
            let code = match x.attrs.get(k) {
                Some(Value::Number(n)) => n.as_u64(),
                Some(Value::String(s)) => s.trim_start_matches("EPSG:").parse().ok(),
                _ => None,
            };
            if let Some(c) = code.and_then(|c| u32::try_from(c).ok()).and_then(Crs::from_epsg) {
                return Some((c, None));
            }
        }
        None
    };
    from(a)
        .or_else(|| attr_str(a, "grid_mapping").and_then(|g| store.coordinate(a, g).or_else(|| store.array(g))).and_then(from))
        // A store that doesn't name its grid mapping may still hold rioxarray's `spatial_ref` (or a
        // `crs`) array beside its variables.
        .or_else(|| ["spatial_ref", "crs"].iter().filter_map(|n| store.array(n)).find_map(from))
}

/// One axis from its coordinates: the first cell's outer edge and the cell size, if the
/// coordinates are evenly spaced (to within a fifth of a cell).
fn axis(v: &[f64]) -> Option<(f64, f64)> {
    let (first, last) = (*v.first()?, *v.last()?);
    if v.len() < 2 {
        return None;
    }
    let step = (last - first) / (v.len() - 1) as f64;
    if !step.is_finite() || step == 0.0 {
        return None;
    }
    let even = v.iter().enumerate().all(|(i, c)| c.is_finite() && (c - (first + step * i as f64)).abs() <= step.abs() * 0.2);
    even.then_some((first - step / 2.0, step))
}

/// Decodes the chunks a [`plan`] named (missing ones are the fill value) into a raster.
/// `crs` is used when the store doesn't say its coordinate system.
pub fn assemble(store: &Store, plan: &Plan, chunks: &HashMap<String, Vec<u8>>, name: &str, crs: Option<Crs>) -> Result<Raster> {
    let a = store.array(&plan.var).ok_or_else(|| bad("the variable is gone"))?;
    let win = plan.window;
    let (h, w) = ((win.y1 - win.y0) as usize, (win.x1 - win.x0) as usize);
    let mut grid = super::filled(w, h, f32::NAN)?;
    // What the cells are read from: chunks, or a window's chunks of shards.
    let inner = match a.codecs.first() {
        Some(Codec::Sharding { inner, codecs, index, .. }) if !plan.shards.is_empty() => Some((inner, codecs, index)),
        _ => None,
    };
    let unit = inner.map_or(&a.chunks, |(i, _, _)| i);
    // A shard's chunks along each dimension, and the shards' indexes as read, by shard key.
    let per: Vec<u64> = inner.map_or_else(Vec::new, |(i, _, _)| a.chunks.iter().zip(i).map(|(s, i)| s / i.max(&1)).collect());
    let mut indexes: HashMap<String, Vec<(u64, u64)>> = HashMap::new();
    let (cy, cx) = (unit.get(plan.y).copied().unwrap_or(1).max(1), unit.get(plan.x).copied().unwrap_or(1).max(1));
    let nd = a.shape.len();
    // Values meaning "no data", before any scaling, as xarray reads them: the CF attributes, and a
    // Zarr 2 array's fill_value (xarray's _FillValue encoding). A Zarr 3 array's fill_value (0 by
    // default in zarr-python) only stands for chunks that were never written, and those cells stay
    // empty here; real zeros are kept. These become NaN before conversion, and the raster has no
    // no-data value of its own: exactly these cells are no-data, and they are left out of the
    // rounded count (the rule `Uncounted` gives the other readers).
    // They are kept exactly as written, so a whole number past 2^53 matches only itself.
    let v2_fill = if a.zarr_format == 2 { a.fill_exact } else { None };
    let mut missing: Vec<Exact> = [v2_fill, attr_exact(a, "_FillValue"), attr_exact(a, "missing_value")].into_iter().flatten().collect();
    missing.dedup();
    let scale = attr_f64(a, "scale_factor").unwrap_or(1.0);
    let offset = attr_f64(a, "add_offset").unwrap_or(0.0);
    let mut found = 0usize;
    // Values f32 doesn't hold exactly: stored ones (Int32 or Int64 past 16,777,216, Float64
    // digits), or scaled ones whose f32 no longer gives back the stored value.
    let scaled = scale != 1.0 || offset != 0.0;
    let mut rounded = 0u64;
    let size = |d: usize| unit.get(d).copied().unwrap_or(1).max(1);
    for gy in win.y0 / cy..win.y1.div_ceil(cy) {
        for gx in win.x0 / cx..win.x1.div_ceil(cx) {
            let idx: Vec<u64> = (0..nd)
                .map(|d| match d {
                    d if d == plan.y => gy,
                    d if d == plan.x => gx,
                    d => plan.fixed.get(d).copied().unwrap_or(0) / size(d),
                })
                .collect();
            // The chunk's id, and (in a shard) the shard's key and the chunk's place in it.
            let (id, shard) = match inner {
                Some(_) => {
                    let shard: Vec<u64> = idx.iter().zip(&per).map(|(u, p)| u / p.max(&1)).collect();
                    let key = a.chunk_key(&shard);
                    let c = idx.iter().zip(&per).fold(0u64, |c, (u, p)| c * p + u % p.max(&1)) as usize;
                    (inner_id(&key, c), Some((key, c)))
                }
                None => (a.chunk_key(&idx), None),
            };
            let (vals, dt) = match (chunks.get(&id), inner) {
                (Some(bytes), Some((i, codecs, _))) => {
                    let (d, little) = decode_chunk(codecs, bytes, i, a.dtype, a.fill).map_err(|e| bad(format!("{}: {e}", a.path)))?;
                    (d, DType { little, ..a.dtype })
                }
                (Some(bytes), None) => a.decode_bytes(bytes)?,
                // A shard not stored is left empty, as a chunk never written. In a stored shard, a
                // chunk its index says it doesn't hold shows the fill value, as when the shard is
                // read whole ([`unshard`]); one its index places but that wasn't read (the shard
                // gone since its index was) is an error, not cells made up.
                (None, Some((i, _, index))) => {
                    let Some((key, c)) = &shard else { continue };
                    let Some(raw) = chunks.get(&format!("{key}#index")) else { continue };
                    if !indexes.contains_key(key) {
                        let count = per.iter().product::<u64>() as usize;
                        indexes.insert(key.clone(), shard_index(raw, count, index).map_err(|e| bad(format!("{key}: {e}")))?);
                    }
                    if indexes.get(key).and_then(|x| x.get(*c)).is_some_and(|&e| e != (u64::MAX, u64::MAX)) {
                        return Err(bad(format!("{}: a chunk of shard {key} wasn't read", a.path)));
                    }
                    let n = i.iter().product::<u64>() as usize;
                    (a.dtype.encode(a.fill.unwrap_or(0.0), a.dtype.little).repeat(n), a.dtype)
                }
                (None, None) => continue,
            };
            found += 1;
            // Each value's position in the chunk, row-major (or column-major).
            let local: Vec<u64> = (0..nd).map(|d| plan.fixed.get(d).copied().unwrap_or(0) % size(d)).collect();
            let strides: Vec<u64> = {
                let mut s = vec![1u64; nd];
                if a.fortran {
                    for d in 1..nd {
                        s[d] = s[d - 1] * size(d - 1);
                    }
                } else {
                    for d in (0..nd.saturating_sub(1)).rev() {
                        s[d] = s[d + 1] * size(d + 1);
                    }
                }
                s
            };
            let base: u64 = (0..nd).filter(|d| *d != plan.y && *d != plan.x).map(|d| local[d] * strides[d]).sum();
            for ly in 0..cy {
                let row = gy * cy + ly;
                if row < win.y0 {
                    continue;
                }
                if row >= win.y1 {
                    break;
                }
                for lx in 0..cx {
                    let col = gx * cx + lx;
                    if col < win.x0 {
                        continue;
                    }
                    if col >= win.x1 {
                        break;
                    }
                    let i = base + ly * strides[plan.y] + lx * strides[plan.x];
                    let at = usize::try_from(i).ok().and_then(|i| i.checked_mul(dt.size));
                    let Some(b) = at.and_then(|at| vals.get(at..at + dt.size)) else { continue };
                    let v = dt.value(b);
                    // Whole numbers are compared exactly: past 2^53 `v` has already been rounded.
                    let value = dt.whole(b).map_or(Exact::Float(v), Exact::Whole);
                    let is_missing = missing.iter().any(|m| m.same(value));
                    let out = if v.is_nan() || is_missing {
                        f32::NAN
                    } else {
                        let x = v * scale + offset;
                        let f = x as f32;
                        // A packed whole number is held when unscaling its f32 gives it back exactly.
                        // Float32 or narrower storage states no more precision than f32 holds, so
                        // scaling it is counted only when it leaves f32's range (overflowing to
                        // infinity, or a nonzero value to 0). A scaled Float64 (or anything scaled
                        // by 0) is held when f32 holds the scaled value.
                        let held = match dt.whole(b) {
                            _ if !scaled => dt.held_as_f32(b),
                            // A finite stored value scaled past f32's range (or f64's) is counted,
                            // whatever its type; a stored infinity stays one.
                            _ if v.is_finite() && f.is_infinite() => false,
                            Some(i) if scale != 0.0 => ((f64::from(f) - offset) / scale).round() as i128 == i,
                            None if dt.size <= 4 => !(f == 0.0 && x != 0.0),
                            _ => f64::from(f) == x,
                        };
                        rounded += u64::from(!held);
                        f
                    };
                    if let Some(g) = grid.get_mut((row - win.y0) as usize * w + (col - win.x0) as usize) {
                        *g = out;
                    }
                }
            }
        }
    }
    if found == 0 {
        return Err(DataError::NoChunks(a.path.clone()));
    }
    // Georeferencing from the coordinate arrays.
    let coords = |p: &Option<String>, start: u64, end: u64| -> Result<Option<(Vec<f64>, &Array)>> {
        match p.as_ref().and_then(|p| store.array(p)) {
            Some(c) => Ok(Some((coord_values(c, start, end, plan.windowed, chunks)?, c))),
            None => Ok(None),
        }
    };
    let (ys, xs) = (coords(&plan.ycoord, win.y0, win.y1)?, coords(&plan.xcoord, win.x0, win.x1)?);
    let (Some((ys, yc)), Some((xs, xc))) = (ys, xs) else {
        return Err(bad(format!("{}: its y and x dimensions have no coordinates, so it can't be placed on the map", a.path)));
    };
    let ((mut y0, mut dy), (mut x0, mut dx)) = match (axis(&ys), axis(&xs)) {
        (Some(y), Some(x)) => (y, x),
        _ => return Err(bad(format!("{}: its coordinates aren't evenly spaced (a curvilinear or irregular grid isn't read)", a.path))),
    };
    // North up and west to east: flip rows (or columns) as needed.
    if dy > 0.0 {
        let mut flipped = super::grid(w, h)?;
        for r in (0..h).rev() {
            flipped.extend_from_slice(grid.get(r * w..(r + 1) * w).unwrap_or_default());
        }
        grid = flipped;
        y0 += dy * h as f64;
        dy = -dy;
    }
    if dx < 0.0 {
        for r in 0..h {
            if let Some(row) = grid.get_mut(r * w..(r + 1) * w) {
                row.reverse();
            }
        }
        x0 += dx * w as f64;
        dx = -dx;
    }
    let lonlat = geographic(yc) && geographic(xc);
    let (crs, crs_note) = if lonlat {
        // The axes are read as longitude/latitude, whatever the WKT says; a shift it gives that
        // isn't applied is still said.
        (Some(Crs::LonLat), crs_of(store, a).and_then(|(_, note)| note))
    } else {
        crs_of(store, a).map_or((crs, None), |(crs, note)| (Some(crs), note))
    };
    let Some(crs) = crs else {
        return Err(bad(format!("{}: the store doesn't say its coordinate system; give \"crs\" (such as EPSG:3035)", a.path)));
    };
    // Longitudes of 0–360: a global grid is turned to start at -180; one wholly east of 180 is
    // moved by 360.
    if crs == Crs::LonLat && x0 + dx * w as f64 > 180.5 {
        if x0 >= 180.0 {
            x0 -= 360.0;
        } else if dx * w as f64 >= 359.0 && x0 > -1.0 - dx {
            let split = (((180.0 - x0) / dx).round().max(0.0) as usize).min(w);
            for r in 0..h {
                if let Some(row) = grid.get_mut(r * w..(r + 1) * w) {
                    row.rotate_left(split);
                }
            }
            x0 += dx * split as f64 - 360.0;
        }
    }
    let transform = GeoTransform { origin_x: x0, origin_y: y0, pixel_width: dx, pixel_height: dy };
    let data_type = match (a.dtype.kind, a.dtype.size) {
        (Kind::Float, 8) => "Float64",
        (Kind::Float, _) => "Float32",
        (Kind::Int, 1) => "Int8",
        (Kind::Int, 2) => "Int16",
        (Kind::Int, _) => "Int32",
        (Kind::UInt, 1) | (Kind::Bool, _) => "UInt8",
        (Kind::UInt, 2) => "UInt16",
        (Kind::UInt, _) => "UInt32",
    };
    let data_type = if scale != 1.0 || offset != 0.0 { "Float32" } else { data_type };
    let mut r = Raster::new(name, w as u32, h as u32, vec![grid], transform, crs, None, ColorHint::Gray, data_type)?;
    if rounded > 0 {
        r.rounded = vec![rounded];
    }
    r.lost.extend(crs_note);
    Ok(r)
}

// --- What a store holds --------------------------------------------------------------------------

/// The coordinate arrays [`info`] labels dimensions with (time, level): their chunk keys.
pub fn info_keys(store: &Store) -> Vec<String> {
    let mut keys = Vec::new();
    for v in store.variables() {
        let (y, x) = map_dims(v);
        for (d, name) in v.dim_names().iter().enumerate() {
            if d == y || d == x {
                continue;
            }
            if let Some(c) =
                store.coordinate(v, name).filter(|c| c.shape.first().is_some_and(|n| *n <= MAX_COORDS) && c.unsupported().is_none())
            {
                for k in all_keys(c) {
                    if !keys.contains(&k) {
                        keys.push(k);
                    }
                }
            }
        }
    }
    keys
}

/// The chunk keys of the coordinates of `variable`'s dimensions other than the map's (as
/// [`info_keys`] lists them for every variable).
pub fn coordinate_keys(store: &Store, variable: &str) -> Vec<String> {
    let Some(v) = store.array(variable) else { return Vec::new() };
    let (y, x) = map_dims(v);
    let mut keys = Vec::new();
    for (d, name) in v.dim_names().iter().enumerate() {
        if d == y || d == x {
            continue;
        }
        if let Some(c) =
            store.coordinate(v, name).filter(|c| c.shape.first().is_some_and(|n| *n <= MAX_COORDS) && c.unsupported().is_none())
        {
            keys.extend(all_keys(c).into_iter().filter(|k| !keys.contains(k)).collect::<Vec<_>>());
        }
    }
    keys
}

/// `variable`'s time dimension: the first of its dimensions other than the map's whose
/// coordinate has CF time units (`days since 2001-01-01`), with its steps as milliseconds since
/// 1970 (UTC). A `noleap` date is the same real date; a 360- or 366-day year is laid over its real
/// year in proportion (360_day's 30 February falls about 1 March), so its dates stay distinct and
/// in order. `None` when no dimension has such a coordinate, or one of its steps isn't a date.
pub fn time_axis(store: &Store, variable: &str, chunks: &HashMap<String, Vec<u8>>) -> Option<(String, Vec<f64>)> {
    let v = store.array(variable)?;
    let (y, x) = map_dims(v);
    for (d, name) in v.dim_names().iter().enumerate() {
        if d == y || d == x {
            continue;
        }
        // A coordinate of the dimension's own length (one found at the root, of another group,
        // may not be), so date `i` is step `i`.
        let Some(c) = store.coordinate(v, name).filter(|c| c.shape.first() == v.shape.get(d)) else { continue };
        if !attr_str(c, "units").is_some_and(|u| u.contains(" since ")) {
            continue;
        }
        let Ok(vals) = values_1d(c, chunks) else { continue };
        let ms: Option<Vec<f64>> = vals.iter().map(|t| time_ms(c, *t)).collect();
        if let Some(ms) = ms.filter(|m| !m.is_empty()) {
            return Some((name.clone(), ms));
        }
    }
    None
}

/// The chunk keys of `variable`'s map coordinates (its y and x), for [`locate`].
pub fn map_coordinate_keys(store: &Store, variable: &str) -> Vec<String> {
    let Some(a) = store.array(variable) else { return Vec::new() };
    let names = a.dim_names();
    let (y, x) = map_dims(a);
    let mut keys = Vec::new();
    for d in [y, x] {
        if let Some(c) = names.get(d).and_then(|n| store.coordinate(a, n)).filter(|c| c.shape.first() == a.shape.get(d)) {
            keys.extend(all_keys(c));
        }
    }
    keys
}

/// Whether `variable`'s map is a longitude/latitude grid; otherwise its coordinates are in the
/// coordinate system [`grid_crs`] gives (or the one a caller is told).
pub fn is_lonlat_grid(store: &Store, variable: &str) -> bool {
    let Some(a) = store.array(variable) else { return false };
    let names = a.dim_names();
    let (y, x) = map_dims(a);
    let geo = |d: usize| names.get(d).and_then(|n| store.coordinate(a, n)).is_some_and(geographic);
    geo(y) && geo(x)
}

/// The coordinate system `variable`'s attributes (or its grid mapping's) say, for a projected grid,
/// with the note [`crs_of`] gives (a shift the WKT gives that isn't applied).
pub fn grid_crs(store: &Store, variable: &str) -> Option<(Crs, Option<String>)> {
    store.array(variable).and_then(|a| crs_of(store, a))
}

/// What a coordinate system typed for a store says ([`super::wkt::typed_crs_note`]: a shift it
/// gives that isn't applied, its projection method's formula), when `r`, read from `variable`, is
/// in it. It is only the fallback: a store that gives its own system ([`grid_crs`]), or a
/// longitude/latitude grid, doesn't use it, so what it says isn't the raster's (#1678).
pub fn typed_crs_note(store: &Store, variable: &str, r: &Raster, typed: &str) -> Option<String> {
    if grid_crs(store, variable).is_some() {
        return None;
    }
    let crs = Crs::parse(typed).ok().filter(|c| *c == r.crs)?;
    super::wkt::typed_crs_note(typed, crs)
}

/// The stored (row, column) of `variable`'s map cell holding the point (`x`, `y`), in the grid's
/// own coordinates: degrees of any longitude convention (-180–180 or 0–360) for a
/// longitude/latitude grid. `None` outside the grid. The coordinates must be evenly spaced, as
/// for a slice.
pub fn locate(store: &Store, variable: &str, chunks: &HashMap<String, Vec<u8>>, x: f64, y: f64) -> Result<Option<(u64, u64)>> {
    let a = store.array(variable).ok_or_else(|| bad(format!("no variable named {variable}")))?;
    let names = a.dim_names();
    let (yd, xd) = map_dims(a);
    let coord = |d: usize| names.get(d).and_then(|n| store.coordinate(a, n)).filter(|c| c.shape.first() == a.shape.get(d));
    let (Some(yc), Some(xc)) = (coord(yd), coord(xd)) else {
        return Err(bad(format!("{}: its y and x dimensions have no coordinates", a.path)));
    };
    let (ys, xs) = (values_1d(yc, chunks)?, values_1d(xc, chunks)?);
    let (Some((y0, dy)), Some((x0, dx))) = (axis(&ys), axis(&xs)) else {
        return Err(bad(format!("{}: its coordinates aren't evenly spaced", a.path)));
    };
    let cell = |v: f64, v0: f64, dv: f64, n: usize| {
        let i = ((v - v0) / dv).floor();
        (i >= 0.0 && i < n as f64).then_some(i as u64)
    };
    let row = cell(y, y0, dy, ys.len());
    // A longitude may be written another way round the globe than the grid's.
    let turns: &[f64] = if geographic(xc) { &[0.0, 360.0, -360.0] } else { &[0.0] };
    let col = turns.iter().find_map(|k| cell(x + k, x0, dx, xs.len()));
    Ok(row.zip(col))
}

/// The chunk keys of `variable`'s map coordinates, for [`window_of`]: each is read whole (its
/// first [`MAX_CHUNKS`] chunks, as any coordinate), so longer ones than [`MAX_COORDS`] can't place
/// a box (a window is given in cells instead).
pub fn window_keys(store: &Store, variable: &str) -> Result<Vec<String>> {
    let a = store.array(variable).ok_or_else(|| bad(format!("no variable named {variable}")))?;
    let names = a.dim_names();
    let (y, x) = map_dims(a);
    for d in [y, x] {
        let n = a.shape.get(d).copied().unwrap_or(0);
        let coord = names.get(d).and_then(|n| store.coordinate(a, n)).filter(|c| c.shape.first() == a.shape.get(d));
        match coord {
            None => {
                let dim = names.get(d).map_or("", String::as_str);
                return Err(bad(format!("{}: its {dim} dimension has no coordinates to place a bbox on: give cells", a.path)));
            }
            Some(c) if n > MAX_COORDS => {
                return Err(bad(format!("{}: {n} values, more than the {MAX_COORDS} read to place a bbox: give cells", c.path)));
            }
            Some(_) => {}
        }
    }
    Ok(map_coordinate_keys(store, variable))
}

/// The window of `variable`'s map cells a box `[west, south, east, north]` covers, in the grid's own
/// coordinates (degrees, of either longitude convention, for a longitude/latitude grid), from its
/// coordinates (`chunks` from [`window_keys`]); whether the box reaches past the grid (only the
/// part within it is read); and whether it was widened to 2 × 2 cells, the fewest a window takes so
/// the cell size is known. A box round the whole globe, or holding the whole axis, is the whole
/// axis. The coordinates must be evenly spaced, as for a slice.
pub fn window_of(store: &Store, variable: &str, chunks: &HashMap<String, Vec<u8>>, bbox: [f64; 4]) -> Result<(Window, bool, bool)> {
    let [w, s, e, n] = bbox;
    if !bbox.iter().all(|v| v.is_finite()) || w >= e || s >= n {
        return Err(bad("bbox is [west, south, east, north], west below east and south below north"));
    }
    let a = store.array(variable).ok_or_else(|| bad(format!("no variable named {variable}")))?;
    let names = a.dim_names();
    let (yd, xd) = map_dims(a);
    let coord = |d: usize| names.get(d).and_then(|n| store.coordinate(a, n)).filter(|c| c.shape.first() == a.shape.get(d));
    let (Some(yc), Some(xc)) = (coord(yd), coord(xd)) else {
        return Err(bad(format!("{}: its y and x dimensions have no coordinates", a.path)));
    };
    let (ys, xs) = (values_1d(yc, chunks)?, values_1d(xc, chunks)?);
    let (Some((y0, dy)), Some((x0, dx))) = (axis(&ys), axis(&xs)) else {
        return Err(bad(format!("{}: its coordinates aren't evenly spaced", a.path)));
    };
    // The cells from `lo` to `hi` along an axis whose first edge is `v0`, a step `dv`, `len` of
    // them: the range, whether it was cut to the grid, and whether it was widened to two cells;
    // None when it misses it.
    let cells = |lo: f64, hi: f64, v0: f64, dv: f64, len: u64| {
        let (i, j) = if dv > 0.0 { ((lo - v0) / dv, (hi - v0) / dv) } else { ((hi - v0) / dv, (lo - v0) / dv) };
        // An edge on a cell's edge, to within rounding, takes no cell past it.
        let snap = |v: f64| if (v - v.round()).abs() < 1e-6 { v.round() } else { v };
        let (i, j) = (snap(i).floor(), snap(j).ceil());
        let (from, to) = (i.max(0.0) as u64, j.min(len as f64).max(0.0) as u64);
        (from < to).then(|| {
            // Two cells at least, so the window's coordinates give the cell size.
            let widened = to - from < 2;
            let (from, to) =
                if widened { (from.min(len.saturating_sub(2)), (from.min(len.saturating_sub(2)) + 2).min(len)) } else { (from, to) };
            (from, to, i < 0.0 || j > len as f64, widened)
        })
    };
    let rows = cells(s, n, y0, dy, ys.len() as u64);
    // A longitude may be written another way round the globe than the grid's.
    let turns: &[f64] = if geographic(xc) { &[0.0, 360.0, -360.0] } else { &[0.0] };
    let len = xs.len() as u64;
    let hits: Vec<_> = turns.iter().filter_map(|k| cells(w + k, e + k, x0, dx, len)).collect();
    // A box holding the whole axis in one turn, or (longitude) going round the globe, is the whole
    // axis: one run of columns.
    let whole = hits
        .iter()
        .find(|h| h.0 == 0 && h.1 == len)
        .copied()
        .or_else(|| (turns.len() > 1 && e - w >= 360.0).then_some((0, len, true, false)));
    // Over the grid's seam (0°/360°, or ±180°) a box is otherwise two runs of columns, which a
    // window isn't: refused rather than read on one side.
    if whole.is_none() && hits.len() > 1 {
        return Err(bad(format!(
            "{}: the bbox crosses the grid's edge at {:.0}°, so it is two parts of the grid: give a box on each side",
            a.path,
            x0 + dx * xs.len() as f64
        )));
    }
    let cols = whole.or_else(|| hits.first().copied());
    let (Some((y0, y1, cut_y, wide_y)), Some((x0, x1, cut_x, wide_x))) = (rows, cols) else {
        return Err(bad(format!("{}: the bbox lies outside its grid", a.path)));
    };
    Ok((Window { x0, y0, x1, y1 }, cut_x || cut_y, wide_x || wide_y))
}

/// What a point's series needs (see [`plan_series`]).
#[derive(Clone, Debug)]
pub struct SeriesPlan {
    pub keys: Vec<String>,
    var: String,
    /// The time dimension, and every dimension's step (the cell's, and the others' fixed ones).
    t: usize,
    at: Vec<u64>,
    from: u64,
    count: u64,
}

/// The chunks holding the stored cell (`row`, `col`) of `sel`'s variable along dimension `dim`,
/// from step `from` for `count` steps, the other dimensions at `sel`'s steps. At most
/// [`MAX_CHUNKS`]: a store chunked a step at a time over a long axis needs a shorter period.
pub fn plan_series(store: &Store, sel: &Selection, dim: &str, from: u64, count: u64, row: u64, col: u64) -> Result<SeriesPlan> {
    let a = store.array(&sel.variable).ok_or_else(|| bad(format!("no variable named {}", sel.variable)))?;
    if let Some(why) = a.unsupported() {
        return Err(bad(why));
    }
    let names = a.dim_names();
    let (y, x) = map_dims(a);
    let t = names
        .iter()
        .position(|n| n == dim)
        .filter(|t| *t != y && *t != x)
        .ok_or_else(|| bad(format!("{} has no dimension {dim}", a.path)))?;
    let n = a.shape.get(t).copied().unwrap_or(0);
    if count == 0 || from.checked_add(count).is_none_or(|end| end > n) {
        return Err(bad(format!("{dim} has {n} steps; {from} to {} is past them", from.saturating_add(count))));
    }
    if row >= a.shape.get(y).copied().unwrap_or(0) || col >= a.shape.get(x).copied().unwrap_or(0) {
        return Err(bad("the cell is outside the grid"));
    }
    let mut at = vec![0u64; a.shape.len()];
    for (d, i) in &sel.index {
        let Some(k) = names.iter().position(|nm| nm == d) else { return Err(bad(format!("{} has no dimension {d}", a.path))) };
        if k != y && k != x && k != t {
            if *i >= a.shape.get(k).copied().unwrap_or(0) {
                return Err(bad(format!("{d} has {} steps; {i} is past them", a.shape.get(k).copied().unwrap_or(0))));
            }
            if let Some(v) = at.get_mut(k) {
                *v = *i;
            }
        }
    }
    if let Some(v) = at.get_mut(y) {
        *v = row;
    }
    if let Some(v) = at.get_mut(x) {
        *v = col;
    }
    let ct = a.chunks.get(t).copied().unwrap_or(1).max(1);
    let (first, last) = (from / ct, (from + count - 1) / ct);
    if last - first + 1 > MAX_CHUNKS as u64 {
        return Err(bad(format!(
            "{count} steps of {} need {} chunks, more than {MAX_CHUNKS}: take a shorter period",
            a.path,
            last - first + 1
        )));
    }
    let keys = (first..=last)
        .map(|g| {
            let idx: Vec<u64> = (0..a.shape.len())
                .map(|d| if d == t { g } else { at.get(d).copied().unwrap_or(0) / a.chunks.get(d).copied().unwrap_or(1).max(1) })
                .collect();
            a.chunk_key(&idx)
        })
        .collect();
    Ok(SeriesPlan { keys, var: a.path.clone(), t, at, from, count })
}

/// The values of a [`plan_series`], one a step (`NaN` for no data, or a chunk that wasn't
/// fetched), scaled as a slice's are.
pub fn assemble_series(store: &Store, plan: &SeriesPlan, chunks: &HashMap<String, Vec<u8>>) -> Result<Vec<f64>> {
    let a = store.array(&plan.var).ok_or_else(|| bad("the variable is gone"))?;
    let nd = a.shape.len();
    let v2_fill = if a.zarr_format == 2 { a.fill } else { None };
    let mut missing: Vec<f64> = [v2_fill, attr_f64(a, "_FillValue"), attr_f64(a, "missing_value")].into_iter().flatten().collect();
    missing.dedup();
    let scale = attr_f64(a, "scale_factor").unwrap_or(1.0);
    let offset = attr_f64(a, "add_offset").unwrap_or(0.0);
    let chunk = |d: usize| a.chunks.get(d).copied().unwrap_or(1).max(1);
    // A cell's place in a chunk, by dimension: saturating, so chunk shapes from a hostile store
    // can't overflow (the value read is then just missing).
    let strides: Vec<u64> = {
        let mut s = vec![1u64; nd];
        if a.fortran {
            for d in 1..nd {
                s[d] = s[d - 1].saturating_mul(chunk(d - 1));
            }
        } else {
            for d in (0..nd.saturating_sub(1)).rev() {
                s[d] = s[d + 1].saturating_mul(chunk(d + 1));
            }
        }
        s
    };
    let step_at = |d: usize, step: u64| if d == plan.t { step } else { plan.at.get(d).copied().unwrap_or(0) };
    let mut out = Vec::with_capacity(usize::try_from(plan.count).unwrap_or(0).min(1 << 20));
    let mut decoded: Option<(u64, Option<(Vec<u8>, DType)>)> = None;
    for step in plan.from..plan.from.saturating_add(plan.count) {
        let g = step / chunk(plan.t);
        if decoded.as_ref().is_none_or(|(k, _)| *k != g) {
            let idx: Vec<u64> = (0..nd).map(|d| step_at(d, step) / chunk(d)).collect();
            let got = match chunks.get(&a.chunk_key(&idx)) {
                Some(b) => Some(a.decode_bytes(b)?),
                None => None,
            };
            decoded = Some((g, got));
        }
        let Some((_, Some((vals, dt)))) = decoded.as_ref() else {
            out.push(f64::NAN);
            continue;
        };
        let i = (0..nd)
            .map(|d| (step_at(d, step) % chunk(d)).saturating_mul(strides.get(d).copied().unwrap_or(0)))
            .fold(0u64, u64::saturating_add);
        let at = usize::try_from(i).ok().and_then(|i| i.checked_mul(dt.size));
        let v = at.and_then(|at| vals.get(at..at.checked_add(dt.size)?)).map(|b| dt.value(b)).unwrap_or(f64::NAN);
        out.push(if v.is_nan() || missing.contains(&v) { f64::NAN } else { v * scale + offset });
    }
    Ok(out)
}

/// A CF time coordinate's value as milliseconds since 1970 (see [`time_axis`]).
fn time_ms(c: &Array, v: f64) -> Option<f64> {
    let units = attr_str(c, "units")?;
    let calendar = attr_str(c, "calendar").unwrap_or("standard").to_ascii_lowercase();
    let (unit, since) = units.split_once(" since ")?;
    let secs = match unit.trim().to_ascii_lowercase().as_str() {
        "seconds" | "second" | "s" => 1.0,
        "minutes" | "minute" | "min" => 60.0,
        "hours" | "hour" | "h" => 3600.0,
        "days" | "day" | "d" => 86_400.0,
        _ => return None,
    };
    if !(v.is_finite() && v.abs() < 1e13) {
        return None;
    }
    let since = since.trim();
    let t = match calendar.as_str() {
        "standard" | "gregorian" | "proleptic_gregorian" => epoch_seconds(since)? + v * secs,
        "noleap" | "365_day" | "all_leap" | "366_day" | "360_day" => {
            // The time of day `since` gives counts too (`days since 2000-01-01 12:00`).
            let offset = v * secs + since.split_once([' ', 'T']).map_or(0.0, |(_, clock)| clock_seconds(clock));
            let day = fixed_calendar(&calendar, since, offset)?;
            let (Some(y), Some(m), Some(d)) = date_parts(&day) else { return None };
            let into_day = offset.rem_euclid(86_400.0);
            match calendar.as_str() {
                // Every day of a 365-day year is a real date.
                "noleap" | "365_day" => days_from_civil(y, m, d) as f64 * 86_400.0 + into_day,
                // 360 or 366 days: placed in the real year in proportion.
                _ => {
                    let (model, before): (f64, i64) = if calendar == "360_day" {
                        (360.0, (m - 1) * 30)
                    } else {
                        (
                            366.0,
                            [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335]
                                .get((m - 1).clamp(0, 11) as usize)
                                .copied()
                                .unwrap_or(0),
                        )
                    };
                    let start = days_from_civil(y, 1, 1);
                    let real = (days_from_civil(y + 1, 1, 1) - start) as f64;
                    (start as f64 + ((before + d - 1) as f64 + into_day / 86_400.0) / model * real) * 86_400.0
                }
            }
        }
        _ => return None,
    };
    let ms = t * 1000.0;
    ms.is_finite().then_some(ms)
}

/// A coordinate value as a label: a CF time (`days since 1850-01-01`) as a date, else the
/// number and units.
fn label(c: &Array, v: f64) -> String {
    let units = attr_str(c, "units").unwrap_or("");
    let calendar = attr_str(c, "calendar").unwrap_or("standard").to_ascii_lowercase();
    if let Some((unit, since)) = units.split_once(" since ") {
        let secs = match unit.trim().to_ascii_lowercase().as_str() {
            "seconds" | "second" | "s" => Some(1.0),
            "minutes" | "minute" | "min" => Some(60.0),
            "hours" | "hour" | "h" => Some(3600.0),
            "days" | "day" | "d" => Some(86_400.0),
            _ => None,
        };
        let since = since.trim();
        if let Some(s) = secs.filter(|_| v.is_finite() && v.abs() < 1e13) {
            match calendar.as_str() {
                "standard" | "gregorian" | "proleptic_gregorian" => {
                    if let Some(base) = epoch_seconds(since) {
                        // `as` saturates rather than wraps.
                        return date_label((base + v * s) as i64);
                    }
                }
                // Model calendars: years of fixed length.
                "noleap" | "365_day" | "all_leap" | "366_day" | "360_day" => {
                    if let Some(l) = fixed_calendar(&calendar, since, v * s) {
                        return l;
                    }
                }
                _ => {}
            }
        }
    }
    let n = if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") };
    if units.is_empty() { n } else { format!("{n} {units}") }
}

/// A date's year, month and day as its text has them: `2001-03-05`, or `-0001-01-01` for a year
/// before 1 CE (the sign is the year's, not a separator).
fn date_parts(s: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let (sign, rest) = match s.trim().strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.trim()),
    };
    let mut p = rest.splitn(3, '-').map(|x| x.trim().parse::<i64>().ok());
    (p.next().flatten().map(|y| sign * y), p.next().flatten(), p.next().flatten())
}

/// A date in a model calendar of fixed-length years (365, 366 or 360 days): `secs` after `since`.
fn fixed_calendar(calendar: &str, since: &str, secs: f64) -> Option<String> {
    let (y, m, d) = date_parts(since.split([' ', 'T']).next()?);
    // The parts of a hostile `units` string are kept to sane ranges, so nothing below overflows.
    let (y0, m0, d0) = (y?.clamp(-MAX_YEAR, MAX_YEAR), m.unwrap_or(1).clamp(1, 12), d.unwrap_or(1).clamp(1, 31));
    let months: [i64; 12] = match calendar {
        "360_day" => [30; 12],
        "all_leap" | "366_day" => [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
        _ => [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    };
    let year: i64 = months.iter().sum();
    let start = months.iter().take((m0 - 1) as usize).sum::<i64>() + d0 - 1;
    // `as` saturates a float past i64's range; the sums saturate too.
    let total = ((secs / 86_400.0).floor() as i64).saturating_add(start);
    let (y, mut doy) = (y0.saturating_add(total.div_euclid(year)), total.rem_euclid(year));
    let mut m = 0usize;
    while m < 11 && doy >= months[m] {
        doy -= months[m];
        m += 1;
    }
    Some(format!("{y:04}-{:02}-{:02}", m + 1, doy + 1))
}

/// Seconds since 1970 of `YYYY-MM-DD[ T]HH:MM:SS`.
fn epoch_seconds(s: &str) -> Option<f64> {
    let (date, time) = s.split_once([' ', 'T']).unwrap_or((s, "0:0:0"));
    let (y, m, day) = date_parts(date);
    let (y, m, day) = (y?.clamp(-MAX_YEAR, MAX_YEAR), m.unwrap_or(1).clamp(1, 12), day.unwrap_or(1).clamp(1, 31));
    Some(days_from_civil(y, m, day) as f64 * 86_400.0 + clock_seconds(time))
}

/// Seconds into the day of `HH:MM:SS` (parts missing are 0; a trailing `Z` is dropped), kept
/// within a few days so a hostile string can't push a date far off.
fn clock_seconds(time: &str) -> f64 {
    let mut t = time.trim().trim_end_matches('Z').split(':').map(|p| p.trim().parse::<f64>().ok().filter(|v| v.is_finite()));
    let (hh, mm, ss) = (t.next().flatten().unwrap_or(0.0), t.next().flatten().unwrap_or(0.0), t.next().flatten().unwrap_or(0.0));
    (hh * 3600.0 + mm * 60.0 + ss).clamp(-7.0 * 86_400.0, 7.0 * 86_400.0)
}

/// The years a `since` date may name: far past any data, small enough that day counts can't
/// overflow.
const MAX_YEAR: i64 = 1_000_000;

fn date_label(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    if secs == 0 { format!("{y:04}-{m:02}-{d:02}") } else { format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60) }
}

/// Labels for many values: at most this many are listed (the dialog shows one at a time); longer
/// dimensions give their first and last.
const MAX_LABELS: usize = 100_000;

/// What a store holds: its variables with their dimensions (sizes, and labels where the
/// coordinates were fetched: `chunks` from [`info_keys`]).
pub fn info(store: &Store, chunks: &HashMap<String, Vec<u8>>) -> Value {
    let vars: Vec<Value> = store
        .variables()
        .iter()
        .map(|v| {
            let (y, x) = map_dims(v);
            let dims: Vec<Value> = v
                .dim_names()
                .iter()
                .enumerate()
                .map(|(d, name)| {
                    let size = v.shape.get(d).copied().unwrap_or(0);
                    let mut o = serde_json::json!({"name": name, "size": size, "map": d == y || d == x});
                    if d != y
                        && d != x
                        && let Some(c) = store.coordinate(v, name)
                        && let Ok(vals) = values_1d(c, chunks)
                    {
                        // Long ones (hourly over decades) give their ends; dim_label gives any step.
                        if vals.len() <= MAX_LABELS {
                            let labels: Vec<String> = vals.iter().map(|x| label(c, *x)).collect();
                            o["labels"] = serde_json::json!(labels);
                        } else if let (Some(a), Some(b)) = (vals.first(), vals.last()) {
                            o["first"] = serde_json::json!(label(c, *a));
                            o["last"] = serde_json::json!(label(c, *b));
                        }
                    }
                    o
                })
                .collect();
            serde_json::json!({
                "name": v.path,
                "longName": attr_str(v, "long_name").or_else(|| attr_str(v, "standard_name")),
                "units": attr_str(v, "units"),
                "dtype": format!("{:?}{}", v.dtype.kind, v.dtype.size * 8),
                "dims": dims,
                "chunks": v.chunks,
                // A sharded variable's chunks inside its shards: what a window reads of them.
                "innerChunks": match v.codecs.first() { Some(Codec::Sharding { inner, .. }) => Some(inner), _ => None },
                "readable": v.unsupported().is_none(),
                "why": v.unsupported(),
            })
        })
        .collect();
    serde_json::json!({"count": vars.len(), "variables": vars})
}

// --- A folder on this computer -------------------------------------------------------------------

/// Whether a folder is a Zarr store (or one of its arrays).
#[cfg(not(target_arch = "wasm32"))]
pub fn is_store(dir: &std::path::Path) -> bool {
    dir.is_dir() && ["zarr.json", ".zmetadata", ".zarray", ".zgroup"].iter().any(|k| dir.join(k).is_file())
}

/// A folder's metadata: consolidated, else each array's files (up to four levels deep).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_metadata(dir: &std::path::Path) -> Result<Store> {
    fn read(docs: &mut Vec<(String, Vec<u8>)>, rel: String, p: std::path::PathBuf) -> Result<()> {
        if p.metadata().map(|m| m.len()).unwrap_or(0) as usize > MAX_METADATA {
            return Err(bad(format!("{} is too large", p.display())));
        }
        if let Ok(b) = std::fs::read(&p) {
            docs.push((rel, b));
        }
        Ok(())
    }
    let mut docs = Vec::new();
    for k in METADATA_KEYS {
        if dir.join(k).is_file() {
            read(&mut docs, k.to_string(), dir.join(k))?;
        }
    }
    // Consolidated: a .zmetadata, or a root zarr.json whose consolidated_metadata is an object
    // (zarr-python 3 writes `"consolidated_metadata": null` for a store that isn't).
    let consolidated = docs.iter().any(|(k, b)| {
        k == ".zmetadata"
            || (k == "zarr.json"
                && serde_json::from_slice::<Value>(b).ok().is_some_and(|v| v.get("consolidated_metadata").is_some_and(Value::is_object)))
    });
    if !consolidated {
        let mut stack = vec![(dir.to_path_buf(), String::new(), 0usize)];
        let mut seen = 0usize;
        while let Some((d, rel, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten().take(MAX_ARRAYS) {
                seen += 1;
                if seen > 4 * MAX_ARRAYS {
                    break;
                }
                let p = e.path();
                if !p.is_dir() || depth >= 4 {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                for k in [".zarray", ".zattrs", "zarr.json"] {
                    if p.join(k).is_file() {
                        read(&mut docs, format!("{r}/{k}"), p.join(k))?;
                    }
                }
                if p.join(".zgroup").is_file() || p.join("zarr.json").is_file() {
                    stack.push((p, r, depth + 1));
                }
            }
        }
    }
    parse_store(&docs)
}

/// Reads a slice of a store in a folder (`selection` as [`Selection::label`] writes it).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_dir(dir: &std::path::Path, selection: &str, crs: Option<&str>) -> Result<Raster> {
    read_dir_in(&read_metadata(dir)?, dir, selection, crs)
}

/// [`read_dir`] with the store's metadata already read ([`read_metadata`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_dir_in(store: &Store, dir: &std::path::Path, selection: &str, crs: Option<&str>) -> Result<Raster> {
    let mut sel = Selection::parse_strict(selection)?;
    if sel.variable.is_empty() {
        sel.variable = store
            .variables()
            .iter()
            .find(|v| v.unsupported().is_none())
            .map(|v| v.path.clone())
            .ok_or_else(|| bad("the store has no variable that can be read"))?;
    }
    let p = plan(store, &sel)?;
    let mut chunks = HashMap::new();
    let mut total = 0u64;
    for k in &p.keys {
        let path = dir.join(k);
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            // Never written: only a file that isn't there. Any other failure is said.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        total = total.saturating_add(meta.len());
        if !meta.is_file() || meta.len() > MAX_CHUNK_FILE || total > MAX_READ {
            return Err(bad(format!("{}: the slice's chunks are too large to read", dir.display())));
        }
        chunks.insert(k.clone(), std::fs::read(&path)?);
    }
    // A window's bytes of files: shards' indexes and coordinates, then the shards' chunks.
    let mut read = |parts: &[Part], chunks: &mut HashMap<String, Vec<u8>>| -> Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        for part in parts {
            let mut f = match std::fs::File::open(dir.join(&part.key)) {
                Ok(f) => f,
                // Never written: only a file that isn't there. Any other failure is said.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let len = f.metadata()?.len();
            let (first, n) = match part.span {
                Span::Bytes(a, b) => (a, b - a + 1),
                Span::Tail(n) => (len.saturating_sub(n), n.min(len)),
            };
            total = total.saturating_add(n);
            if first.saturating_add(n) > len || n > MAX_CHUNK_FILE || total > MAX_READ {
                return Err(bad(format!("{}: {} is cut, or the window's chunks are too large to read", dir.display(), part.key)));
            }
            f.seek(SeekFrom::Start(first))?;
            let mut buf = Vec::new();
            f.take(n).read_to_end(&mut buf)?;
            chunks.insert(part.id.clone(), buf);
        }
        Ok(())
    };
    read(&p.parts, &mut chunks)?;
    let inner = inner_parts(store, &p, &chunks)?;
    read(&inner, &mut chunks)?;
    let crs = crs.map(Crs::parse).transpose().map_err(DataError::Format)?;
    let stem = dir.file_stem().and_then(|s| s.to_str()).unwrap_or("zarr");
    let mut r = assemble(store, &p, &chunks, &format!("{stem} {}", sel.label()), crs)?;
    r.source = Some(dir.display().to_string());
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CF date's parts, a year before 1 CE included (its sign isn't a separator).
    #[test]
    fn dates_before_1_ce() {
        assert_eq!(date_parts("2001-03-05"), (Some(2001), Some(3), Some(5)));
        assert_eq!(date_parts("-0001-01-01"), (Some(-1), Some(1), Some(1)));
        assert_eq!(date_parts("1850"), (Some(1850), None, None));
        assert_eq!(date_parts("x-1-1"), (None, Some(1), Some(1)));
        assert!(fixed_calendar("noleap", "-0001-01-01", 0.0).is_some());
    }

    /// Model calendars: the time of day in `since` counts, and daily 360- and 366-day dates stay
    /// distinct and in order (30 February and 1 March are two days).
    #[test]
    fn model_calendar_dates_stay_apart() {
        let arr = serde_json::json!({"shape": [2], "chunks": [2], "dtype": "<f4", "compressor": null, "fill_value": null, "filters": null, "order": "C", "zarr_format": 2});
        let meta = serde_json::json!({"metadata": {"t/.zarray": arr}});
        let st = parse_store(&[(".zmetadata".into(), meta.to_string().into_bytes())]).unwrap();
        let at = |cal: &str, units: &str, v: f64| {
            let mut t = st.arrays[0].clone();
            t.attrs.insert("units".into(), Value::String(units.into()));
            t.attrs.insert("calendar".into(), Value::String(cal.into()));
            time_ms(&t, v).unwrap()
        };
        // Noon on 1 January 2000, in a standard and a noleap calendar alike.
        let noon = 946_728_000_000.0;
        assert_eq!(at("standard", "days since 2000-01-01 12:00", 0.0), noon);
        assert_eq!(at("noleap", "days since 2000-01-01 12:00", 0.0), noon);
        assert_eq!(at("noleap", "hours since 2000-01-01T06:00:00", 6.0), noon);
        for cal in ["360_day", "all_leap", "366_day"] {
            let days: Vec<f64> = (0..800).map(|d| at(cal, "days since 2001-01-01", f64::from(d))).collect();
            assert!(days.windows(2).all(|w| w[0] < w[1]), "{cal}: not in order");
            // A year's first day is the real year's.
            assert_eq!(days[0], 978_307_200_000.0, "{cal}");
        }
    }

    fn lz4_block(raw: &[u8]) -> Vec<u8> {
        lz4_flex::block::compress(raw)
    }

    /// A blosc frame of one block, LZ4, byte-shuffled.
    fn blosc_frame(raw: &[u8], typesize: u8) -> Vec<u8> {
        let shuffled = {
            let n = raw.len() / typesize as usize;
            let mut o = vec![0u8; raw.len()];
            for j in 0..n {
                for i in 0..typesize as usize {
                    o[i * n + j] = raw[j * typesize as usize + i];
                }
            }
            o
        };
        // Not split (flag 0x10), so one stream.
        let c = lz4_block(&shuffled);
        let mut f = vec![2u8, 1, 0x1 | 0x10 | (1 << 5), typesize];
        f.extend((raw.len() as u32).to_le_bytes());
        f.extend((raw.len() as u32).to_le_bytes());
        f.extend(((16 + 4 + 4 + c.len()) as u32).to_le_bytes());
        f.extend(20u32.to_le_bytes());
        f.extend((c.len() as i32).to_le_bytes());
        f.extend(c);
        f
    }

    /// A blosc frame as c-blosc writes LZ4 with byte shuffle: full blocks split into a stream per
    /// byte of the type (no 0x10 flag), the last, shorter block in one stream.
    fn blosc_blocks(raw: &[u8], typesize: usize, blocksize: usize) -> Vec<u8> {
        let shuffle = |b: &[u8]| {
            let n = b.len() / typesize;
            let mut o = b.to_vec();
            for j in 0..n {
                for i in 0..typesize {
                    o[i * n + j] = b[j * typesize + i];
                }
            }
            o
        };
        let blocks: Vec<&[u8]> = raw.chunks(blocksize).collect();
        let mut body = Vec::new();
        let mut starts = Vec::new();
        let header = 16 + 4 * blocks.len();
        for b in &blocks {
            starts.push((header + body.len()) as u32);
            let s = shuffle(b);
            let streams = if b.len() == blocksize { typesize } else { 1 };
            for part in s.chunks(s.len() / streams) {
                let c = lz4_block(part);
                body.extend((c.len() as i32).to_le_bytes());
                body.extend(c);
            }
        }
        let mut f = vec![2u8, 1, 0x1 | (1 << 5), typesize as u8];
        f.extend((raw.len() as u32).to_le_bytes());
        f.extend((blocksize as u32).to_le_bytes());
        f.extend(((header + body.len()) as u32).to_le_bytes());
        for s in starts {
            f.extend(s.to_le_bytes());
        }
        f.extend(body);
        f
    }

    #[test]
    fn blosc_split_blocks_and_a_leftover() {
        // Two full blocks split in four streams, then a shorter one that isn't split.
        let raw: Vec<u8> = (0..2700u32).flat_map(|v| ((v % 97) as f32 * 1.5).to_le_bytes()).collect();
        let f = blosc_blocks(&raw, 4, 4096);
        assert_eq!(blosc(&f, raw.len()).unwrap(), raw);
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// A consolidated Zarr 2 store: `t(time=2, lat=2, lon=4)` in chunks of (1, 2, 2), with lat, lon
    /// and time coordinates, blosc-LZ4 compressed.
    fn store_v2() -> (Store, HashMap<String, Vec<u8>>) {
        let arr = |shape: &str, chunks: &str, dtype: &str, comp: &str| {
            serde_json::from_str::<Value>(&format!(
                r#"{{"shape":{shape},"chunks":{chunks},"dtype":"{dtype}","compressor":{comp},"fill_value":"NaN","filters":null,"order":"C","zarr_format":2}}"#
            ))
            .unwrap()
        };
        let blosc = r#"{"id":"blosc","cname":"lz4","clevel":5,"shuffle":1,"blocksize":0}"#;
        let meta = serde_json::json!({"metadata": {
            ".zgroup": {"zarr_format": 2},
            "t/.zarray": arr("[2,2,4]", "[1,2,2]", "<f4", blosc),
            "t/.zattrs": {"_ARRAY_DIMENSIONS": ["time","lat","lon"], "units": "K", "long_name": "temperature"},
            "lat/.zarray": arr("[2]", "[2]", "<f8", "null"),
            "lat/.zattrs": {"_ARRAY_DIMENSIONS": ["lat"], "units": "degrees_north"},
            "lon/.zarray": arr("[4]", "[4]", "<f8", r#"{"id":"zlib","level":1}"#),
            "lon/.zattrs": {"_ARRAY_DIMENSIONS": ["lon"], "units": "degrees_east"},
            "time/.zarray": arr("[2]", "[2]", "<i8", "null"),
            "time/.zattrs": {"_ARRAY_DIMENSIONS": ["time"], "units": "hours since 2000-01-01", "calendar": "proleptic_gregorian"},
        }, "zarr_consolidated_format": 1});
        let store = parse_store(&[(".zmetadata".into(), meta.to_string().into_bytes())]).unwrap();
        let mut chunks = HashMap::new();
        // Latitude ascending (south to north), longitude 0–360 (a global grid of 90° cells).
        chunks.insert("lat/0".to_string(), [-45.0f64, 45.0].iter().flat_map(|x| x.to_le_bytes()).collect());
        let lon: Vec<u8> = [45.0f64, 135.0, 225.0, 315.0].iter().flat_map(|x| x.to_le_bytes()).collect();
        chunks.insert("lon/0".to_string(), miniz_oxide::deflate::compress_to_vec_zlib(&lon, 6));
        chunks.insert("time/0".to_string(), [0i64, 30].iter().flat_map(|x| x.to_le_bytes()).collect());
        // time 1: values 100 + 10·row + col over the whole (lat, lon) grid.
        for cx in 0..2u32 {
            let mut v = Vec::new();
            for r in 0..2u32 {
                for c in 0..2u32 {
                    v.push((100 + 10 * r + cx * 2 + c) as f32);
                }
            }
            chunks.insert(format!("t/1.0.{cx}"), blosc_frame(&f32s(&v), 4));
        }
        (store, chunks)
    }

    /// The rounded count of a 2 × 2 variable of type `dtype` holding `values` (its bytes), with
    /// `attrs` (scaling, a fill value) added to its attributes, on latitude and longitude.
    fn two_by_two(dtype: &str, attrs: Value, values: Vec<u8>) -> Vec<u64> {
        let arr = |shape: &str, dtype: &str| {
            serde_json::from_str::<Value>(&format!(
                r#"{{"shape":{shape},"chunks":{shape},"dtype":"{dtype}","compressor":null,"fill_value":null,"filters":null,"order":"C","zarr_format":2}}"#
            ))
            .unwrap()
        };
        let mut v_attrs = serde_json::json!({"_ARRAY_DIMENSIONS": ["lat","lon"]});
        if let (Some(to), Some(from)) = (v_attrs.as_object_mut(), attrs.as_object()) {
            to.extend(from.clone());
        }
        let meta = serde_json::json!({"metadata": {
            ".zgroup": {"zarr_format": 2},
            "v/.zarray": arr("[2,2]", dtype),
            "v/.zattrs": v_attrs,
            "lat/.zarray": arr("[2]", "<f8"),
            "lat/.zattrs": {"_ARRAY_DIMENSIONS": ["lat"], "units": "degrees_north"},
            "lon/.zarray": arr("[2]", "<f8"),
            "lon/.zattrs": {"_ARRAY_DIMENSIONS": ["lon"], "units": "degrees_east"},
        }, "zarr_consolidated_format": 1});
        let store = parse_store(&[(".zmetadata".into(), meta.to_string().into_bytes())]).unwrap();
        let f64s = |v: [f64; 2]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let chunks =
            HashMap::from([("v/0.0".to_string(), values), ("lat/0".into(), f64s([10.0, 20.0])), ("lon/0".into(), f64s([30.0, 40.0]))]);
        let p = plan(&store, &Selection { variable: "v".into(), index: vec![], window: None }).unwrap();
        assemble(&store, &p, &chunks, "v", None).unwrap().rounded
    }

    #[test]
    fn whole_no_data_values_are_compared_exactly() {
        for (file, samples, _, counted) in crate::raster::NODATA_WHOLE_CASES {
            let bytes = samples.iter().flat_map(|x| x.to_le_bytes()).collect();
            // As a JSON number, as zarr-python writes an integer fill value.
            let n: i64 = file.parse().unwrap();
            assert_eq!(two_by_two("<i8", serde_json::json!({"_FillValue": n}), bytes).iter().sum::<u64>(), *counted, "fill {file}");
        }
    }

    #[test]
    fn no_data_values_are_left_out_of_the_count_by_one_rule() {
        for c in crate::raster::NODATA_CASES {
            let attrs = c.file.map_or(serde_json::json!({}), |f| serde_json::json!({"_FillValue": f}));
            let bytes = c.samples.iter().flat_map(|x| x.to_le_bytes()).collect();
            assert_eq!(two_by_two("<f8", attrs, bytes).iter().sum::<u64>(), c.zarr, "fill {:?}", c.file);
        }
    }

    #[test]
    fn values_f32_cannot_hold_are_counted() {
        // A 2 × 2 variable of type `dtype` holding `values`, scaled by `scale` and `offset`, on
        // latitude and longitude.
        let read_scaled = |dtype: &str, (scale, offset): (f64, f64), values: Vec<u8>| {
            two_by_two(dtype, serde_json::json!({"scale_factor": scale, "add_offset": offset}), values)
        };
        let read = |dtype: &str, values: Vec<u8>| read_scaled(dtype, (1.0, 0.0), values);
        let i64s = |v: [i64; 4]| v.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(read("<i8", i64s([16_777_217, 1 << 40, 3, -16_777_216])), vec![1]);
        assert!(read("<i8", i64s([16_777_216, 1, 2, 3])).is_empty());
        // Past 2^53 a fill value is matched exactly: i64::MIN + 1 isn't missing, and is counted.
        let fill = serde_json::json!({"_FillValue": "-9223372036854775808"});
        assert_eq!(two_by_two("<i8", fill, i64s([i64::MIN, i64::MIN + 1, 0, 1])), vec![1]);
        // Past 2^53 too, where an f64 has already rounded 2^60 + 1 to 2^60.
        assert_eq!(read("<i8", i64s([(1 << 60) + 1, 1 << 60, 0, 0])), vec![1]);
        let u64s = |v: [u64; 4]| v.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(read("<u8", u64s([(1 << 60) + 1, 1 << 60, u64::MAX, 0])), vec![2]);
        let f64s = |v: [f64; 4]| v.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(read("<f8", f64s([0.1, 1234567.891234, 0.5, f64::NAN])), vec![2]);
        assert!(read("<f8", f64s([0.5, 2.0, -3.25, f64::NAN])).is_empty());
        // Scaled, the value written counts: packed whole numbers are held while unscaling their
        // f32 gives them back, a scaled float when f32 holds the scaled value. Halving 16777217
        // doesn't help: 8388608.5 needs the same 25 bits, and comes back as 16777216.
        let i32s = |v: [i32; 4]| v.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(read_scaled("<i4", (0.5, 0.0), i32s([16_777_217, 3, 5, 7])), vec![1]);
        assert!(read_scaled("<i4", (0.01, 273.15), i32s([-32_768, 1, 2_315, 32_767])).is_empty());
        assert_eq!(read_scaled("<i4", (1.0, 0.25), i32s([100_000_001, 1, 2, 3])), vec![1]);
        assert_eq!(read_scaled("<f8", (0.1, 0.0), f64s([1.0, 5.0, 10.0, f64::NAN])), vec![1]);
        // The same values stored as Float32 or Float16 hold no more than f32 does: none counted.
        assert!(read_scaled("<f4", (0.1, 0.0), f32s(&[1.0, 1.5, 10.0, 0.123_456_7])).is_empty());
        let f16s = |v: [u16; 4]| v.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert!(read_scaled("<f2", (0.1, 273.15), f16s([0x3c00, 0x3e00, 0x4900, 0])).is_empty());
        // Unless scaling leaves f32's range: 3e38 × 10 overflows and 1e-30 × 1e-20 vanishes, while
        // an infinity stored as such stays one.
        assert_eq!(read_scaled("<f4", (10.0, 0.0), f32s(&[3e38, 1.0, f32::INFINITY, 0.0])), vec![1]);
        assert_eq!(read_scaled("<f4", (1e-20, 0.0), f32s(&[1e-30, 1.0, 0.0, 2.0])), vec![1]);
        // Past f64's range too, for Float32 and Float64 storage alike.
        assert_eq!(read_scaled("<f4", (1e300, 0.0), f32s(&[1e30, 0.0, f32::INFINITY, 0.0])), vec![1]);
        assert_eq!(read_scaled("<f8", (1e10, 0.0), f64s([1e300, 0.0, f64::INFINITY, 0.5])), vec![1]);
        // Scaled 64-bit whole numbers are compared exactly too, and a scale of 0 writes the offset.
        assert_eq!(read_scaled("<i8", (2.0, 0.0), i64s([(1 << 60) + 1, 1 << 60, 3, 0])), vec![1]);
        assert_eq!(read_scaled("<u8", (2.0, 0.0), u64s([(1 << 60) + 1, 1 << 60, 3, 0])), vec![1]);
        assert!(read_scaled("<i4", (0.0, 5.0), i32s([1, 2, 16_777_217, 4])).is_empty());
    }

    #[test]
    fn reads_a_slice_of_a_v2_store() {
        let (store, chunks) = store_v2();
        assert_eq!(store.variables().len(), 1);
        let sel = Selection { variable: "t".into(), index: vec![("time".into(), 1)], window: None };
        let p = plan(&store, &sel).unwrap();
        assert!(
            p.keys.contains(&"t/1.0.0".to_string()) && p.keys.contains(&"t/1.0.1".to_string()) && p.keys.contains(&"lat/0".to_string())
        );
        let r = assemble(&store, &p, &chunks, "t", None).unwrap();
        assert_eq!((r.width, r.height, r.crs), (4, 2, Crs::LonLat));
        // North up (rows flipped) and turned to start at -180 (columns from 180° first).
        assert_eq!(r.transform, GeoTransform { origin_x: -180.0, origin_y: 90.0, pixel_width: 90.0, pixel_height: -90.0 });
        let row0: Vec<f32> = r.bands[0].data[..4].to_vec();
        assert_eq!(row0, vec![112.0, 113.0, 110.0, 111.0]);
        // A missing chunk is no data; none at all is an error.
        let mut fewer = chunks.clone();
        fewer.remove("t/1.0.1");
        let r = assemble(&store, &p, &fewer, "t", None).unwrap();
        assert!(r.bands[0].data.iter().filter(|v| v.is_nan()).count() == 4);
        let p0 = plan(&store, &Selection { variable: "t".into(), index: vec![], window: None }).unwrap();
        assert!(assemble(&store, &p0, &chunks, "t", None).is_err());
        // Out of range, unknown names.
        assert!(plan(&store, &Selection { variable: "t".into(), index: vec![("time".into(), 2)], window: None }).is_err());
        assert!(plan(&store, &Selection { variable: "nope".into(), index: vec![], window: None }).is_err());
        assert!(plan(&store, &Selection { variable: "t".into(), index: vec![("depth".into(), 0)], window: None }).is_err());
        // Time labels.
        let info = info(&store, &chunks);
        assert_eq!(info["variables"][0]["dims"][0]["labels"], serde_json::json!(["2000-01-01", "2000-01-02 06:00"]));
        assert_eq!(info["variables"][0]["longName"], "temperature");
    }

    #[test]
    fn v3_metadata_codecs_and_keys() {
        let z = serde_json::json!({
            "zarr_format": 3, "node_type": "array", "shape": [3, 4], "data_type": "int16",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [3, 4]}},
            "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
            "codecs": [{"name": "bytes", "configuration": {"endian": "big"}}, {"name": "zstd", "configuration": {"level": 1}}],
            "fill_value": -9999, "dimension_names": ["y", "x"], "attributes": {"epsg_code": 3857, "_FillValue": -9999}
        });
        let root = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
            "elev": z,
            "y": {"zarr_format": 3, "node_type": "array", "shape": [3], "data_type": "float64", "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [3]}}, "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}], "fill_value": 0, "dimension_names": ["y"]},
            "x": {"zarr_format": 3, "node_type": "array", "shape": [4], "data_type": "float64", "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [4]}}, "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}, {"name": "crc32c"}], "fill_value": 0, "dimension_names": ["x"]},
        }}});
        let store = parse_store(&[("zarr.json".into(), root.to_string().into_bytes())]).unwrap();
        let a = store.array("elev").unwrap();
        assert_eq!(a.chunk_key(&[0, 0]), "elev/c/0/0");
        let p = plan(&store, &Selection { variable: "elev".into(), index: vec![], window: None }).unwrap();
        assert_eq!(p.keys, vec!["elev/c/0/0", "y/c/0", "x/c/0"]);
        // A zstd frame isn't made here (ruzstd decodes only): the chunk is checked raw instead.
        let raw: Vec<u8> = (0..12i16).map(|v| if v == 5 { -9999 } else { v * 10 }).flat_map(|v| v.to_be_bytes()).collect();
        let mut plain = a.clone();
        plain.codecs.retain(|c| *c != Codec::Zstd);
        let vals = plain.decode(&raw).unwrap();
        assert_eq!(vals[..3], [0.0, 10.0, 20.0]);
        let mut st2 = store.clone();
        if let Some(e) = st2.arrays.iter_mut().find(|x| x.path == "elev") {
            *e = plain;
        }
        let mut chunks = HashMap::new();
        chunks.insert("elev/c/0/0".to_string(), raw);
        chunks.insert("y/c/0".to_string(), [300.0f64, 200.0, 100.0].iter().flat_map(|x| x.to_le_bytes()).collect());
        let mut x: Vec<u8> = [0.0f64, 10.0, 20.0, 30.0].iter().flat_map(|x| x.to_le_bytes()).collect();
        x.extend([0u8; 4]);
        chunks.insert("x/c/0".to_string(), x);
        let r = assemble(&st2, &p, &chunks, "elev", None).unwrap();
        assert_eq!(r.crs, Crs::WebMercator);
        assert_eq!(r.transform, GeoTransform { origin_x: -5.0, origin_y: 350.0, pixel_width: 10.0, pixel_height: -100.0 });
        assert!(r.bands[0].data[5].is_nan());
        // A Zarr 3 fill_value (0, zarr-python's default) isn't no-data: real zeros stay, and only
        // the CF _FillValue masks.
        if let Some(e) = st2.arrays.iter_mut().find(|x| x.path == "elev") {
            e.fill = Some(0.0);
            e.attrs.remove("_FillValue");
        }
        let r = assemble(&st2, &p, &chunks, "elev", None).unwrap();
        assert_eq!(r.bands[0].data[0], 0.0);
        assert_eq!(r.bands[0].data[5], -9999.0);
        // Unknown codecs, and shards that can't be read, are said, not misread.
        let refused = |codecs: Value, why: &str| {
            let mut r = root.clone();
            r["consolidated_metadata"]["metadata"]["elev"]["codecs"] = codecs;
            let s = parse_store(&[("zarr.json".into(), r.to_string().into_bytes())]).unwrap();
            let e = plan(&s, &Selection { variable: "elev".into(), index: vec![], window: None }).unwrap_err().to_string();
            assert!(e.contains(why), "{e}");
        };
        let shard = |inner: Value, codecs: Value| serde_json::json!({"name": "sharding_indexed", "configuration": {"chunk_shape": inner, "codecs": codecs, "index_codecs": [{"name": "bytes"}]}});
        refused(serde_json::json!([{"name": "transpose"}, {"name": "bytes"}]), "transposed");
        refused(serde_json::json!([shard(serde_json::json!([2, 4]), serde_json::json!([{"name": "bytes"}]))]), "don't tile");
        refused(
            serde_json::json!([shard(
                serde_json::json!([3, 2]),
                serde_json::json!([shard(serde_json::json!([3, 1]), serde_json::json!([]))])
            )]),
            "inside shards",
        );
        refused(
            serde_json::json!([shard(serde_json::json!([3, 2]), serde_json::json!([{"name": "bytes"}])), {"name": "zstd"}]),
            "after them",
        );
    }

    /// A shard of a (4, 6) int16 array in chunks of (2, 3), raw: chunk (0, 1) isn't held, and
    /// the index (little- or big-endian, with a checksum) is at the end or the start.
    fn shard_file(index_at_end: bool, index_little: bool) -> Vec<u8> {
        let value = |r: u64, c: u64| (r * 10 + c) as i16;
        let mut body = Vec::new();
        let mut index = Vec::new();
        let head = if index_at_end { 0 } else { 4 * 16 + 4 };
        for (gy, gx) in [(0u64, 0u64), (0, 1), (1, 0), (1, 1)] {
            if (gy, gx) == (0, 1) {
                index.extend([u64::MAX, u64::MAX]);
                continue;
            }
            let start = head + body.len() as u64;
            for r in 0..2 {
                for c in 0..3 {
                    body.extend(value(gy * 2 + r, gx * 3 + c).to_le_bytes());
                }
            }
            index.extend([start, head + body.len() as u64 - start]);
        }
        let mut idx: Vec<u8> = index.iter().flat_map(|v| if index_little { v.to_le_bytes() } else { v.to_be_bytes() }).collect();
        idx.extend([0u8; 4]);
        if index_at_end { [body, idx].concat() } else { [idx, body].concat() }
    }

    /// A store's own WKT is read as a `.prj` is: UTM zone 17N on NAD 27, bound to WGS 84 by the
    /// NTv2 grid `conus`, is read without the grid, which the raster says; WGS 84's UTM zone 17N,
    /// as WKT or by its code, says nothing (#1678).
    #[test]
    fn a_stores_wkt_says_a_grid_it_doesnt_apply() {
        use crate::raster::wkt::tests::{BOUND_BY_A_GRID, NAD27_UTM17};
        let said = "the file's NTv2 transformation to WGS 84 (conus) isn't applied, so positions may be off by as much as that shift";
        // The system alone says its datum has no shift, as from a `.prj`; WGS 84's says nothing.
        let datum = "the datum North American Datum 1927 has no conversion to WGS 84 in the file, so positions may be off by up to a few hundred metres";
        let utm17 = crate::raster::wkt::to_wkt(Crs::Epsg(32617)).unwrap();
        let coords = |dim: &str, n: usize| serde_json::json!({"zarr_format": 3, "node_type": "array", "shape": [n], "data_type": "float64", "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [n]}}, "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}], "fill_value": 0, "dimension_names": [dim]});
        let bytes = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let chunks = HashMap::from([
            ("elev/c/0/0".to_string(), [1i16, 2, 3, 4].iter().flat_map(|v| v.to_le_bytes()).collect()),
            ("y/c/0".to_string(), bytes(&[4_800_100.0, 4_800_000.0])),
            ("x/c/0".to_string(), bytes(&[500_000.0, 500_100.0])),
        ]);
        for (attrs, crs, lost) in [
            (serde_json::json!({"crs_wkt": BOUND_BY_A_GRID}), Crs::parse(NAD27_UTM17).unwrap(), vec![said]),
            (serde_json::json!({"spatial_ref": BOUND_BY_A_GRID}), Crs::parse(NAD27_UTM17).unwrap(), vec![said]),
            (serde_json::json!({"crs_wkt": NAD27_UTM17}), Crs::parse(NAD27_UTM17).unwrap(), vec![datum]),
            (serde_json::json!({"crs_wkt": utm17}), Crs::Epsg(32617), vec![]),
            (serde_json::json!({"epsg_code": 32617}), Crs::Epsg(32617), vec![]),
        ] {
            let elev = serde_json::json!({
                "zarr_format": 3, "node_type": "array", "shape": [2, 2], "data_type": "int16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 2]}},
                "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
                "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                "fill_value": 0, "dimension_names": ["y", "x"], "attributes": attrs
            });
            let root = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
                "elev": elev, "y": coords("y", 2), "x": coords("x", 2),
            }}});
            let store = parse_store(&[("zarr.json".into(), root.to_string().into_bytes())]).unwrap();
            let p = plan(&store, &Selection { variable: "elev".into(), index: vec![], window: None }).unwrap();
            let r = assemble(&store, &p, &chunks, "elev", None).unwrap();
            assert_eq!(r.crs, crs, "{attrs}");
            assert_eq!(r.lost, lost, "{attrs}");
            // The point series reads the same system, and the same note.
            assert_eq!(grid_crs(&store, "elev"), Some((crs, lost.first().map(|n| n.to_string()))), "{attrs}");
        }
    }

    /// A longitude/latitude store is read as such whatever its WKT says, and a shift the WKT gives
    /// that isn't applied is said as on a projected grid; a clean WKT says nothing.
    #[test]
    fn a_lonlat_stores_wkt_says_a_grid_it_doesnt_apply() {
        use crate::raster::wkt::tests::BOUND_BY_A_GRID;
        let said = "the file's NTv2 transformation to WGS 84 (conus) isn't applied, so positions may be off by as much as that shift";
        let coords = |dim: &str, n: usize| serde_json::json!({"zarr_format": 3, "node_type": "array", "shape": [n], "data_type": "float64", "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [n]}}, "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}], "fill_value": 0, "dimension_names": [dim]});
        let bytes = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let chunks = HashMap::from([
            ("elev/c/0/0".to_string(), [1i16, 2, 3, 4].iter().flat_map(|v| v.to_le_bytes()).collect()),
            ("lat/c/0".to_string(), bytes(&[43.1, 43.0])),
            ("lon/c/0".to_string(), bytes(&[-79.1, -79.0])),
        ]);
        for (attrs, lost) in
            [(serde_json::json!({"crs_wkt": BOUND_BY_A_GRID}), vec![said]), (serde_json::json!({"crs_wkt": "EPSG:4326"}), vec![])]
        {
            let elev = serde_json::json!({
                "zarr_format": 3, "node_type": "array", "shape": [2, 2], "data_type": "int16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 2]}},
                "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
                "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                "fill_value": 0, "dimension_names": ["lat", "lon"], "attributes": attrs
            });
            let root = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
                "elev": elev, "lat": coords("lat", 2), "lon": coords("lon", 2),
            }}});
            let store = parse_store(&[("zarr.json".into(), root.to_string().into_bytes())]).unwrap();
            let p = plan(&store, &Selection { variable: "elev".into(), index: vec![], window: None }).unwrap();
            let r = assemble(&store, &p, &chunks, "elev", None).unwrap();
            assert_eq!(r.crs, Crs::LonLat, "{attrs}");
            assert_eq!(r.lost, lost, "{attrs}");
        }
    }

    #[test]
    fn sharded_chunks() {
        for (at_end, little) in [(true, true), (false, true), (true, false)] {
            let z = serde_json::json!({
                "zarr_format": 3, "node_type": "array", "shape": [4, 6], "data_type": "int16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [4, 6]}},
                "codecs": [{"name": "sharding_indexed", "configuration": {
                    "chunk_shape": [2, 3],
                    "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
                    "index_codecs": [{"name": "bytes", "configuration": {"endian": if little { "little" } else { "big" }}}, {"name": "crc32c"}],
                    "index_location": if at_end { "end" } else { "start" }}}],
                "fill_value": -1, "dimension_names": ["y", "x"]
            });
            let s = parse_store(&[("zarr.json".into(), z.to_string().into_bytes())]).unwrap();
            let a = s.array("").unwrap();
            assert_eq!(a.unsupported(), None);
            assert_eq!(a.chunk_key(&[0, 0]), "c/0/0");
            let vals = a.decode(&shard_file(at_end, little)).unwrap();
            // Row-major over the shard; the chunk it doesn't hold is the fill value.
            let want: Vec<f64> =
                (0..4).flat_map(|r| (0..6).map(move |c| if r < 2 && c >= 3 { -1.0 } else { (r * 10 + c) as f64 })).collect();
            assert_eq!(vals, want, "index at end {at_end}, little {little}");
        }
    }

    /// Blosc's bit shuffle of `size`-byte elements (the first multiple of 8 of them).
    fn bitshuffle(raw: &[u8], size: usize) -> Vec<u8> {
        let n = raw.len() / size / 8 * 8;
        let mut o = raw.to_vec();
        o[..n * size].fill(0);
        for e in 0..n {
            for j in 0..size {
                for k in 0..8 {
                    let bit = (raw[e * size + j] >> k) & 1;
                    o[(j * 8 + k) * (n / 8) + e / 8] |= bit << (e % 8);
                }
            }
        }
        o
    }

    #[test]
    fn blosc_bit_shuffle() {
        // 1001 values: format 3 shuffles the first 1000 and leaves the last; format 2 shuffles a
        // block only when it is a whole number of 8 values, so this one is stored as it is.
        let raw: Vec<u8> = (0..1001u32).flat_map(|v| ((v * 37 % 1013) as f32 - 500.0).to_le_bytes()).collect();
        for (format, body) in [(3u8, bitshuffle(&raw, 4)), (2, raw.clone())] {
            let c = lz4_block(&body);
            let mut f = vec![format, 1, 0x4 | 0x10 | (1 << 5), 4];
            f.extend((raw.len() as u32).to_le_bytes());
            f.extend((raw.len() as u32).to_le_bytes());
            f.extend(((16 + 4 + 4 + c.len()) as u32).to_le_bytes());
            f.extend(20u32.to_le_bytes());
            f.extend((c.len() as i32).to_le_bytes());
            f.extend(c);
            assert_eq!(blosc(&f, raw.len()).unwrap(), raw, "format {format}");
        }
        assert_eq!(bitunshuffle(&bitshuffle(&raw[..64], 2), 2, 2), raw[..64]);
        // As c-blosc 1.21 writes it (numcodecs `Blosc(cname="zstd", clevel=9, shuffle=BITSHUFFLE)`
        // of `arange(3000, dtype="<i4") // 7`): one 12,000-byte block, its bits transposed whole
        // (bitshuffle's 8 KiB sub-blocks are its own framing, which blosc doesn't use).
        let hex = concat!(
            "02019404e02e0000e02e000078000000140000006000000028b52ffd60e02db502005242070ef01903d27a8f44ed7297",
            "b9b16c0af7f6f6feff6ded8b47e9848b920914a0c05e661df0300cb701ad016f12858b2928e35426fd34dd31af0e33dd",
            "19cfae1e4667aba7aa554766810ab06cab9a5bab6c93aa5b",
        );
        let real: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
        assert_eq!((real[0], real[2] & 0x4, u32::from_le_bytes(real[8..12].try_into().unwrap())), (2, 0x4, 12_000));
        let want: Vec<u8> = (0..3000i32).flat_map(|v| (v / 7).to_le_bytes()).collect();
        assert_eq!(blosc(&real, want.len()).unwrap(), want);
        assert_eq!(to_f16(1.0), 0x3c00);
        assert_eq!(f16(to_f16(-2.5)), -2.5);
        assert_eq!(DType::parse_v2(">i2").unwrap().encode(-2.0, false), vec![0xff, 0xfe]);
    }

    #[test]
    fn codecs_round_trip() {
        let raw: Vec<u8> = (0..4000u32).flat_map(|v| ((v * 7) as f32).to_le_bytes()).collect();
        assert_eq!(blosc(&blosc_frame(&raw, 4), raw.len()).unwrap(), raw);
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        assert_eq!(inflate(&z, true, raw.len()).unwrap(), raw);
        let mut gz = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3];
        gz.extend(b"name\0");
        gz.extend(miniz_oxide::deflate::compress_to_vec(&raw, 6));
        assert_eq!(gunzip(&gz, raw.len()).unwrap(), raw);
        let mut l = (raw.len() as u32).to_le_bytes().to_vec();
        l.extend(lz4_block(&raw));
        assert_eq!(numcodecs_lz4(&l, raw.len()).unwrap(), raw);
        assert_eq!(unshuffle(&[1, 3, 2, 4, 9], 2), vec![1, 2, 3, 4, 9]);
        assert_eq!(f16(0x3c00), 1.0);
        assert_eq!(DType::parse_v2(">i2").unwrap().value(&[0xff, 0xfe]), -2.0);
        assert_eq!(DType::parse_v3("uint16").unwrap().value(&[1, 1]), 257.0);
    }

    /// Stores zarr-python writes (`v2.zarr`: blosc LZ4 + shuffle, consolidated; `v3.zarr`: zstd),
    /// in the folder RUSTGIS_TEST_ZARR names; skipped without it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn stores_zarr_python_writes() {
        let Ok(dir) = std::env::var("RUSTGIS_TEST_ZARR") else { return };
        for store in ["v2.zarr", "v3.zarr"] {
            let path = std::path::Path::new(&dir).join(store);
            assert!(is_store(&path), "{store}");
            let r = read_dir(&path, "t2m[time=2]", None).unwrap();
            assert_eq!((r.width, r.height, r.crs), (360, 180, Crs::LonLat), "{store}");
            assert_eq!(r.transform, GeoTransform { origin_x: -180.0, origin_y: 90.0, pixel_width: 1.0, pixel_height: -1.0 }, "{store}");
            // The north-west corner (lon 0.5–19.5 at the top) is no data; it sits right of -180 after the turn.
            assert!(r.bands[0].data[180].is_nan() && !r.bands[0].data[0].is_nan(), "{store}");
            let s = &r.bands[0].stats;
            assert!(s.min > 230.0 && s.max < 320.0, "{store}: {s:?}");
            let store_m = read_metadata(&path).unwrap();
            let labels = info(&store_m, &HashMap::new());
            assert_eq!(labels["variables"][0]["name"], "t2m");
        }
    }

    #[test]
    fn store_paths_stay_inside_and_dates_stay_in_range() {
        // Keys that would lead out of the store are skipped, never read.
        let arr = serde_json::json!({"shape": [2, 2], "chunks": [2, 2], "dtype": "<f4", "compressor": null, "fill_value": null, "filters": null, "order": "C", "zarr_format": 2});
        for bad in ["../up/.zarray", "a/../../x/.zarray", "a\\b/.zarray", "C:x/.zarray", "a//b/.zarray", "./a/.zarray"] {
            let meta = serde_json::json!({"metadata": {bad: arr.clone()}});
            assert!(parse_store(&[(".zmetadata".into(), meta.to_string().into_bytes())]).is_err(), "{bad}");
        }
        let meta = serde_json::json!({"metadata": {"grp/v/.zarray": arr.clone()}});
        let st = parse_store(&[(".zmetadata".into(), meta.to_string().into_bytes())]).unwrap();
        assert_eq!(st.arrays[0].path, "grp/v");
        assert!(safe_path("") && safe_path("a/b") && !safe_path("/etc") && !safe_path("a/.."));
        // Hostile `since` dates: kept in range, no overflow (debug builds would panic).
        let mut t = st.arrays[0].clone();
        for units in
            ["days since 9223372036854775807-99-01", "days since -9223372036854775807-01-01", "seconds since 2000-01-01T99999999999:00"]
        {
            t.attrs.insert("units".into(), Value::String(units.into()));
            for cal in ["standard", "noleap", "360_day"] {
                t.attrs.insert("calendar".into(), Value::String(cal.into()));
                for v in [0.0, 9.9e12, -9.9e12, 1e300] {
                    let _ = label(&t, v);
                }
            }
        }
        assert!(fixed_calendar("noleap", "9223372036854775807-01-01", 1e300).is_some());
    }

    #[test]
    fn consolidated_null_is_not_consolidated() {
        let dir = std::env::temp_dir().join(format!("rustgis-zarr-null-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("v")).unwrap();
        // zarr-python 3 writes `"consolidated_metadata": null`: the arrays' own files are read.
        std::fs::write(dir.join("zarr.json"), r#"{"zarr_format": 3, "node_type": "group", "consolidated_metadata": null}"#).unwrap();
        std::fs::write(
            dir.join("v/zarr.json"),
            r#"{"zarr_format": 3, "node_type": "array", "shape": [2, 2], "data_type": "float32", "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 2]}}, "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}], "fill_value": 0, "dimension_names": ["y", "x"]}"#,
        )
        .unwrap();
        let st = read_metadata(&dir).unwrap();
        assert!(st.array("v").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hostile_input_never_panics() {
        let raw: Vec<u8> = (0..4000u32).flat_map(|v| (v as f32).to_le_bytes()).collect();
        let frame = blosc_frame(&raw, 4);
        for n in 0..frame.len() {
            let _ = blosc(&frame[..n], raw.len());
        }
        let mut lied = frame.clone();
        lied[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(blosc(&lied, raw.len()).is_err());
        for d in [&b""[..], b"\x1f\x8b\x08\xff", b"\x00\x00\x00\x10abc", b"\x28\xb5\x2f\xfd"] {
            let _ = gunzip(d, 100);
            let _ = numcodecs_lz4(d, 100);
            let _ = unzstd(d, 100);
        }
        // Shards cut anywhere, with an index lying about where its chunks are or how many there
        // are, fail without panicking or making room for what they claim.
        let shard = |inner: u64| {
            serde_json::json!({"zarr_format": 3, "node_type": "array", "shape": [4, 6], "data_type": "int16",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [4, 6]}},
            "codecs": [{"name": "sharding_indexed", "configuration": {"chunk_shape": [2, inner],
                "codecs": [{"name": "bytes"}, {"name": "zstd"}], "index_codecs": [{"name": "bytes"}, {"name": "crc32c"}]}}],
            "fill_value": 0})
        };
        let s = parse_store(&[("zarr.json".into(), shard(3).to_string().into_bytes())]).unwrap();
        let a = s.array("").unwrap();
        let file = shard_file(true, true);
        for n in 0..file.len() {
            let _ = a.decode(&file[..n]);
        }
        let mut lied = file.clone();
        let at = lied.len() - 68;
        lied[at..at + 8].copy_from_slice(&(u64::MAX - 3).to_le_bytes());
        assert!(a.decode(&lied).is_err());
        let s1 = parse_store(&[("zarr.json".into(), shard(1).to_string().into_bytes())]).unwrap();
        assert!(s1.array("").unwrap().decode(&[0u8; 40]).is_err());
        let huge = serde_json::json!({"metadata": {"a/.zarray": {"shape": [1u64 << 40, 1u64 << 40], "chunks": [1u64 << 30, 1u64 << 30], "dtype": "<f8", "compressor": null}}});
        let s = parse_store(&[(".zmetadata".into(), huge.to_string().into_bytes())]).unwrap();
        assert!(plan(&s, &Selection { variable: "a".into(), index: vec![], window: None }).is_err());
        assert!(parse_store(&[(".zmetadata".into(), b"{}".to_vec())]).is_err());
        assert!(parse_store(&[("zarr.json".into(), b"[1,2".to_vec())]).is_err());
        assert_eq!(
            Selection::parse("t2m[time=3, level=1]"),
            Selection { variable: "t2m".into(), index: vec![("time".into(), 3), ("level".into(), 1)], window: None }
        );
        assert_eq!(Selection::parse("t2m[time=3,level=1]").label(), "t2m[time=3,level=1]");
    }

    /// A Zarr 3 store of `v` (time 2, y 8, x 12; int16, value `t·1000 + row·100 + col`) in zstd
    /// chunks of 1 × 2 × 3, or (`sharded`) in shards of 1 × 4 × 6 holding them, the index at the end
    /// with a checksum. Step 0's chunk (rows 2–3, columns 3–5) and its rows 4–7 × columns 6–11 aren't
    /// written (a shard not holding a chunk, and a shard not stored). `y` is stored plain (cell
    /// centres from 75 down by 10), `x` with zstd (from 5 up by 10): its files, by key.
    #[cfg(not(target_arch = "wasm32"))]
    fn window_store(sharded: bool) -> Vec<(String, Vec<u8>)> {
        let coord = |n: u64, codecs: Value| {
            serde_json::json!({"zarr_format": 3, "node_type": "array", "shape": [n], "data_type": "float64",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [n]}}, "codecs": codecs, "fill_value": 0})
        };
        let inner = serde_json::json!([{"name": "bytes", "configuration": {"endian": "little"}}, {"name": "zstd"}]);
        let (chunk_shape, codecs) = if sharded {
            let index = serde_json::json!([{"name": "bytes", "configuration": {"endian": "little"}}, {"name": "crc32c"}]);
            let sharding = serde_json::json!({"name": "sharding_indexed", "configuration": {"chunk_shape": [1, 2, 3], "codecs": inner,
                "index_codecs": index, "index_location": "end"}});
            (serde_json::json!([1, 4, 6]), serde_json::json!([sharding]))
        } else {
            (serde_json::json!([1, 2, 3]), inner)
        };
        let v = serde_json::json!({"zarr_format": 3, "node_type": "array", "shape": [2, 8, 12], "data_type": "int16",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": chunk_shape}}, "codecs": codecs,
            "fill_value": -1, "dimension_names": ["time", "y", "x"], "attributes": {"epsg_code": 3857}});
        let root = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
            "v": v, "y": coord(8, serde_json::json!([{"name": "bytes", "configuration": {"endian": "little"}}])),
            "x": coord(12, serde_json::json!([{"name": "bytes", "configuration": {"endian": "little"}}, {"name": "zstd"}]))}}});
        let f64s = |v: Vec<f64>| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let mut files = vec![
            ("zarr.json".to_string(), root.to_string().into_bytes()),
            ("y/c/0".to_string(), f64s((0..8).map(|i| 75.0 - 10.0 * i as f64).collect())),
            ("x/c/0".to_string(), zstd::encode_all(&f64s((0..12).map(|i| 5.0 + 10.0 * i as f64).collect())[..], 3).unwrap()),
        ];
        // Chunk (t, gy, gx), zstd; None where it isn't written.
        let chunk = |t: u64, gy: u64, gx: u64| {
            if t == 0 && ((gy, gx) == (1, 1) || (gy >= 2 && gx >= 2)) {
                return None;
            }
            let raw: Vec<u8> = (0..2)
                .flat_map(|r| (0..3).map(move |c| (t * 1000 + (gy * 2 + r) * 100 + gx * 3 + c) as i16))
                .flat_map(i16::to_le_bytes)
                .collect();
            Some(zstd::encode_all(&raw[..], 3).unwrap())
        };
        for t in 0..2u64 {
            if !sharded {
                for (gy, gx) in (0..4u64).flat_map(|gy| (0..4u64).map(move |gx| (gy, gx))) {
                    files.extend(chunk(t, gy, gx).map(|b| (format!("v/c/{t}/{gy}/{gx}"), b)));
                }
                continue;
            }
            for (sy, sx) in [(0u64, 0u64), (0, 1), (1, 0), (1, 1)] {
                let (mut body, mut index) = (Vec::new(), Vec::new());
                for (iy, ix) in [(0u64, 0u64), (0, 1), (1, 0), (1, 1)] {
                    match chunk(t, sy * 2 + iy, sx * 2 + ix) {
                        Some(packed) => {
                            index.extend([body.len() as u64, packed.len() as u64]);
                            body.extend(packed);
                        }
                        None => index.extend([u64::MAX, u64::MAX]),
                    }
                }
                if body.is_empty() {
                    continue;
                }
                body.extend(index.iter().flat_map(|v| v.to_le_bytes()));
                body.extend([0u8; 4]);
                files.push((format!("v/c/{t}/{sy}/{sx}"), body));
            }
        }
        files
    }

    /// What `p` reads of `files`: its whole files by key, its parts by their ids (as a web store
    /// answers ranges).
    #[cfg(not(target_arch = "wasm32"))]
    fn read_plan(files: &[(String, Vec<u8>)], p: &Plan) -> HashMap<String, Vec<u8>> {
        let mut got: HashMap<String, Vec<u8>> = files.iter().filter(|(k, _)| p.keys.contains(k)).cloned().collect();
        for q in &p.parts {
            let Some((_, f)) = files.iter().find(|(k, _)| *k == q.key) else { continue };
            let bytes = match q.span {
                Span::Bytes(a, b) => f[a as usize..=b as usize].to_vec(),
                Span::Tail(n) => f[f.len() - n as usize..].to_vec(),
            };
            got.insert(q.id.clone(), bytes);
        }
        got
    }

    /// A window reads only the chunks it touches, a plain coordinate's own bytes and a compressed
    /// one's chunk; chunks never written are empty; the cells are those the whole slice holds there.
    /// A folder's window is the same, saved with its slice's label.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_window_reads_only_its_chunks() {
        let files = window_store(false);
        let store = parse_store(&files[..1]).unwrap();
        let sel = |t: u64, w: Window| Selection { variable: "v".into(), index: vec![("time".into(), t)], window: Some(w) };
        // Columns 2–5 and rows 1–4 of step 1: four chunks.
        let p = plan(&store, &sel(1, Window { x0: 2, y0: 1, x1: 5, y1: 4 })).unwrap();
        assert_eq!(p.keys, ["v/c/1/0/0", "v/c/1/0/1", "v/c/1/1/0", "v/c/1/1/1", "x/c/0"]);
        assert_eq!(p.parts, [Part { key: "y/c/0".into(), span: Span::Bytes(8, 31), id: "y/c/0@8-31".into() }]);
        let r = assemble(&store, &p, &read_plan(&files, &p), "v", None).unwrap();
        assert_eq!((r.width, r.height), (3, 3));
        let want: Vec<f32> = (1..4).flat_map(|r| (2..5).map(move |c| (1000 + r * 100 + c) as f32)).collect();
        assert_eq!(r.bands[0].data, want);
        assert_eq!(r.transform, GeoTransform { origin_x: 20.0, origin_y: 70.0, pixel_width: 10.0, pixel_height: -10.0 });
        // Step 0, rows 2–7 and columns 4–9, over chunks never written.
        let w0 = Window { x0: 4, y0: 2, x1: 10, y1: 8 };
        let p = plan(&store, &sel(0, w0)).unwrap();
        let r = assemble(&store, &p, &read_plan(&files, &p), "v", None).unwrap();
        let at = |r: usize, c: usize| r * 6 + c;
        assert!(r.bands[0].data[at(0, 0)].is_nan() && r.bands[0].data[at(1, 1)].is_nan());
        assert_eq!(r.bands[0].data[at(0, 2)], 206.0);
        assert!(r.bands[0].data[at(2, 2)].is_nan());
        assert_eq!(r.bands[0].data[at(2, 0)], 404.0);
        // The same window from a folder.
        // Removed however the test ends.
        struct Gone(std::path::PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!("rustgis-zarr-window-{}", std::process::id()));
        let _gone = Gone(dir.clone());
        for (k, b) in &files {
            let path = dir.join(k);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b).unwrap();
        }
        let label = sel(0, w0).label();
        assert_eq!(label, "v[time=0,cells=4:2:10:8]");
        assert_eq!(Selection::parse(&label), sel(0, w0));
        let from_dir = read_dir(&dir, &label, None).unwrap();
        assert_eq!(format!("{:?}", from_dir.bands[0].data), format!("{:?}", r.bands[0].data));
        // A window that isn't four whole numbers is refused, not read as the whole slice.
        // A dimension named `cells` keeps its step through a label.
        let named = Selection { variable: "v".into(), index: vec![("cells".into(), 1)], window: Some(w0) };
        assert_eq!(named.label(), "v[cells=1,cells=4:2:10:8]");
        assert_eq!(Selection::parse_strict(&named.label()).unwrap(), named);
        assert_eq!(Selection::parse_strict("v[cells=1]").unwrap().index, [("cells".to_string(), 1)]);
        for bad in ["v[time=0,cells=4:x:10:8]", "v[time=0,cells=4:2:10]", "v[cells=x]"] {
            assert!(Selection::parse_strict(bad).is_err(), "{bad}");
            assert!(read_dir(&dir, bad, None).unwrap_err().to_string().contains("aren't four whole numbers"), "{bad}");
        }
        // A box in the grid's coordinates is the cells it covers, cut to the grid (and said so).
        let mut coords = HashMap::new();
        for k in window_keys(&store, "v").unwrap() {
            coords.insert(k.clone(), files.iter().find(|(f, _)| *f == k).unwrap().1.clone());
        }
        assert_eq!(
            window_of(&store, "v", &coords, [21.0, 41.0, 49.0, 69.0]).unwrap(),
            (Window { x0: 2, y0: 1, x1: 5, y1: 4 }, false, false)
        );
        assert_eq!(
            window_of(&store, "v", &coords, [-50.0, 41.0, 13.0, 200.0]).unwrap(),
            (Window { x0: 0, y0: 0, x1: 2, y1: 4 }, true, false)
        );
        // An edge on a cell's edge but for rounding takes no cell past it.
        assert_eq!(
            window_of(&store, "v", &coords, [11.0, 41.0, (0.1 + 0.2) * 100.0, 69.0]).unwrap().0,
            Window { x0: 1, y0: 1, x1: 3, y1: 4 }
        );
        // Within one cell: two, so the cell size is known, and said.
        assert_eq!(
            window_of(&store, "v", &coords, [21.0, 41.0, 22.0, 42.0]).unwrap(),
            (Window { x0: 2, y0: 3, x1: 4, y1: 5 }, false, true)
        );
        assert!(window_of(&store, "v", &coords, [500.0, 41.0, 600.0, 69.0]).is_err());
        assert!(window_of(&store, "v", &coords, [49.0, 41.0, 21.0, 69.0]).is_err());
        // On a longitude grid of 0–360°, a box over its 0°/360° seam is refused (it is two runs of
        // columns); one on either side, written either way round, is read.
        let globe = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
            "v": {"zarr_format": 3, "node_type": "array", "shape": [2, 12], "data_type": "int16", "dimension_names": ["lat", "lon"],
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 12]}}, "codecs": [{"name": "bytes"}], "fill_value": 0},
            "lat": {"zarr_format": 3, "node_type": "array", "shape": [2], "data_type": "float64", "dimension_names": ["lat"],
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2]}}, "codecs": [{"name": "bytes"}], "fill_value": 0},
            "lon": {"zarr_format": 3, "node_type": "array", "shape": [12], "data_type": "float64", "dimension_names": ["lon"],
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [12]}}, "codecs": [{"name": "bytes"}], "fill_value": 0}}}});
        let globe = parse_store(&[("zarr.json".into(), globe.to_string().into_bytes())]).unwrap();
        let mut gc = HashMap::new();
        gc.insert("lat/c/0".to_string(), [15.0f64, -15.0].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>());
        gc.insert("lon/c/0".to_string(), (0..12).flat_map(|i| (15.0 + 30.0 * i as f64).to_le_bytes()).collect::<Vec<u8>>());
        for seam in [[-20.0, -10.0, 20.0, 10.0], [340.0, -10.0, 380.0, 10.0]] {
            let e = window_of(&globe, "v", &gc, seam).unwrap_err().to_string();
            assert!(e.contains("crosses the grid's edge at 360°"), "{seam:?}: {e}");
        }
        assert_eq!(
            window_of(&globe, "v", &gc, [-50.0, -10.0, -10.0, 10.0]).unwrap(),
            (Window { x0: 10, y0: 0, x1: 12, y1: 2 }, false, false)
        );
        assert_eq!(window_of(&globe, "v", &gc, [10.0, -10.0, 50.0, 10.0]).unwrap(), (Window { x0: 0, y0: 0, x1: 2, y1: 2 }, false, false));
        // A box round the whole globe, or holding the whole axis in one turn, is the whole axis.
        let all = (Window { x0: 0, y0: 0, x1: 12, y1: 2 }, true, false);
        assert_eq!(window_of(&globe, "v", &gc, [-200.0, -10.0, 200.0, 10.0]).unwrap(), all);
        assert_eq!(window_of(&globe, "v", &gc, [-10.0, -10.0, 370.0, 10.0]).unwrap(), all);
        // A window outside the grid, or under 2 × 2 cells, is refused; one wanting more chunks than
        // a read takes says to take a smaller one.
        for w in [Window { x0: 2, y0: 1, x1: 13, y1: 4 }, Window { x0: 2, y0: 1, x1: 3, y1: 4 }, Window { x0: 5, y0: 1, x1: 2, y1: 4 }] {
            assert!(plan(&store, &sel(0, w)).is_err(), "{w:?}");
        }
        let many = serde_json::json!({"zarr_format": 3, "node_type": "array", "shape": [100, 100], "data_type": "int16",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [1, 1]}}, "codecs": [{"name": "bytes"}], "fill_value": 0});
        let many = parse_store(&[("zarr.json".into(), many.to_string().into_bytes())]).unwrap();
        let e = plan(&many, &Selection { variable: "".into(), index: vec![], window: Some(Window { x0: 0, y0: 0, x1: 65, y1: 65 }) })
            .unwrap_err();
        assert!(e.to_string().contains("4225 chunks for the window, more than 4096: take a smaller window"), "{e}");
        // A coordinate chunk sized past what a byte offset holds (hostile metadata) neither panics
        // nor overflows: its window reads, or is refused.
        for (n, cs) in [(u64::MAX / 2 + 9, u64::MAX / 2 + 1), (u64::MAX / 2 + 9, u64::MAX / 2 + 9)] {
            let z = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
                "v": {"zarr_format": 3, "node_type": "array", "shape": [2, n], "data_type": "int16", "dimension_names": ["y", "x"],
                    "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [1, 1]}}, "codecs": [{"name": "bytes"}], "fill_value": 0},
                "x": {"zarr_format": 3, "node_type": "array", "shape": [n], "data_type": "float64", "dimension_names": ["x"],
                    "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [cs]}}, "codecs": [{"name": "bytes"}], "fill_value": 0}}}});
            let hostile = parse_store(&[("zarr.json".into(), z.to_string().into_bytes())]).unwrap();
            let w = Window { x0: n - 4, y0: 0, x1: n - 2, y1: 2 };
            let _ = plan(&hostile, &Selection { variable: "v".into(), index: vec![], window: Some(w) });
        }
        // A coordinate's bytes cut short is an error, not a misplaced raster.
        let p = plan(&store, &sel(1, Window { x0: 2, y0: 1, x1: 5, y1: 4 })).unwrap();
        let mut cut = read_plan(&files, &p);
        cut.insert("y/c/0@8-31".into(), vec![0u8; 16]);
        assert!(assemble(&store, &p, &cut, "v", None).is_err());
    }

    /// A window of a sharded variable reads each shard's index by its last bytes, then only the
    /// shard's chunks it needs, by range; a chunk the index says isn't there shows the fill value
    /// and a shard not stored is empty, as when the shards are read whole. An index cut short, or
    /// placing a chunk past what a read takes, is an error.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_window_reads_only_its_shards_chunks_by_range() {
        let files = window_store(true);
        let store = parse_store(&files[..1]).unwrap();
        let sel = |t: u64, w: Window| Selection { variable: "v".into(), index: vec![("time".into(), t)], window: Some(w) };
        let read = |p: &Plan| {
            let mut got = read_plan(&files, p);
            let inner = inner_parts(&store, p, &got).unwrap();
            let q = Plan { keys: Vec::new(), parts: inner.clone(), ..p.clone() };
            got.extend(read_plan(&files, &q));
            (inner, got)
        };
        // Columns 2–5 and rows 1–4 of step 1: shard (1, 0, 0)'s four chunks, by range.
        let p = plan(&store, &sel(1, Window { x0: 2, y0: 1, x1: 5, y1: 4 })).unwrap();
        assert_eq!(p.keys, ["x/c/0"]);
        let spans = |key: &str| p.parts.iter().filter(|q| q.key == key).map(|q| q.span).collect::<Vec<_>>();
        assert_eq!(spans("v/c/1/0/0"), [Span::Tail(4 * 16 + 4)]);
        assert_eq!(spans("y/c/0"), [Span::Bytes(8, 31)]);
        let (inner, got) = read(&p);
        assert_eq!(inner.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(), ["v/c/1/0/0#0", "v/c/1/0/0#1", "v/c/1/0/0#2", "v/c/1/0/0#3"]);
        let r = assemble(&store, &p, &got, "v", None).unwrap();
        assert_eq!(r.bands[0].data, (1..4).flat_map(|r| (2..5).map(move |c| (1000 + r * 100 + c) as f32)).collect::<Vec<_>>());
        // A chunk the index places but whose bytes didn't come (the shard gone since its index
        // was read) is an error, not the fill value.
        let mut gone = got.clone();
        gone.remove("v/c/1/0/0#0");
        assert!(assemble(&store, &p, &gone, "v", None).is_err());
        // Step 0, rows 2–7 and columns 4–9: the chunk shard (0, 0, 0) doesn't hold and shard
        // (0, 1, 1), not stored, are not asked for. The first shows the fill value and the second
        // is empty, as when the shards are read whole.
        let w0 = Window { x0: 4, y0: 2, x1: 10, y1: 8 };
        let p = plan(&store, &sel(0, w0)).unwrap();
        let (inner, got) = read(&p);
        assert!(inner.iter().all(|q| q.id != "v/c/0/0/0#3" && q.key != "v/c/0/1/1"), "{inner:?}");
        let r = assemble(&store, &p, &got, "v", None).unwrap();
        let at = |r: usize, c: usize| r * 6 + c;
        assert_eq!((r.bands[0].data[at(0, 0)], r.bands[0].data[at(1, 1)]), (-1.0, -1.0));
        assert!(r.bands[0].data[at(2, 2)].is_nan());
        assert_eq!((r.bands[0].data[at(0, 2)], r.bands[0].data[at(2, 0)]), (206.0, 404.0));
        let whole = Selection { window: None, ..sel(0, w0) };
        let pw = plan(&store, &whole).unwrap();
        let all = assemble(&store, &pw, &read_plan(&files, &pw), "v", None).unwrap();
        let cut: Vec<f32> =
            (2..8).flat_map(|row| (4..10).map(move |col| (row, col))).map(|(row, col)| all.bands[0].data[row * 12 + col]).collect();
        assert_eq!(format!("{cut:?}"), format!("{:?}", r.bands[0].data));
        // From a folder, by range: the same cells.
        let dir = std::env::temp_dir().join(format!("rustgis-zarr-shard-window-{}", std::process::id()));
        for (k, b) in &files {
            let path = dir.join(k);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b).unwrap();
        }
        let from_dir = read_dir(&dir, &sel(0, w0).label(), None).unwrap();
        assert_eq!(format!("{:?}", from_dir.bands[0].data), format!("{:?}", r.bands[0].data));
        // A shard that is there but can't be read (a folder in its place) is an error, not cells
        // left empty as if it were never written.
        let shard = dir.join("v/c/0/0/0");
        std::fs::remove_file(&shard).unwrap();
        std::fs::create_dir(&shard).unwrap();
        assert!(read_dir(&dir, &sel(0, w0).label(), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        // A window over more than MAX_CHUNKS of the shards' chunks reads its shards whole, as a
        // window did before, rather than being refused: 260 × 260 cells of 2 × 2 chunks in shards
        // of 130 × 130.
        let big = serde_json::json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
            "v": {"zarr_format": 3, "node_type": "array", "shape": [1, 260, 260], "data_type": "int16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [1, 130, 130]}},
                "codecs": [{"name": "sharding_indexed", "configuration": {"chunk_shape": [1, 2, 2], "codecs": [{"name": "bytes"}],
                    "index_codecs": [{"name": "bytes"}], "index_location": "end"}}],
                "fill_value": 0, "dimension_names": ["time", "y", "x"]}}}});
        let big = parse_store(&[("zarr.json".to_string(), big.to_string().into_bytes())]).unwrap();
        let p = plan(&big, &sel(0, Window { x0: 0, y0: 0, x1: 260, y1: 260 })).unwrap();
        assert!(p.shards.is_empty() && p.parts.is_empty(), "{:?}", p.parts);
        assert_eq!(p.keys, ["v/c/0/0/0", "v/c/0/0/1", "v/c/0/1/0", "v/c/0/1/1"]);
        // A small window of it still reads only its chunks.
        let p = plan(&big, &sel(0, Window { x0: 0, y0: 0, x1: 4, y1: 4 })).unwrap();
        assert_eq!((p.shards.len(), p.shards[0].chunks.len()), (1, 4));
        // An index cut short, or one placing a chunk past what a read takes.
        let p = plan(&store, &sel(1, Window { x0: 2, y0: 1, x1: 5, y1: 4 })).unwrap();
        let mut cut = HashMap::new();
        cut.insert("v/c/1/0/0#index".to_string(), vec![0u8; 20]);
        assert!(inner_parts(&store, &p, &cut).is_err());
        let mut lying = vec![0u8; 68];
        lying[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        cut.insert("v/c/1/0/0#index".to_string(), lying);
        assert!(inner_parts(&store, &p, &cut).is_err());
    }
}
