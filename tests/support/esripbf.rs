//! Shared, test-only encoder of ArcGIS protocol buffer answers (`f=pbf`), as
//! `FeatureCollection.proto` lays them out: for the reader's tests and for the services that
//! answer in protocol buffer pages.
#![cfg(test)]
#![allow(dead_code)]

/// The geometry type of a points layer (kept from the reader's own, which its tests see).
const POINT: u64 = 0;
/// The object id field type.
const OID: u64 = 6;

pub fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub fn number(field: u64, v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, field << 3);
    varint(&mut out, v);
    out
}

pub fn message(field: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (field << 3) | 2);
    varint(&mut out, bytes.len() as u64);
    out.extend_from_slice(bytes);
    out
}

pub fn double(field: u64, v: f64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (field << 3) | 1);
    out.extend_from_slice(&v.to_le_bytes());
    out
}

pub fn float(field: u64, v: f32) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (field << 3) | 5);
    out.extend_from_slice(&v.to_le_bytes());
    out
}

pub fn packed(values: impl IntoIterator<Item = u64>) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        varint(&mut out, v);
    }
    out
}

/// An answer: a version, then a query result holding `result`'s fields as its feature result.
pub fn answer(result: &[Vec<u8>]) -> Vec<u8> {
    [message(1, b"10.0"), message(2, &message(1, &result.concat()))].concat()
}

/// A spatial reference of longitude/latitude (WKID 4326), as services send with `outSR=4326`.
pub fn lonlat() -> Vec<u8> {
    message(8, &number(1, 4326))
}

pub fn field(name: &str, ty: u64) -> Vec<u8> {
    message(13, &[message(1, name.as_bytes()), number(2, ty)].concat())
}

/// A transform: lower-left or upper-left origin, X and Y scale and translate.
pub fn transform(upper_left: bool, scale: f64, x0: f64, y0: f64) -> Vec<u8> {
    message(
        12,
        &[
            number(1, if upper_left { 0 } else { 1 }),
            message(2, &[double(1, scale), double(2, scale)].concat()),
            message(3, &[double(1, x0), double(2, y0)].concat()),
        ]
        .concat(),
    )
}

pub fn text(s: &str) -> Vec<u8> {
    message(1, &message(1, s.as_bytes()))
}
pub fn sint64(v: i64) -> Vec<u8> {
    message(1, &number(8, zz(v)))
}
pub fn uint(v: u64) -> Vec<u8> {
    message(1, &number(5, v))
}
pub fn uint64(v: u64) -> Vec<u8> {
    message(1, &number(7, v))
}
pub fn single(v: f32) -> Vec<u8> {
    message(1, &float(2, v))
}
pub fn real(v: f64) -> Vec<u8> {
    message(1, &double(3, v))
}
pub fn null() -> Vec<u8> {
    message(1, &number(10, 1))
}

/// A geometry of parts `lengths` (none: one part) and coordinates, X and Y as differences.
pub fn geometry(lengths: &[u64], coords: &[i64]) -> Vec<u8> {
    let mut g = Vec::new();
    if !lengths.is_empty() {
        g.extend(message(2, &packed(lengths.iter().copied())));
    }
    g.extend(message(3, &packed(coords.iter().map(|&c| zz(c)))));
    message(2, &g)
}

pub fn feature(parts: &[Vec<u8>]) -> Vec<u8> {
    message(15, &parts.concat())
}

/// A page of a points layer, for paging tests: its object ids `ids` in a field named `oid`, each a
/// point at (id, 1), and whether the service says it holds more (`exceededTransferLimit`).
pub fn page(ids: &[u64], more: bool, oid: &str) -> Vec<u8> {
    let mut parts = vec![number(7, POINT), lonlat(), transform(false, 1.0, 0.0, 0.0), field(oid, OID)];
    if more {
        parts.push(number(9, 1));
    }
    parts.extend(ids.iter().map(|&id| feature(&[uint(id), geometry(&[], &[i64::try_from(id).unwrap_or(0), 1])])));
    answer(&parts)
}
