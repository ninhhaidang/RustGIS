//! LiDAR point clouds: LAS 1.0–1.4 (point formats 0–10) and LAZ (COPC included), read as a
//! point layer with each point's elevation, intensity, classification, return number and colour.
//! Oversized clouds are sampled only for display; whole reads are refused.

use std::io::{Read, Seek, SeekFrom};

use crate::raster::Crs;
use crate::{DataError, Dataset, Feature, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};
use rustgis_geom::{Coord, Extent};

/// Most points read whole; larger files are sampled only for display.
pub const MAX_POINTS: usize = if cfg!(target_arch = "wasm32") { 500_000 } else { 2_000_000 };

/// Most points decompressed from a LAZ file (each is read to reach the next sampled one).
const MAX_DECOMPRESSED: u64 = if cfg!(target_arch = "wasm32") { 40_000_000 } else { 400_000_000 };

/// The smallest record of each point format.
const RECORD_LEN: [u16; 11] = [20, 28, 26, 34, 57, 63, 30, 36, 38, 59, 67];

/// Variable length records read for the coordinate system (and LAZ's), and the most bytes one
/// may take.
const MAX_VLRS: u32 = 1000;
const MAX_VLR_BYTES: u64 = 1 << 20;

/// The extensions read.
pub fn is_lidar_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".las") || l.ends_with(".laz")
}

/// Whether bytes start a LAS or LAZ file.
pub fn looks_like_las(bytes: &[u8]) -> bool {
    bytes.starts_with(b"LASF")
}

/// What a LAS header says.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub version: (u8, u8),
    pub format: u8,
    pub compressed: bool,
    pub record_len: u16,
    pub header_size: u16,
    pub point_offset: u32,
    pub count: u64,
    pub scale: [f64; 3],
    pub offset: [f64; 3],
    /// min x, min y, min z, max x, max y, max z.
    pub bounds: [f64; 6],
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).and_then(|s| s.try_into().ok()).map(u16::from_le_bytes)
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes)
}
fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    b.get(at..at + 4).and_then(|s| s.try_into().ok()).map(i32::from_le_bytes)
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes)
}
fn f64_at(b: &[u8], at: usize) -> Option<f64> {
    b.get(at..at + 8).and_then(|s| s.try_into().ok()).map(f64::from_le_bytes)
}

fn bad(what: &str) -> DataError {
    DataError::Format(format!("not a readable LAS file ({what})"))
}

/// Reads the header (the first 375 bytes at most).
pub fn header(b: &[u8]) -> Result<Header> {
    if !looks_like_las(b) {
        return Err(bad("no LASF signature"));
    }
    let version = (*b.get(24).ok_or_else(|| bad("version"))?, *b.get(25).ok_or_else(|| bad("version"))?);
    if version.0 != 1 || version.1 > 4 {
        return Err(DataError::Unsupported(format!("LAS {}.{}", version.0, version.1)));
    }
    let header_size = u16_at(b, 94).ok_or_else(|| bad("header size"))?;
    let point_offset = u32_at(b, 96).ok_or_else(|| bad("point offset"))?;
    let raw_format = *b.get(104).ok_or_else(|| bad("point format"))?;
    // LAZ marks its formats with the top bits.
    let compressed = raw_format & 0xC0 != 0;
    let format = raw_format & 0x3F;
    let min_len = *RECORD_LEN.get(usize::from(format)).ok_or_else(|| DataError::Unsupported(format!("LAS point format {format}")))?;
    let record_len = u16_at(b, 105).ok_or_else(|| bad("record length"))?;
    if record_len < min_len {
        return Err(bad("records shorter than their format"));
    }
    let legacy = u64::from(u32_at(b, 107).ok_or_else(|| bad("point count"))?);
    let count = if version.1 >= 4 && header_size >= 375 { u64_at(b, 247).filter(|n| *n > 0).unwrap_or(legacy) } else { legacy };
    let f = |at| f64_at(b, at).filter(|x| x.is_finite()).ok_or_else(|| bad("scale, offset or bounds"));
    let scale = [f(131)?, f(139)?, f(147)?];
    if scale.contains(&0.0) {
        return Err(bad("a zero scale"));
    }
    let offset = [f(155)?, f(163)?, f(171)?];
    // max x, min x, max y, min y, max z, min z.
    let bounds = [f(187)?, f(203)?, f(219)?, f(179)?, f(195)?, f(211)?];
    if u32::from(header_size) > point_offset || header_size < 227 {
        return Err(bad("header size"));
    }
    Ok(Header { version, format, compressed, record_len, header_size, point_offset, count, scale, offset, bounds })
}

/// A variable length record: its user id, record id and data.
struct Vlr {
    user: String,
    record: u16,
    data: Vec<u8>,
}

