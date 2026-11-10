//! GeoPackage geometry blobs and column types, shared by the SQLite reader (desktop) and the
//! in-memory one (browser).

use crate::FieldType;
use rustgis_geom::Extent;

/// A GeoPackage geometry blob flagged empty: valid, with nothing to draw.
pub(crate) fn is_empty_blob(b: &[u8]) -> bool {
    b.starts_with(b"GP") && b.get(3).is_some_and(|f| f & 0b1_0000 != 0)
}

/// A blob flagged empty whose header is whole (version 0, the one GeoPackage defines; 8 bytes and
/// the envelope its flags give): a row the readers keep without geometry. Any other is unreadable,
/// not empty.
pub(crate) fn is_whole_empty_blob(b: &[u8]) -> bool {
    is_empty_blob(b) && b.get(2) == Some(&0) && b.get(3).and_then(|&f| envelope_len(f)).is_some_and(|n| b.len() >= 8 + n)
}

/// The length of the envelope a header's flags give; `None` for a code the standard doesn't define.
fn envelope_len(flags: u8) -> Option<usize> {
    match (flags >> 1) & 0b111 {
        0 => Some(0),
        1 => Some(32),
        2 | 3 => Some(48),
        4 => Some(64),
        _ => None,
    }
}

/// A GeoPackage geometry blob: the header's envelope (if any) and the WKB after it.
pub(crate) fn split_blob(b: &[u8]) -> Option<(Option<Extent>, &[u8])> {
    let (magic, rest) = b.split_at_checked(2)?;
    if magic != b"GP" {
        // Some writers store plain WKB.
        return Some((None, b));
    }
    let &flags = rest.get(1)?;
    let little = flags & 1 == 1;
    let env_len = envelope_len(flags)?;
    if flags & 0b1_0000 != 0 {
        // The empty flag.
        return None;
    }
    let env = b.get(8..8 + env_len)?;
    let f = |i: usize| -> Option<f64> {
        let a: [u8; 8] = env.get(i * 8..i * 8 + 8)?.try_into().ok()?;
        Some(if little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) })
    };
    // Envelope order: min x, max x, min y, max y.
    let envelope = if env_len >= 32 { Some(Extent::new(f(0)?, f(2)?, f(1)?, f(3)?)).filter(|e| !e.is_empty()) } else { None };
    Some((envelope, b.get(8 + env_len..)?))
}

/// The field type of a declared column type; `None` for a BLOB (not an attribute).
pub(crate) fn field_type(decl: &str) -> Option<FieldType> {
    let d = decl.to_ascii_uppercase();
    Some(if d.contains("BOOL") {
        FieldType::Boolean
    } else if d.contains("INT") {
        FieldType::Integer
    } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") || d.contains("NUMERIC") || d.contains("DECIMAL") {
        FieldType::Double
    } else if d.contains("BLOB") {
        return None;
    } else {
        FieldType::Text
    })
}