fn read_vlrs<R: Read + Seek>(r: &mut R, at: u64, n: u32, extended: bool, end: Option<u64>) -> Result<Vec<Vlr>> {
    let mut out = Vec::new();
    let mut pos = at;
    let head = if extended { 60 } else { 54 };
    for _ in 0..n.min(MAX_VLRS) {
        let mut h = vec![0u8; head];
        r.seek(SeekFrom::Start(pos))?;
        if r.read_exact(&mut h).is_err() {
            break;
        }
        let user = String::from_utf8_lossy(h.get(2..18).unwrap_or(&[])).trim_end_matches('\0').to_string();
        let record = u16_at(&h, 18).unwrap_or(0);
        let len = if extended { u64_at(&h, 20).unwrap_or(0) } else { u64::from(u16_at(&h, 20).unwrap_or(0)) };
        let data_at = pos + head as u64;
        if end.is_some_and(|e| data_at.saturating_add(len) > e) {
            break;
        }
        // Only the records RustGIS reads are kept, and only when small.
        // A WKT record is 2112 whoever wrote it (libLAS wrote its own user id).
        let wanted = record == 2112 || (user == "LASF_Projection" && record == 34735) || (user == "laszip encoded" && record == 22204);
        if wanted && len <= MAX_VLR_BYTES {
            let mut data = vec![0u8; len as usize];
            r.read_exact(&mut data)?;
            out.push(Vlr { user, record, data });
        }
        pos = data_at.saturating_add(len);
    }
    Ok(out)
}

/// The coordinate system the records say, with what its WKT's reading says
/// ([`crate::raster::wkt::read_wkt`]): WKT, else GeoTIFF keys' EPSG code.
fn crs_of(vlrs: &[Vlr]) -> Option<std::result::Result<(Crs, Option<String>), String>> {
    if let Some(v) = vlrs.iter().find(|v| v.record == 2112) {
        let wkt = String::from_utf8_lossy(&v.data).trim_end_matches('\0').trim().to_string();
        if !wkt.is_empty() {
            let text = horizontal(&wkt).trim();
            return Some(if text.contains('[') { crate::raster::wkt::read_wkt(text) } else { Crs::parse(text).map(|c| (c, None)) });
        }
    }
    let keys = vlrs.iter().find(|v| v.user == "LASF_Projection" && v.record == 34735)?;
    let n = usize::from(u16_at(&keys.data, 6)?).min(256);
    let mut code = None;
    for k in 0..n {
        let at = 8 + k * 8;
        // A cut-off entry ends the keys; a code found before it stands.
        let (Some(id), Some(loc), Some(val)) = (u16_at(&keys.data, at), u16_at(&keys.data, at + 2), u16_at(&keys.data, at + 6)) else {
            break;
        };
        // ProjectedCSTypeGeoKey first, else GeographicTypeGeoKey; 32767 is "user defined".
        if loc == 0 && val != 32767 && val != 0 && (id == 3072 || (id == 2048 && code.is_none())) {
            code = Some(val);
        }
    }
    let code = code?;
    Some(
        Crs::from_epsg(u32::from(code)).map(|c| (c, None)).ok_or_else(|| format!("EPSG:{code} isn't a coordinate system RustGIS converts")),
    )
}

/// The horizontal part of a compound WKT (a projected or geographic system with a vertical one,
/// as LiDAR files often give), or the WKT itself.
fn horizontal(wkt: &str) -> &str {
    let up = wkt.trim_start().to_ascii_uppercase();
    if !(up.starts_with("COMPD_CS") || up.starts_with("COMPOUNDCRS")) {
        return wkt;
    }
    let start = ["PROJCS[", "GEOGCS[", "PROJCRS[", "GEOGCRS[", "GEODCRS["]
        .iter()
        .filter_map(|k| up.find(k).map(|i| i + (wkt.len() - wkt.trim_start().len())))
        .min();
    let Some(start) = start else { return wkt };
    // To the bracket closing it, outside quoted names.
    let (mut depth, mut quoted) = (0usize, false);
    for (i, ch) in wkt.char_indices().skip_while(|(i, _)| *i < start) {
        match ch {
            '"' => quoted = !quoted,
            '[' if !quoted => depth += 1,
            ']' if !quoted => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return wkt.get(start..=i).unwrap_or(wkt);
                }
            }
            _ => {}
        }
    }
    wkt
}

/// One point's fields, from its record.
struct Point {
    x: f64,
    y: f64,
    z: f64,
    intensity: u16,
    class: u8,
    ret: u8,
    rgb: Option<[u16; 3]>,
}

fn point(h: &Header, rec: &[u8]) -> Option<Point> {
    let c = |i: usize| -> Option<f64> { Some(f64::from(i32_at(rec, i * 4)?) * h.scale[i] + h.offset[i]) };
    let (x, y, z) = (c(0)?, c(1)?, c(2)?);
    let intensity = u16_at(rec, 12)?;
    let extended = h.format >= 6;
    let (ret, class) = if extended { (rec.get(14)? & 0x0F, *rec.get(16)?) } else { (rec.get(14)? & 0x07, rec.get(15)? & 0x1F) };
    let rgb_at = match h.format {
        2 => Some(20),
        3 | 5 => Some(28),
        7 | 8 | 10 => Some(30),
        _ => None,
    };
    let rgb = match rgb_at {
        Some(at) => Some([u16_at(rec, at)?, u16_at(rec, at + 2)?, u16_at(rec, at + 4)?]),
        None => None,
    };
    Some(Point { x, y, z, intensity, class, ret, rgb })
}

/// Reads a LAS or LAZ file, its points kept as the file holds them, held in the coordinate system
/// it says (or `crs` gives) when RustGIS converts it ([`Dataset::held_in`]). Points with no place
/// in longitude/latitude are kept too, and said. Refuses files over [`MAX_POINTS`] or missing
/// records; points with a coordinate that isn't a number are counted skips.
pub fn read<R: Read + Seek + Send + Sync>(r: R, name: &str, crs: Option<Crs>) -> Result<(Dataset, ReadReport)> {
    let Cloud { ds, report, count, .. } = read_points(r, name, crs, false)?;
    let read = (ds.features.len() as u64).saturating_add(report.skipped as u64);
    if read < count {
        return Err(DataError::Format(format!(
            "{name}: could not read the whole cloud ({read} of {count} points; a damaged or cut-off file)"
        )));
    }
    Ok((ds, report))
}

/// A cloud [`read_points`] read.
pub(crate) struct Cloud {
    pub(crate) ds: Dataset,
    pub(crate) report: ReadReport,
    /// The header's point count.
    pub(crate) count: u64,
    /// For a display sample of points held in a system other than longitude/latitude: each
    /// point's place there, in the order of the features (NaN for one with none).
    pub(crate) places: Option<Vec<Coord>>,
}

/// Reads a display sample, retaining the header's original point count for whole-read checks.
pub(crate) fn read_sample<R: Read + Seek + Send + Sync>(r: R, name: &str) -> Result<Cloud> {
    read_points(r, name, None, true)
}

/// Shared decoder; only a display read may sample an oversized cloud.
fn read_points<R: Read + Seek + Send + Sync>(mut r: R, name: &str, crs: Option<Crs>, sample: bool) -> Result<Cloud> {
    let mut head = vec![0u8; 375];
    r.seek(SeekFrom::Start(0))?;
    let got = read_up_to(&mut r, &mut head)?;
    head.truncate(got);
    let h = header(&head)?;
    if !sample && h.count > MAX_POINTS as u64 {
        return Err(DataError::Format(format!("{name}: the file has {} points, more than the {MAX_POINTS} read at once", h.count)));
    }
    let mut report = ReadReport::default();
    let n_vlrs = u32_at(&head, 100).unwrap_or(0);
    let mut vlrs = read_vlrs(&mut r, u64::from(h.header_size), n_vlrs, false, Some(u64::from(h.point_offset)))?;
    // LAS 1.4's extended records after the points (a WKT may be there).
    if h.version.1 >= 4
        && let (Some(at), Some(n)) = (u64_at(&head, 235), u32_at(&head, 243))
        && at > 0
        && n > 0
    {
        vlrs.extend(read_vlrs(&mut r, at, n, true, None).unwrap_or_default());
    }
    let mut said = false;
    let crs = match crs {
        Some(c) => Some(c),
        None => match crs_of(&vlrs) {
            Some(Ok((c, note))) => {
                if let Some(note) = note {
                    report.messages.push(format!("{name}: the coordinate system: {note}"));
                }
                Some(c)
            }
            Some(Err(e)) => {
                report.messages.push(crate::loss::Note::loss(
                    crate::loss::LossCode::AssumedCrs,
                    format!("{name}: {e}; coordinates are kept as they are"),
                ));
                said = true;
                None
            }
            None => None,
        },
    };
    let lonlat_bounds = (-180.0..=180.0).contains(&h.bounds[0])
        && (-180.0..=180.0).contains(&h.bounds[3])
        && (-90.0..=90.0).contains(&h.bounds[1])
        && (-90.0..=90.0).contains(&h.bounds[4]);
    let transform = match crs {
        Some(Crs::LonLat) | None => None,
        Some(c) => match c.transformer() {
            Ok(t) => Some(t),
            Err(e) => {
                report.messages.push(crate::loss::Note::loss(
                    crate::loss::LossCode::AssumedCrs,
                    format!("{name}: the coordinate system: {e}; coordinates are kept as they are"),
                ));
                None
            }
        },
    };
    if crs.is_none() && !lonlat_bounds && !said {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::AssumedCrs,
            format!("{name}: the file doesn't say its coordinate system; coordinates are kept as they are"),
        ));
    }

    let has_rgb = matches!(h.format, 2 | 3 | 5 | 7 | 8 | 10);
    let mut fields = vec![
        Field::new("z", FieldType::Double),
        Field::new("intensity", FieldType::Integer),
        Field::new("classification", FieldType::Integer),
        Field::new("return", FieldType::Integer),
    ];
    if has_rgb {
        for c in ["red", "green", "blue"] {
            fields.push(Field::new(c, FieldType::Integer));
        }
    }
    let mut ds = Dataset::new(name, GeometryType::Point, fields);
    let total = h.count;
    if total == 0 {
        return Ok(Cloud { ds, report, count: total, places: None });
    }
    // A LAZ file is decompressed point by point: past MAX_DECOMPRESSED only its first points are
    // read (and sampled), with a notice.
    let readable = if h.compressed { total.min(MAX_DECOMPRESSED) } else { total };
    let stride = readable.div_ceil(MAX_POINTS as u64).max(1);
    let mut points: Vec<Point> = Vec::new();
    // The brightest colour value read (over every point decompressed, not only the sample).
    let mut brightest = 0u16;
    let rec_len = usize::from(h.record_len);
    if h.compressed {
        let vlr = vlrs
            .iter()
            .find(|v| v.user == "laszip encoded" && v.record == 22204)
            .ok_or_else(|| bad("a LAZ file without its laszip record"))?;
        if total > MAX_DECOMPRESSED {
            report
                .messages
                .push(format!("{name}: only its first {MAX_DECOMPRESSED} of {total} points are read (use a smaller extract for the rest)"));
        }
        let read = laz_points(&mut r, &h, &vlr.data, readable, stride, &mut points, &mut brightest)?;
        if read < readable {
            // A damaged or cut-off file: the points before the fault, or nothing to show.
            if read == 0 {
                return Err(bad(
                    "its compressed points couldn't be read (a damaged file, or one written with a LAZ variant RustGIS doesn't read)",
                ));
            }
            report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedFeature,
                format!("{name}: decompression stopped after {read} of {readable} points (a damaged or cut-off file)"),
            ));
        }
    } else {
        let mut rec = vec![0u8; rec_len];
        let mut i = 0u64;
        // Where the reader is, so records read one after the other aren't sought (a seek empties a
        // buffered reader).
        let mut pos = None;
        while i < total && points.len() < MAX_POINTS {
            let at = u64::from(h.point_offset).saturating_add(i.saturating_mul(rec_len as u64));
            if pos != Some(at) {
                r.seek(SeekFrom::Start(at))?;
            }
            pos = Some(at.saturating_add(rec_len as u64));
            if r.read_exact(&mut rec).is_err() {
                report.messages.push(crate::loss::Note::loss(
                    crate::loss::LossCode::DroppedFeature,
                    format!("{name}: the file ends before its last points"),
                ));
                break;
            }
            if let Some(p) = point(&h, &rec) {
                brightest = brightest.max(p.rgb.map_or(0, |c| c.into_iter().max().unwrap_or(0)));
                points.push(p);
            }
            i += stride;
        }
    }
    if stride > 1 {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedOverLimit,
            format!("{name}: {} of its {total} points are shown, evenly spread", points.len()),
        ));
    }
    // LAS colours are 16-bit by the standard, but some writers store 8-bit values: a file whose
    // colours never pass 255 (over every point read) is taken as 8-bit. A 16-bit file that dark
    // throughout can't be told apart, and shows dark either way.
    let wide = brightest > 255;
    // Kept as the file holds them, held in its system when RustGIS converts it (#2032 C2f-4b).
    // Each point is converted once to be checked: one with no place in longitude/latitude is
    // kept too and counted, and a sample keeps the places for its reads to test.
    let (mut not_numbers, mut unplaced, mut beyond) = (0usize, 0usize, 0usize);
    let mut places = (sample && transform.is_some()).then(|| Vec::with_capacity(points.len()));
    for (id, p) in points.into_iter().enumerate() {
        let c = Coord::new(p.x, p.y);
        if !c.is_finite() {
            not_numbers += 1;
            continue;
        }
        if let Some(t) = &transform {
            let place = t.to_lonlat(c).filter(|l| l.is_finite());
            match place {
                None => unplaced += 1,
                Some(l) if Extent::new(l.x, l.y, l.x, l.y).beyond_lonlat() => beyond += 1,
                Some(_) => {}
            }
            if let Some(places) = &mut places {
                places.push(place.unwrap_or(Coord::new(f64::NAN, f64::NAN)));
            }
        }
        let mut attrs =
            vec![Value::Float(p.z), Value::Int(i64::from(p.intensity)), Value::Int(i64::from(p.class)), Value::Int(i64::from(p.ret))];
        if has_rgb {
            let rgb = p.rgb.unwrap_or([0; 3]);
            for v in rgb {
                attrs.push(Value::Int(i64::from(if wide { v >> 8 } else { v })));
            }
        }
        ds.features.push(Feature { id: id as u64, geometry: Some(Geometry::Point(c)), attrs });
    }
    if not_numbers > 0 {
        report.skipped += not_numbers;
        let n = if not_numbers == 1 {
            "one point has a coordinate that isn't a number (NaN or infinite) and was left out".to_string()
        } else {
            format!("{not_numbers} points have a coordinate that isn't a number (NaN or infinite) and were left out")
        };
        report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedFeature, format!("{name}: {n}")));
    }
    if let (Some(c), Some(_)) = (crs, &transform) {
        ds.crs = Some(c);
        ds.held_in = c;
        let kept = ds.features.len();
        // A sample counts the points it shows, which the notice above says are a sample.
        let what = if stride > 1 { "points shown" } else { "points" };
        let notes = crate::io::kept_unplaced_note(what, unplaced, unplaced == kept, c).into_iter();
        report.messages.extend(notes.chain(crate::io::beyond_lonlat_note(what, beyond, c)).map(|m| format!("{name}: {m}")));
    }
    Ok(Cloud { ds, report, count: total, places })
}

fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(buf.get_mut(got..).unwrap_or_default())? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// Decompresses a LAZ file's first `count` points in order, keeping every `stride`-th; how many
/// were decompressed (fewer when the file is damaged or cut off).
fn laz_points<R: Read + Seek + Send + Sync>(
    r: &mut R,
    h: &Header,
    vlr: &[u8],
    count: u64,
    stride: u64,
    out: &mut Vec<Point>,
    brightest: &mut u16,
) -> Result<u64> {
    let vlr = laz::LazVlr::from_buffer(vlr).map_err(|e| DataError::Format(format!("LAZ: {e}")))?;
    r.seek(SeekFrom::Start(u64::from(h.point_offset)))?;
    let mut dec = laz::LasZipDecompressor::new(&mut *r, vlr).map_err(|e| DataError::Format(format!("LAZ: {e}")))?;
    let mut rec = vec![0u8; usize::from(h.record_len)];
    let mut read = 0u64;
    for i in 0..count {
        if dec.decompress_one(&mut rec).is_err() {
            // A cut-off file: the points read so far.
            break;
        }
        read += 1;
        let p = point(h, &rec);
        if let Some(c) = p.as_ref().and_then(|p| p.rgb) {
            *brightest = (*brightest).max(c.into_iter().max().unwrap_or(0));
        }
        if i % stride == 0
            && let Some(p) = p
        {
            out.push(p);
            if out.len() >= MAX_POINTS {
                // The sample is full: the rest of the points count as read.
                return Ok(count);
            }
        }
    }
    Ok(read)
}

/// Reads a LAS or LAZ file held in memory.
pub fn read_bytes(bytes: &[u8], name: &str, crs: Option<Crs>) -> Result<(Dataset, ReadReport)> {
    read(std::io::Cursor::new(bytes), name, crs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A LAS file built in code: `format`, the points (x, y, z, class) and an optional WKT.
    fn las(minor: u8, format: u8, points: &[(f64, f64, f64, u8)], wkt: Option<&str>) -> Vec<u8> {
        let header_size: u16 = if minor >= 4 { 375 } else { 227 };
        let rec_len = RECORD_LEN[usize::from(format)];
        let vlr = wkt.map(|w| {
            let mut v = vec![0u8; 54];
            v[2..17].copy_from_slice(b"LASF_Projection");
            v[18..20].copy_from_slice(&2112u16.to_le_bytes());
            v[20..22].copy_from_slice(&(w.len() as u16).to_le_bytes());
            v.extend_from_slice(w.as_bytes());
            v
        });
        let vlr_len = vlr.as_ref().map_or(0, Vec::len);
        let point_offset = u32::from(header_size) + vlr_len as u32;
        let mut b = vec![0u8; usize::from(header_size)];
        b[0..4].copy_from_slice(b"LASF");
        b[24] = 1;
        b[25] = minor;
        b[94..96].copy_from_slice(&header_size.to_le_bytes());
        b[96..100].copy_from_slice(&point_offset.to_le_bytes());
        b[100..104].copy_from_slice(&u32::from(vlr.is_some()).to_le_bytes());
        b[104] = format;
        b[105..107].copy_from_slice(&rec_len.to_le_bytes());
        b[107..111].copy_from_slice(&(points.len() as u32).to_le_bytes());
        for (i, s) in [0.01f64, 0.01, 0.01].iter().enumerate() {
            b[131 + i * 8..139 + i * 8].copy_from_slice(&s.to_le_bytes());
        }
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for p in points {
            for (k, v) in [p.0, p.1, p.2].iter().enumerate() {
                lo[k] = lo[k].min(*v);
                hi[k] = hi[k].max(*v);
            }
        }
        for k in 0..3 {
            b[179 + k * 16..187 + k * 16].copy_from_slice(&hi[k].to_le_bytes());
            b[187 + k * 16..195 + k * 16].copy_from_slice(&lo[k].to_le_bytes());
        }
        if minor >= 4 {
            b[247..255].copy_from_slice(&(points.len() as u64).to_le_bytes());
        }
        if let Some(v) = vlr {
            b.extend_from_slice(&v);
        }
        for p in points {
            let mut r = vec![0u8; usize::from(rec_len)];
            for (k, v) in [p.0, p.1, p.2].iter().enumerate() {
                r[k * 4..k * 4 + 4].copy_from_slice(&((v / 0.01).round() as i32).to_le_bytes());
            }
            r[12..14].copy_from_slice(&100u16.to_le_bytes());
            if format >= 6 {
                r[14] = 0x11;
                r[16] = p.3;
            } else {
                r[14] = 0x09;
                r[15] = p.3;
            }
            if format == 2 {
                r[20..26].copy_from_slice(&[0, 0xFF, 0, 0x80, 0, 0]);
            }
            b.extend_from_slice(&r);
        }
        b
    }

    #[test]
    fn colours_and_cut_off_geokeys() {
        // Colours that never pass 255 are 8-bit values, kept as they are.
        let mut b = las(2, 2, &[(10.0, 50.0, 1.0, 2), (10.5, 50.5, 2.0, 2)], None);
        for k in 0..2 {
            let at = 227 + k * usize::from(RECORD_LEN[2]) + 20;
            b[at..at + 6].copy_from_slice(&[200, 0, 100, 0, 50, 0]);
        }
        let (ds, _) = read_bytes(&b, "t", None).unwrap();
        assert_eq!(&ds.features[0].attrs[4..], &[Value::Int(200), Value::Int(100), Value::Int(50)]);
        // GeoTIFF keys claiming three entries but cut off after the first: its code stands.
        let mut data = Vec::new();
        for v in [1u16, 1, 0, 3, 3072, 0, 1, 32633] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.extend_from_slice(&2048u16.to_le_bytes());
        let keys = Vlr { user: "LASF_Projection".into(), record: 34735, data };
        assert!(matches!(crs_of(&[keys]), Some(Ok((c, None))) if c == Crs::from_epsg(32633).unwrap()));
    }

    #[test]
    fn reads_formats_and_versions() {
        let pts = [(10.0, 50.0, 300.5, 2), (10.5, 50.5, 310.25, 6)];
        for (minor, format) in [(0, 0), (2, 1), (2, 2), (3, 3), (4, 6), (4, 7), (4, 10)] {
            let (ds, report) = read_bytes(&las(minor, format, &pts, None), "t", None).unwrap();
            assert_eq!(ds.features.len(), 2, "1.{minor} format {format}");
            assert!(report.messages.is_empty(), "{:?}", report.messages);
            let f = &ds.features[1];
            assert_eq!(f.geometry, Some(Geometry::Point(Coord::new(10.5, 50.5))));
            assert_eq!(f.attrs[0], Value::Float(310.25));
            assert_eq!(f.attrs[2], Value::Int(6));
            assert_eq!(f.attrs[3], Value::Int(1));
            assert_eq!(ds.fields.len(), if matches!(format, 2 | 3 | 7 | 10) { 7 } else { 4 });
        }
        // 16-bit colours come out in 8 bits.
        let (ds, _) = read_bytes(&las(2, 2, &pts, None), "t", None).unwrap();
        assert_eq!(&ds.features[0].attrs[4..], &[Value::Int(255), Value::Int(128), Value::Int(0)]);
    }

    #[test]
    fn reprojects_and_samples() {
        // UTM 32N metres, with a WKT record.
        let wkt = r#"PROJCS["WGS 84 / UTM zone 32N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",9],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1],AUTHORITY["EPSG","32632"]]"#;
        let (ds, _) = read_bytes(&las(2, 0, &[(500000.0, 5_000_000.0, 1.0, 1)], Some(wkt)), "t", None).unwrap();
        // Kept as the file holds it, held in its system, and placed at 9°E through it.
        assert_eq!(ds.held_in, Crs::Epsg(32632));
        assert_eq!(ds.crs, Some(Crs::Epsg(32632)));
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(500000.0, 5_000_000.0))));
        let Geometry::Point(c) = ds.geometry_to_lonlat(ds.features[0].geometry.as_ref().unwrap()).unwrap().into_owned() else { panic!() };
        assert!((c.x - 9.0).abs() < 1e-6 && (c.y - 45.1535).abs() < 0.001, "{c:?}");
        // Projected coordinates with no system: kept, and said.
        let (ds, report) = read_bytes(&las(2, 0, &[(500000.0, 5_000_000.0, 1.0, 1)], None), "t", None).unwrap();
        assert_eq!(ds.features.len(), 1);
        assert!(report.messages.iter().any(|m| m.contains("doesn't say its coordinate system")));
        let (ds, report) = read_bytes(&las(2, 0, &[(500000.0, 5_000_000.0, 1.0, 1)], None), "t", Some(Crs::Epsg(32632))).unwrap();
        assert!(report.messages.is_empty());
        assert_eq!(ds.held_in, Crs::Epsg(32632));
        assert!(matches!(ds.features[0].geometry, Some(Geometry::Point(c)) if c.x == 500000.0));
    }

    /// What reading a WKT record finds is said: UTM zone 17N on NAD 27 bound to WGS 84 by an NTv2
    /// grid, as PROJ 9.8.1 writes it, is read without the grid, which is said. The WGS 84 zone of
    /// `reprojects_and_samples` says nothing (#1678).
    #[test]
    fn a_wkt_records_notes_are_said() {
        let bound = crate::raster::wkt::tests::BOUND_BY_A_GRID;
        let (ds, report) = read_bytes(&las(2, 0, &[(500000.0, 4_800_000.0, 1.0, 1)], Some(bound)), "t", None).unwrap();
        // On UTM zone 17N's central meridian, 81°W.
        let lonlat = ds.geometry_to_lonlat(ds.features[0].geometry.as_ref().unwrap()).unwrap().into_owned();
        assert!(matches!(lonlat, Geometry::Point(c) if (c.x + 81.0).abs() < 1e-3), "{lonlat:?}");
        let said = "t: the coordinate system: the file's NTv2 transformation to WGS 84 (conus) isn't applied, so positions may be off by as much as that shift";
        assert_eq!(report.messages, [said]);
        let utm32 = r#"PROJCS["WGS 84 / UTM zone 32N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",9],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1],AUTHORITY["EPSG","32632"]]"#;
        let (_, report) = read_bytes(&las(2, 0, &[(500000.0, 5_000_000.0, 1.0, 1)], Some(utm32)), "t", None).unwrap();
        assert!(report.messages.is_empty(), "{:?}", report.messages);
    }

    /// Oversized legacy and LAS 1.4 counts are refused before records or LAZ metadata are needed.
    #[test]
    fn oversized_whole_reads_are_refused() {
        let count = MAX_POINTS as u64 + 1;
        for minor in [2, 4] {
            let mut bytes = las(minor, 0, &[(1.0, 2.0, 3.0, 2)], None);
            bytes[107..111].copy_from_slice(&(count as u32).to_le_bytes());
            if minor == 4 {
                bytes[247..255].copy_from_slice(&count.to_le_bytes());
            }
            for compressed in [false, true] {
                bytes[104] = if compressed { 0x80 } else { 0 };
                let err = read_bytes(&bytes, "oversized", None).unwrap_err().to_string();
                assert!(err.contains(&count.to_string()) && err.contains(&MAX_POINTS.to_string()), "{err}");
            }
        }
    }

    #[test]
    fn sampled_source_keeps_original_count_and_refuses_materialization() {
        let count = MAX_POINTS as u64 + 1;
        let mut bytes = las(4, 0, &[(1.0, 2.0, 3.0, 2), (1.5, 2.5, 4.0, 2)], None);
        let clean = crate::source::open_bytes("clean.las", bytes.clone(), None).unwrap();
        let (ds, report) = crate::source::materialize(clean.as_ref()).unwrap();
        assert_eq!(ds.features.len(), 2);
        assert!(report.messages.is_empty());
        bytes[247..255].copy_from_slice(&count.to_le_bytes());
        let src = crate::source::open_bytes("oversized.las", bytes.clone(), None).unwrap();
        assert_eq!(src.info().count, Some(count));
        let batch = src.read(&crate::source::Request::all()).unwrap();
        assert!(!batch.complete);
        assert_eq!(batch.matched, count);
        assert!(!batch.features.is_empty());
        assert!(src.info().notes.iter().any(|m| m.contains("points") && m.contains(&count.to_string())));
        assert!(crate::source::materialize(src.as_ref()).is_err());
        assert!(crate::io::read_bytes("oversized.las", &bytes).is_err());
        assert!(crate::io::read_owned("oversized.las", bytes).is_err());
    }

    /// A point with a coordinate that isn't a number is a disclosed skip, read whole or through
    /// the source, while missing records still prevent a whole read.
    #[test]
    fn source_materializes_with_counted_losses() {
        let mut bytes = las(2, 0, &[(0.0, 0.0, 3.0, 2), (1.0, 0.0, 4.0, 2), (0.0, 0.0, 5.0, 2)], Some("EPSG:32632"));
        // A finite scale overflows only the second point's X during coordinate decoding: the one in
        // the middle.
        bytes[131..139].copy_from_slice(&f64::MAX.to_le_bytes());
        let (direct, report) = read_bytes(&bytes, "lossy", None).unwrap();
        assert_eq!(direct.features.len(), 2);
        assert!(!direct.crs_incomplete);
        assert_eq!(report.skipped, 1);
        assert_eq!(
            report.messages.texts().collect::<Vec<_>>(),
            ["lossy: one point has a coordinate that isn't a number (NaN or infinite) and was left out"]
        );
        let src = crate::source::open_bytes("lossy.las", bytes.clone(), None).unwrap();
        assert_eq!(src.info().count, Some(3));
        let (ds, report) = crate::source::materialize(src.as_ref()).unwrap();
        assert_eq!(ds.features, direct.features);
        assert!(!ds.crs_incomplete);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.messages.len(), 1, "{:?}", report.messages);
        // The sample's places stay in step with its points past the one left out: the point after
        // it is found by its own place.
        let around = Extent::new(4.0, -1.0, 5.0, 1.0);
        let batch = src.read(&crate::source::Request { extent: Some(around), limit: 10, own: None }).unwrap();
        assert_eq!(batch.features.iter().map(|f| f.id).collect::<Vec<_>>(), [0, 2]);
        bytes.truncate(bytes.len() - usize::from(RECORD_LEN[0]));
        assert!(crate::io::read_bytes("truncated.las", &bytes).is_err());
        let truncated = crate::source::open_bytes("truncated.las", bytes, None).unwrap();
        assert!(crate::source::materialize(truncated.as_ref()).is_err());
    }

    /// A point with no place in longitude/latitude is kept as the file holds it and said once,
    /// read whole or through the source (#2032 C2f-4b); nothing is lost, so Define Projection
    /// may give the cloud another system.
    #[test]
    fn a_point_with_no_place_is_kept_and_said() {
        let bytes = las(2, 0, &[(500_000.0, 5_000_000.0, 3.0, 2), (20_000_000.0, 0.0, 4.0, 2)], Some("EPSG:32632"));
        let direct = crate::io::read_bytes("far.las", &bytes).unwrap();
        let src = crate::source::open_bytes("far.las", bytes, None).unwrap();
        let materialized = crate::source::materialize(src.as_ref()).unwrap();
        let said = "one of the points has a place in EPSG:32632 that can't be given in longitude/latitude: it is kept as the file holds it, but doesn't draw";
        for (ds, report) in [direct, materialized] {
            assert_eq!(ds.features.len(), 2);
            assert_eq!(ds.held_in, Crs::Epsg(32632));
            assert_eq!(report.skipped, 0);
            assert!(!ds.crs_incomplete);
            assert_eq!(report.messages.iter().filter(|m| m.ends_with(said)).count(), 1, "{:?}", report.messages);
            assert!(crate::io::define_projection(&ds, Crs::WebMercator).is_ok());
        }
    }

    /// The source reads a box in longitude/latitude by the points' places there, which a point
    /// with none is outside of, and a box in the file's own system by the points as they are.
    #[test]
    fn a_point_with_no_place_is_outside_a_box_in_longitude_latitude() {
        let bytes = las(2, 0, &[(500_000.0, 5_000_000.0, 3.0, 2), (20_000_000.0, 0.0, 4.0, 2)], Some("EPSG:32632"));
        let src = crate::source::open_bytes("far.las", bytes, None).unwrap();
        let ids = |req: &crate::source::Request| src.read(req).unwrap().features.iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(&crate::source::Request::all()), [0, 1]);
        let around = Extent::new(8.0, 44.0, 10.0, 46.0);
        assert_eq!(ids(&crate::source::Request { extent: Some(around), limit: 10, own: None }), [0]);
        let world = Extent::new(-180.0, -90.0, 180.0, 90.0);
        assert_eq!(ids(&crate::source::Request { extent: Some(world), limit: 10, own: None }), [0]);
        let east = Extent::new(19_000_000.0, -1.0, 21_000_000.0, 1.0);
        let own = crate::source::Request { extent: Some(world), limit: 10, own: Some((Crs::Epsg(32632), east)) };
        assert_eq!(ids(&own), [1]);
        // The extent is the place that has one.
        let e = src.info().extent;
        assert!((e.min_x - 9.0).abs() < 1e-6 && (e.max_y - 45.1535).abs() < 0.001, "{e:?}");
    }

    /// A cloud none of whose points has a place in the system it says is said so, and Define
    /// Projection can give it the one they are in.
    #[test]
    fn a_cloud_with_no_place_can_be_given_its_system() {
        let bytes = las(2, 0, &[(20_000_000.0, 0.0, 3.0, 2), (19_000_000.0, 100.0, 4.0, 2)], Some("EPSG:32632"));
        let (ds, report) = crate::io::read_bytes("mercator.las", &bytes).unwrap();
        assert_eq!(ds.features.len(), 2);
        assert_eq!(report.skipped, 0);
        assert!(report.messages.iter().any(|m| m.contains("none of the points has a place")), "{:?}", report.messages);
        let fixed = crate::io::define_projection(&ds, Crs::WebMercator).unwrap();
        assert!(matches!(fixed.features[0].geometry, Some(Geometry::Point(c)) if c == Coord::new(20_000_000.0, 0.0)));
    }

    #[test]
    fn hostile_files_never_panic() {
        let b = las(4, 7, &[(1.0, 2.0, 3.0, 2), (1.5, 2.5, 3.5, 5), (1.2, 2.2, 3.2, 9)], Some("GEOGCS[\"x\"]"));
        for n in 0..b.len() {
            let _ = read_bytes(&b[..n], "t", None);
        }
        // Lies in the header: a huge count, a zero scale, a short record.
        let mut lie = b.clone();
        lie[247..255].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(read_bytes(&lie, "t", None).is_err());
        let shown = crate::source::open_bytes("lie.las", lie, None).unwrap();
        assert_eq!(shown.info().count, Some(u64::MAX));
        assert!(crate::source::materialize(shown.as_ref()).is_err());
        let mut zero = b.clone();
        zero[131..139].copy_from_slice(&0f64.to_le_bytes());
        assert!(read_bytes(&zero, "t", None).is_err());
        let mut short = b.clone();
        short[105..107].copy_from_slice(&10u16.to_le_bytes());
        assert!(read_bytes(&short, "t", None).is_err());
        assert!(read_bytes(b"LASF", "t", None).is_err() && read_bytes(b"", "t", None).is_err());
    }
}
