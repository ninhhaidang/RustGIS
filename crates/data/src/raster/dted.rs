//! DTED (Digital Terrain Elevation Data) levels 0, 1 and 2 (`.dt0`, `.dt1`, `.dt2`), read
//! natively on the desktop and in the browser (#2076).
//!
//! Every fact here is from the public performance specification MIL-PRF-89020B (NIMA, 23 May
//! 2000, Distribution Statement A), cited by paragraph; character positions in it count from 1,
//! the offsets here from 0.
//!
//! - A file (3.10.7.4) is a User Header Label (UHL, 80 bytes, from byte 1), a Data Set
//!   Identification record (DSI, 648 bytes, from byte 81), an Accuracy Description record (ACC,
//!   2700 bytes, from byte 729), then the data records from byte 3429. Each header starts with
//!   its name as a recognition sentinel (3.12 c, d, e).
//! - UHL (3.12 c): longitude of origin `DDDMMSSH` at character 5, latitude of origin `DDDMMSSH`
//!   at 13 (the lower-left, south-west, post of the data), longitude and latitude intervals in
//!   tenths of seconds at 21 and 25 (four digits each), the number of longitude lines at 48 and of
//!   latitude points at 52 (four digits each).
//! - DSI (3.12 d): the series designator `DTED0`–`DTED2` at character 60, the vertical datum
//!   (`MSL` or `E96`) at 142, the horizontal datum (`WGS84`) at 145, the latitude and longitude
//!   intervals in tenths of seconds at 274 and 278, and the numbers of latitude and longitude
//!   lines at 282 and 286.
//! - Data records (3.12 f, 3.10.5) run west to east, one longitude line each: a sentinel byte
//!   252 octal (0xAA), a 3-byte data block count, a 2-byte longitude count and a 2-byte latitude
//!   count, then one 2-byte height per latitude point from south to north, then a 4-byte checksum:
//!   12 + 2 × points bytes in all (2414 at level 1, 7214 at level 2).
//! - Numbers in a record are "fixed binary": signed-magnitude, big-endian (3.11.1, the note to
//!   3.12 f). The sign is the top bit and negatives are not complemented, so `0x8005` is −5.
//! - The checksum (3.10.6) is the sum of every byte of the record before it, each an unsigned
//!   8-bit value.
//! - A void is the null value, every bit set: −32767 in signed magnitude (3.10.9.1, 3.11.3.1).
//! - Heights are meters "as determined by the Earth Gravity Model (EGM) 1996" (3.12 f), on WGS
//!   84 horizontally.
//! - A cell is 1° × 1° between whole degrees (3.5.1) and adjacent cells share their edges
//!   (3.5.2); the latitude interval is the same everywhere, while the longitude interval widens
//!   with latitude in five zones (3.9.2, Tables I–III): from 0°, 50°, 70°, 75° and 80°, one, two,
//!   three, four and six times the latitude interval, which is 30″ at level 0, 3″ at level 1 and
//!   1″ at level 2.
//!
//! Posts lie on the grid lines (true longitude = count × interval + origin, 3.12 f), so as a
//! raster each post is a cell centered on it, and the cells reach half a cell beyond the cell's
//! whole-degree lines ([`super::post_transform`]), as an SRTM tile's do.
//!
//! A column whose record doesn't start with the sentinel is damaged and read as no data, and the
//! columns are counted in a notice. A record whose latitude count isn't 0 is refused, naming the
//! file and column: a full-grid record starts at latitude point 0 (3.12 f), and its heights would
//! otherwise be placed north or south of where they belong. A whole cell whose longitude counts
//! all number the lines from 1 rather than 0 is read by place, and the columns are counted in a
//! notice. Any other longitude count that disagrees with its place (leading columns left out,
//! counts offset, or a stray count) means the heights may belong to another column, and the file
//! is refused, naming the first such record ([`placement`]). A column whose checksum doesn't
//! match its heights is read as stored, and counted in a notice too: GDAL's DTED driver
//! documentation says some producers write wrong checksums, and that GDAL ignores them by default.
//! A complete cell cut short is read as far as it goes, the missing columns as no data, and said;
//! a partial cell cut short is refused, as it can't be told from the magnetic-tape layout.
//!
//! The same documentation says some producers write heights in two's complement rather than
//! signed magnitude. Signed magnitude reads such a negative height as one below −16000 m, which no
//! terrain has, so a height decoding below −16000 (other than the void, every bit set) is decoded
//! as two's complement instead, and counted in a notice ([`decode_height`]).

use super::{ColorHint, Crs, Raster, max_samples, post_transform, step_within, vertical_note, void_note};
use crate::{DataError, Result};

/// The headers' length: the UHL, DSI and ACC (3.10.7.4).
const HEADERS: usize = 80 + 648 + 2700;
/// Where the DSI record starts.
const DSI: usize = 80;
/// Where the ACC record starts.
const ACC: usize = 80 + 648;
/// The data record's sentinel, 252 octal (3.12 f).
const SENTINEL: u8 = 0o252;
/// The no-data value: a void (3.10.9.1).
pub const VOID: i16 = -32767;
/// Most posts along a side: 3601, level 2's latitude points (3.10.7.4.2).
pub const MAX_POSTS: usize = 3601;
/// The largest DTED file read: the headers and 3601 records of 3601 points, with room for bytes
/// after them (said, not read).
pub const MAX_BYTES: u64 = (HEADERS + MAX_POSTS * (12 + 2 * MAX_POSTS)) as u64 + (1 << 20);

/// The ASCII text of `bytes[at..at + len]`, if it is all there.
fn field(bytes: &[u8], at: usize, len: usize) -> Option<&str> {
    std::str::from_utf8(bytes.get(at..at.checked_add(len)?)?).ok()
}

/// A field of `len` digits as a number, without a sign or spaces.
fn digits(bytes: &[u8], at: usize, len: usize) -> Option<u32> {
    let s = field(bytes, at, len)?;
    if s.bytes().all(|b| b.is_ascii_digit()) { s.parse().ok() } else { None }
}

/// An angle written `DDDMMSSH` (3.12 c) as signed degrees: `H` is `N` or `E` for positive, `S`
/// or `W` for negative.
fn angle(bytes: &[u8], at: usize) -> Option<f64> {
    let (d, m, s) = (digits(bytes, at, 3)?, digits(bytes, at + 3, 2)?, digits(bytes, at + 5, 2)?);
    if m >= 60 || s >= 60 {
        return None;
    }
    let v = f64::from(d) + f64::from(m) / 60.0 + f64::from(s) / 3600.0;
    match bytes.get(at + 7)?.to_ascii_uppercase() {
        b'N' | b'E' => Some(v),
        b'S' | b'W' => Some(-v),
        _ => None,
    }
}

/// A big-endian signed-magnitude number of `bytes` (3.11.1): the top bit is the sign.
fn signed_magnitude(bytes: &[u8]) -> i64 {
    let raw = bytes.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b));
    let top = 1u64 << (bytes.len() * 8).saturating_sub(1).min(63);
    let magnitude = (raw & !top) as i64;
    if raw & top != 0 { -magnitude } else { magnitude }
}

/// A height as a record stores it (3.11.1): signed magnitude, with the void (every bit set) as
/// [`VOID`]. A value below −16000 m that isn't the void is taken for two's complement, the way some
/// producers write heights, and decoded so; the `bool` says when. Such a producer's void (−32767,
/// `0x8001` in two's complement) reads as −1 in signed magnitude, a plausible height: it can't be
/// told apart, and is read as −1.
fn decode_height(raw: [u8; 2]) -> (i16, bool) {
    let word = u16::from_be_bytes(raw);
    if word == 0xFFFF {
        return (VOID, false);
    }
    if twos_complement(word) {
        return (i16::from_be_bytes(raw), true);
    }
    let magnitude = (word & 0x7FFF) as i16;
    (if word & 0x8000 != 0 { -magnitude } else { magnitude }, false)
}

/// Whether [`decode_height`] takes a stored word for two's complement: negative in signed
/// magnitude, below −16000, and not the void. Counted for columns skipped at a step without
/// decoding them.
fn twos_complement(word: u16) -> bool {
    word != 0xFFFF && word & 0x8000 != 0 && word & 0x7FFF > 16000
}

/// The longitude interval MIL-PRF-89020B sets, in tenths of seconds, for DTED `level` (0–2) in
/// the cell whose south edge is `south` (3.9.2, Tables I–III): the latitude interval times one,
/// two, three, four or six, by the zone of the cell's edge nearer the equator.
pub fn spec_longitude_interval(level: u8, south: f64) -> Option<u32> {
    let base = spec_latitude_interval(level)?;
    let north = south + 1.0;
    let equatorward = if south < 0.0 && north > 0.0 { 0.0 } else { south.abs().min(north.abs()) };
    let times = match equatorward {
        e if e < 50.0 => 1,
        e if e < 70.0 => 2,
        e if e < 75.0 => 3,
        e if e < 80.0 => 4,
        _ => 6,
    };
    Some(base * times)
}

/// The latitude interval MIL-PRF-89020B sets, in tenths of seconds, for DTED `level`: 30″, 3″
/// and 1″ (Tables I–III).
pub fn spec_latitude_interval(level: u8) -> Option<u32> {
    match level {
        0 => Some(300),
        1 => Some(30),
        2 => Some(10),
        _ => None,
    }
}

/// Whether records can be read by their place, given each whole record's longitude count (`None`
/// for one without its sentinel) in a cell of `cols` longitude lines; `Err` gives the place and
/// count of the first record whose heights may belong to another column.
///
/// The counts number the lines from 0 at the origin (3.12 f). Read by place are only:
/// - every record counting its own place;
/// - a whole cell numbered from 1: every column's record there, each counting its place + 1, so the
///   last counts `cols`, which no line from 0 can.
///
/// Any other count may mean the heights belong to another column, as when leading columns are
/// left out (counts 100 to 500 in a cell of 3601 lines) or the counts are offset, even past the
/// cell's last line; read by place, those heights would land in the wrong columns, so the file is
/// refused.
fn placement(counts: &[Option<i64>], cols: usize) -> std::result::Result<(), (usize, i64)> {
    let numbered_from_one = counts.len() == cols && counts.iter().enumerate().all(|(i, c)| c.is_none_or(|c| c == i as i64 + 1));
    if numbered_from_one {
        return Ok(());
    }
    match counts.iter().enumerate().find_map(|(i, c)| c.filter(|c| *c != i as i64).map(|c| (i, c))) {
        Some(first) => Err(first),
        None => Ok(()),
    }
}

/// Reads a DTED file: `bytes` are the file, `file_name` its name (its extension gives the level
/// the spacing is checked against when the DSI doesn't name one).
///
/// One step that doesn't look at Cancel: the file is read in a single pass, a record at a time
/// ([`read_from`] streams a file the same way). On the desktop it runs in Add Data's read-ahead,
/// off the UI thread, and a Cancel takes effect when it returns. It stays on one thread, as
/// [`super::hgt::read`] does: a cell is at most a quarter of the largest SRTM tile's size.
///
/// Memory: the grid it gives, 4 bytes a height kept (`f32`), which at full resolution is about
/// twice the file (a record stores 2 bytes a height; a level 2 cell of 3601 × 3601: 26 MB on disk,
/// 52 MB as the grid), and at a step no more than [`max_samples`] heights; beside it the headers,
/// one record and one column (7 KB each at most) and each record's count (58 KB at most). Read
/// from `bytes` (an upload, the browser), the file is held too, about 3× the file in all;
/// [`read_from`], which the desktop reads a path with, holds about 2×.
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Raster> {
    read_limited(file_name, bytes, max_samples())
}

/// [`read`] from a reader: the headers, then one record at a time, so the file's bytes are never
/// held whole, only the heights kept. Every check is made as [`read`] makes it on the same bytes;
/// those that need the file's length are made once the records are read. Its peak is the grid
/// and one record: about twice the file at full resolution (see [`read`]).
pub fn read_from(file_name: &str, source: impl std::io::Read) -> Result<Raster> {
    read_from_limited(file_name, source, max_samples())
}

/// [`read_from`] for a file read by its path, `len` its size when it was checked: one larger than
/// [`MAX_BYTES`] is refused. It is read to its end, so bytes written after the check are counted
/// with any others past the last column and said, not left unread; they go to a sink, so memory
/// stays as [`read_from`]'s.
pub fn read_file(file_name: &str, len: u64, source: impl std::io::Read) -> Result<Raster> {
    if len > MAX_BYTES {
        return Err(DataError::Format("DTED: the file is larger than any DTED cell".into()));
    }
    read_from(file_name, source)
}

/// [`read`], holding at most `max` samples (the tests' small limit exercises the reduction).
fn read_limited(file_name: &str, bytes: &[u8], max: usize) -> Result<Raster> {
    read_from_limited(file_name, bytes, max)
}

/// [`read_from`], holding at most `max` samples.
fn read_from_limited(file_name: &str, mut source: impl std::io::Read, max: usize) -> Result<Raster> {
    use std::io::Read;
    let bad = |m: &str| DataError::Format(format!("DTED: {m}"));
    let mut head = Vec::with_capacity(HEADERS);
    source.by_ref().take(HEADERS as u64).read_to_end(&mut head)?;
    let bytes = head.as_slice();
    if bytes.len() < HEADERS {
        return Err(bad(if bytes.starts_with(b"UHL") { "the headers are cut short" } else { "not a DTED file (no UHL header)" }));
    }
    if !bytes.starts_with(b"UHL") {
        return Err(bad("not a DTED file (no UHL header)"));
    }
    if field(bytes, DSI, 3) != Some("DSI") || field(bytes, ACC, 3) != Some("ACC") {
        return Err(bad("the DSI and ACC records aren't where the UHL ends"));
    }
    // The UHL: origin, intervals and counts (3.12 c).
    let west =
        angle(bytes, 4).filter(|v| (-180.0..=180.0).contains(v)).ok_or_else(|| bad("the UHL's longitude of origin can't be read"))?;
    let south = angle(bytes, 12).filter(|v| (-90.0..=90.0).contains(v)).ok_or_else(|| bad("the UHL's latitude of origin can't be read"))?;
    let lon_tenths = digits(bytes, 20, 4).filter(|v| *v > 0).ok_or_else(|| bad("the UHL's longitude interval can't be read"))?;
    let lat_tenths = digits(bytes, 24, 4).filter(|v| *v > 0).ok_or_else(|| bad("the UHL's latitude interval can't be read"))?;
    let posts = |v: Option<u32>| v.map(|v| v as usize).filter(|v| (2..=MAX_POSTS).contains(v));
    let cols = posts(digits(bytes, 47, 4)).ok_or_else(|| bad("the UHL's number of longitude lines isn't 2 to 3601"))?;
    let rows = posts(digits(bytes, 51, 4)).ok_or_else(|| bad("the UHL's number of latitude points isn't 2 to 3601"))?;
    let (dx, dy) = (f64::from(lon_tenths) / 36_000.0, f64::from(lat_tenths) / 36_000.0);
    // A cell doesn't cross whole degrees (3.5.1): its posts span a degree at most.
    let (east, north) = (west + (cols - 1) as f64 * dx, south + (rows - 1) as f64 * dy);
    if east - west > 1.0 + 1e-9 || north - south > 1.0 + 1e-9 || north > 90.0 + 1e-9 || east > 180.0 + 1e-9 {
        return Err(bad("its posts span more than the one-degree cell a DTED file holds"));
    }
    let mut lost = Vec::new();
    // What the DSI says differently (3.12 d), and what the spec sets for the level (3.9.2).
    let dsi = [
        (digits(bytes, DSI + 273, 4), lat_tenths, "latitude interval"),
        (digits(bytes, DSI + 277, 4), lon_tenths, "longitude interval"),
        (digits(bytes, DSI + 281, 4), rows as u32, "number of latitude lines"),
        (digits(bytes, DSI + 285, 4), cols as u32, "number of longitude lines"),
    ];
    let differ: Vec<String> =
        dsi.iter().filter_map(|(d, u, what)| d.filter(|d| d != u).map(|d| format!("{what} {d} (the UHL's is {u})"))).collect();
    if !differ.is_empty() {
        lost.push(format!("its DSI record gives a different {}; the UHL's are used", differ.join(", ")));
    }
    // A partial cell (3.11.3) in the magnetic-tape layout: the UHL counts a full cell (3.12 c), but
    // the DSI counts only the lines that have heights, and the records leave missing posts out, so
    // they differ in length (3.11.3.2.2, 3.13.4 s, t). The CD-ROM layout always has the full grid
    // in both, and is the one read here (3.11.3.2.1), so its file holds every record whole. A tape
    // cell with a height on every line counts the whole cell in its DSI too, but leaving posts out
    // makes its file shorter than the full grid's: refused as well, said as cut short or tape.
    let (dsi_rows, dsi_cols) = (dsi[2].0.map(|n| n as usize), dsi[3].0.map(|n| n as usize));
    // The full grid's length is checked once the records are read.
    let partial = digits(bytes, DSI + 289, 2).filter(|p| (1..=99).contains(p));
    if let Some(percent) = partial
        && (dsi_cols.is_some_and(|n| n < cols) || dsi_rows.is_some_and(|n| n < rows))
    {
        return Err(bad(&format!(
            "a partial cell ({percent}% coverage) in the magnetic-tape layout, whose DSI counts only the {} of {cols} longitude lines and {} of {rows} latitude points with heights and whose records leave missing posts out; only the full-grid layout, as on CD-ROM, is read",
            dsi_cols.unwrap_or(cols),
            dsi_rows.unwrap_or(rows)
        )));
    }
    // The level: the DSI's series designator (3.12 d), or the extension when it names none.
    let ext = std::path::Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let named = match ext.as_str() {
        "dt0" => Some(0u8),
        "dt1" => Some(1),
        "dt2" => Some(2),
        _ => None,
    };
    let designated = match field(bytes, DSI + 59, 5) {
        Some("DTED0") => Some(0u8),
        Some("DTED1") => Some(1),
        Some("DTED2") => Some(2),
        _ => None,
    };
    if let (Some(d), Some(n)) = (designated, named)
        && d != n
    {
        lost.push(format!("its DSI says DTED level {d} but the file is named .{ext}; the DSI's level is used"));
    }
    let level = designated.or(named);
    if let Some(level) = level {
        let tenths = |t: u32| format!("{}″", f64::from(t) / 10.0);
        let want = (spec_latitude_interval(level), spec_longitude_interval(level, south.floor()));
        if want != (Some(lat_tenths), Some(lon_tenths))
            && let (Some(lat), Some(lon)) = want
        {
            lost.push(format!(
                "its spacing ({} of latitude by {} of longitude) isn't the {} by {} MIL-PRF-89020B sets for DTED level {level} at this latitude; the file's own spacing is used",
                tenths(lat_tenths),
                tenths(lon_tenths),
                tenths(lat),
                tenths(lon)
            ));
        }
    }
    match field(bytes, DSI + 144, 5) {
        Some("WGS84") => {}
        Some(other) if !other.trim().is_empty() => {
            lost.push(format!("its horizontal datum is {:?}, not WGS84; it is read as WGS 84", other.trim()));
        }
        _ => lost.push("it doesn't name its horizontal datum; it is read as WGS 84, as MIL-PRF-89020B sets".into()),
    }
    let surface = match field(bytes, DSI + 141, 3) {
        Some("E96") => "the EGM96 geoid (mean sea level)".to_string(),
        Some("MSL") => "mean sea level (its vertical datum MSL)".to_string(),
        Some(other) if !other.trim().is_empty() => format!("its vertical datum {:?}, taken for mean sea level", other.trim()),
        _ => "mean sea level (it doesn't name its vertical datum; MIL-PRF-89020B sets EGM96)".to_string(),
    };
    lost.push(vertical_note(&surface));

    // The data records, one a longitude line, west to east (3.10.5, 3.12 f), read one at a time;
    // the checks on them all are made once they are read, in the order they were made on the
    // whole file, so a stream gives the error the same bytes would.
    let record = 12 + 2 * rows;
    let step = step_within(cols, rows, max);
    let (w, h) = step.map_or((0, 0), |step| (cols.div_ceil(step), rows.div_ceil(step)));
    let mut data = super::filled(w, h, f32::from(VOID))?;
    let (mut damaged, mut misnumbered, mut mismatched, mut voids, mut twos) = (0usize, 0usize, 0usize, 0u64, 0u64);
    let mut column = Vec::with_capacity(rows);
    // A record's longitude count is its line's number from the origin (3.12 f), so the record at
    // place `i` should count `i`. Records are read by place, which is right only when the counts
    // can't mean the heights belong to another column (see [`placement`]).
    let mut lon_counts: Vec<Option<i64>> = Vec::with_capacity(cols);
    // A full-grid record's first height is latitude point 0 (3.12 f); the first counting another.
    let mut shifted = None;
    let (mut present, mut body_len) = (0usize, 0u64);
    let mut rec = Vec::with_capacity(record);
    for i in 0..cols {
        rec.clear();
        let n = source.by_ref().take(record as u64).read_to_end(&mut rec)?;
        body_len += n as u64;
        if n < record {
            break;
        }
        present += 1;
        let sentinel = rec.first() == Some(&SENTINEL);
        lon_counts.push(sentinel.then(|| signed_magnitude(rec.get(4..6).unwrap_or_default())));
        let lat_count = signed_magnitude(rec.get(6..8).unwrap_or_default());
        if shifted.is_none() && sentinel && lat_count != 0 {
            shifted = Some((i, lat_count));
        }
        // Too large a cell is refused below; its records are still read for the earlier checks.
        let Some(step) = step else { continue };
        let Some((head, (heights, sum))) =
            rec.split_at_checked(8).and_then(|(head, rest)| Some((head, rest.split_at_checked(rest.len().checked_sub(4)?)?)))
        else {
            damaged += 1;
            continue;
        };
        let lon_count = signed_magnitude(head.get(4..6).unwrap_or_default());
        if head.first() != Some(&SENTINEL) {
            damaged += 1;
            continue;
        }
        // A whole cell numbered from 1 is read by place, and said (any other longitude count that
        // isn't the record's place, and any latitude count other than 0, is refused below).
        if lon_count != i as i64 {
            misnumbered += 1;
        }
        // A wrong checksum is counted, and the heights read as stored (see the module's notes).
        let total: u64 = head.iter().chain(heights).map(|b| u64::from(*b)).sum();
        if signed_magnitude(sum) != total as i64 {
            mismatched += 1;
        }
        let raws = heights.as_chunks::<2>().0.iter().take(rows);
        if i % step != 0 {
            // A column skipped at a step: only counted, for the notices speak of the whole file.
            for word in raws.map(|raw| u16::from_be_bytes(*raw)) {
                voids += u64::from(word == 0xFFFF);
                twos += u64::from(twos_complement(word));
            }
            continue;
        }
        column.clear();
        for raw in raws {
            let (v, two) = decode_height(*raw);
            voids += u64::from(v == VOID);
            twos += u64::from(two);
            column.push(v);
        }
        // Rows run north to south: output row `r` is the latitude point `rows - 1 - r × step`.
        for r in 0..h {
            if let Some(cell) = data.get_mut(r * w + i / step) {
                *cell = f32::from(column.get(rows.saturating_sub(1 + r * step)).copied().unwrap_or(VOID));
            }
        }
    }
    // Bytes after the last column, counted and not kept.
    if present == cols {
        body_len += std::io::copy(&mut source, &mut std::io::sink())?;
    }
    if let Some(percent) = partial {
        let full = (HEADERS as u64).saturating_add((cols as u64).saturating_mul(record as u64));
        let len = (HEADERS as u64).saturating_add(body_len);
        if len < full {
            return Err(bad(&format!(
                "a partial cell ({percent}% coverage) whose file is {} bytes shorter than its full grid of {cols} × {rows} posts: cut short, or in the magnetic-tape layout, which leaves missing posts out. A partial cell is read only whole, in the full-grid layout as on CD-ROM, since the two can't be told apart",
                full - len
            )));
        }
    }
    if present == 0 {
        return Err(bad("the file is cut short: it has no whole data record"));
    }
    placement(&lon_counts, cols).map_err(|(place, count)| {
        bad(&format!(
            "{file_name}: the record at column {place} counts itself as column {count}, not {place}: its heights may belong to another column (columns left out, or counts offset), so the file is refused rather than read with heights in the wrong columns. Only a whole cell numbered from 1 is read by place"
        ))
    })?;
    // One counting another latitude point would place every height in its column north or south
    // of where it belongs, so it is refused.
    if let Some((place, count)) = shifted {
        return Err(bad(&format!(
            "{file_name}: the record at column {place} counts its first height as latitude point {count}, not 0, where every full-grid record starts: its heights would be placed {count} points from where they belong, so the file is refused"
        )));
    }
    let step = step.ok_or_else(|| bad("the cell is too large to open"))?;
    if damaged == present {
        return Err(bad("no data record is whole: none starts with the 0xAA sentinel"));
    }
    let count = |n: usize| if n == 1 { format!("1 of its {cols} columns has") } else { format!("{n} of its {cols} columns have") };
    if present < cols {
        lost.push(format!("the file is cut short: {} no heights, read as no data", count(cols - present)));
    } else if let Some(extra) = body_len.checked_sub((cols * record) as u64).filter(|n| *n > 0) {
        lost.push(format!("{extra} bytes after its last column are ignored"));
    }
    if damaged > 0 {
        lost.push(format!("{} a damaged record header (not the 0xAA sentinel), read as no data", count(damaged)));
    }
    match misnumbered {
        0 => {}
        1 => lost.push(format!("1 of its {cols} columns' longitude count numbers it from 1, not 0; it is read by its place in the file")),
        n => lost.push(format!(
            "{n} of its {cols} columns' longitude counts number them from 1, not 0; they are read by their place in the file"
        )),
    }
    match mismatched {
        0 => {}
        1 => lost.push(format!(
            "1 of its {cols} columns' checksum doesn't match its heights (some producers write them wrongly); its heights are read as stored"
        )),
        n => lost.push(format!(
            "{n} of its {cols} columns' checksums don't match their heights (some producers write them wrongly); their heights are read as stored"
        )),
    }
    match twos {
        0 => {}
        1 => lost.push(
            "1 of its heights was written in two's complement, not the signed magnitude MIL-PRF-89020B sets; it is read as two's complement"
                .into(),
        ),
        n => lost.push(format!(
            "{n} of its heights were written in two's complement, not the signed magnitude MIL-PRF-89020B sets; they are read as two's complement"
        )),
    }
    lost.extend(void_note(voids, (rows * cols) as u64, VOID));
    let name = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("dted").to_string();
    let transform = post_transform(west, south, rows, dx, dy, step);
    let mut r = Raster::new(name, w as u32, h as u32, vec![data], transform, Crs::LonLat, Some(f64::from(VOID)), ColorHint::Gray, "Int16")?;
    if step > 1 {
        // The file's own size, as the layer says it; each cell read stands for exactly `step`
        // posts across and down (`step`, which `Raster::file_pixels` and the warp's scale take
        // rather than this over the size read, since `step` needn't divide the posts).
        r.overview = Some((cols as u32, rows as u32));
        r.step = step as u32;
        r.posts = true;
    }
    r.lost = lost;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A height in signed magnitude (3.11.1); `None` is a void (every bit set).
    fn sm(v: Option<i16>) -> [u8; 2] {
        match v {
            None => [0xff, 0xff],
            Some(v) if v < 0 => (0x8000 | v.unsigned_abs()).to_be_bytes(),
            Some(v) => (v as u16).to_be_bytes(),
        }
    }

    /// A DTED file per MIL-PRF-89020B: origin `lon`/`lat` (`DDDMMSSH`), intervals in tenths of
    /// seconds, `cols` × `rows` posts, the height of column `i` (west to east), point `j` (south
    /// to north) `f(i, j)`.
    fn dted(
        lon: &str,
        lat: &str,
        (lon_t, lat_t): (u32, u32),
        (cols, rows): (usize, usize),
        f: impl Fn(usize, usize) -> Option<i16>,
    ) -> Vec<u8> {
        let uhl = format!("UHL1{lon}{lat}{lon_t:04}{lat_t:04}NA  U  {:12}{cols:04}{rows:04}0{:24}", "", "");
        assert_eq!(uhl.len(), 80);
        let mut dsi = vec![b' '; 648];
        let mut put = |at: usize, s: &str| dsi[at - 1..at - 1 + s.len()].copy_from_slice(s.as_bytes());
        put(1, "DSI");
        put(4, "U");
        put(60, "DTED1");
        put(142, "E96");
        put(145, "WGS84");
        put(274, &format!("{lat_t:04}"));
        put(278, &format!("{lon_t:04}"));
        put(282, &format!("{rows:04}"));
        put(286, &format!("{cols:04}"));
        put(290, "00");
        let mut acc = vec![b' '; 2700];
        acc[..3].copy_from_slice(b"ACC");
        let mut b = uhl.into_bytes();
        b.extend(dsi);
        b.extend(acc);
        assert_eq!(b.len(), 3428);
        for i in 0..cols {
            let mut rec = vec![SENTINEL, 0, (i >> 8) as u8, i as u8];
            rec.extend((i as u16).to_be_bytes());
            rec.extend([0, 0]);
            for j in 0..rows {
                rec.extend(sm(f(i, j)));
            }
            let sum: u32 = rec.iter().map(|b| u32::from(*b)).sum();
            rec.extend(sum.to_be_bytes());
            b.extend(rec);
        }
        b
    }

    /// The offset of column `i`'s record in a file of `rows` points a column.
    fn at(i: usize, rows: usize) -> usize {
        HEADERS + i * (12 + 2 * rows)
    }

    #[test]
    fn signed_magnitude_is_not_twos_complement() {
        assert_eq!(signed_magnitude(&[0x80, 0x05]), -5);
        assert_eq!(signed_magnitude(&[0x00, 0x05]), 5);
        assert_eq!(signed_magnitude(&[0xff, 0xff]), -32767);
        assert_eq!(signed_magnitude(&[0x80, 0x00]), 0);
        assert_eq!(signed_magnitude(&[0x00, 0x01, 0x02, 0x03]), 0x010203);
        assert_eq!(signed_magnitude(&[]), 0);
    }

    #[test]
    fn heights_below_minus_16000_are_read_as_twos_complement() {
        assert_eq!(decode_height([0x80, 0x05]), (-5, false));
        assert_eq!(decode_height([0x00, 0x05]), (5, false));
        assert_eq!(decode_height([0xff, 0xff]), (VOID, false), "the void, not two's complement −1");
        assert_eq!(decode_height([0xbe, 0x80]), (-16000, false), "−16000 itself is signed magnitude");
        assert_eq!(decode_height([0xff, 0xfb]), (-5, true));
        assert_eq!(decode_height([0xc1, 0x80]), (-16000, true));
        // A two's-complement producer's void (0x8001) reads as −1: it can't be told apart.
        assert_eq!(decode_height([0x80, 0x01]), (-1, false));
    }

    #[test]
    fn a_twos_complement_height_is_read_and_said() {
        let rows = 11;
        let mut b = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| Some((i + j) as i16));
        // Column 4, point 2 holds −5 as two's complement (0xFFFB); its checksum is made right.
        let rec = at(4, rows);
        b[rec + 8 + 2 * 2..rec + 8 + 2 * 2 + 2].copy_from_slice(&(-5i16).to_be_bytes());
        let sum: u32 = b[rec..rec + 8 + 2 * rows].iter().map(|v| u32::from(*v)).sum();
        b[rec + 8 + 2 * rows..rec + 12 + 2 * rows].copy_from_slice(&sum.to_be_bytes());
        let r = read("x.dt1", &b).unwrap();
        assert_eq!(r.value(0, 4, 10 - 2), Some(-5.0));
        assert!(
            r.lost.iter().any(|l| l
                == "1 of its heights was written in two's complement, not the signed magnitude MIL-PRF-89020B sets; it is read as two's complement"),
            "{:?}",
            r.lost
        );
        assert!(!r.lost.iter().any(|l| l.contains("checksum")), "{:?}", r.lost);
    }

    #[test]
    fn the_dsi_names_the_level() {
        // 11 × 11 posts of 3″ is no level's spacing: the notice names the level checked against.
        let b = dted("0000000E", "0000000N", (30, 30), (11, 11), |_, _| Some(1));
        let r = read("x.dt2", &b).unwrap();
        assert!(
            r.lost.iter().any(|l| l == "its DSI says DTED level 1 but the file is named .dt2; the DSI's level is used"),
            "{:?}",
            r.lost
        );
        assert!(!r.lost.iter().any(|l| l.contains("isn't the")), "level 1's spacing is 3″ by 3″: {:?}", r.lost);
        // Agreeing, nothing is said; without a designator the extension gives the level.
        assert!(!read("x.dt1", &b).unwrap().lost.iter().any(|l| l.contains("DSI says")));
        let mut blank = b.clone();
        blank[DSI + 59..DSI + 64].copy_from_slice(b"     ");
        let r = read("x.dt2", &blank).unwrap();
        assert!(!r.lost.iter().any(|l| l.contains("DSI says")), "{:?}", r.lost);
        assert!(r.lost.iter().any(|l| l.contains("isn't the 1″ by 1″") && l.contains("level 2")), "{:?}", r.lost);
    }

    #[test]
    fn the_spec_zones_set_the_longitude_interval() {
        // Tables I–III, north and south alike.
        for (level, lat, want) in [
            (1, 37.0, 30),
            (1, 49.0, 30),
            (1, 50.0, 60),
            (1, -51.0, 60),
            (1, -50.0, 30),
            (1, 70.0, 90),
            (1, 75.0, 120),
            (1, 80.0, 180),
            (1, -90.0, 180),
            (0, 0.0, 300),
            (0, 60.0, 600),
            (2, 0.0, 10),
            (2, 72.0, 30),
            (2, 85.0, 60),
        ] {
            assert_eq!(spec_longitude_interval(level, lat), Some(want), "level {level} at {lat}");
        }
        assert_eq!(spec_longitude_interval(3, 0.0), None);
    }

    #[test]
    fn reads_a_cell_in_place() {
        // 11 × 11 posts of 360″ (0.1°), from 37°N, 122°W, as a level 1 file would have 1201.
        let f = |i: usize, j: usize| match (i, j) {
            (0, 0) => Some(-412),
            (3, 2) => None,
            (10, 10) => Some(4421),
            _ => Some((i * 100 + j) as i16 - 500),
        };
        let bytes = dted("1220000W", "0370000N", (3600, 3600), (11, 11), f);
        let r = read("cell.dt1", &bytes).unwrap();
        assert_eq!((r.width, r.height, r.crs, r.nodata, r.data_type.as_str()), (11, 11, Crs::LonLat, Some(-32767.0), "Int16"));
        let t = r.transform;
        assert!((t.origin_x - (-122.05)).abs() < 1e-12 && (t.origin_y - 38.05).abs() < 1e-12, "{t:?}");
        assert!((t.pixel_width - 0.1).abs() < 1e-12 && (t.pixel_height + 0.1).abs() < 1e-12);
        // Column 0, point 0 is the south-west post: the bottom-left cell.
        assert_eq!(r.value(0, 0, 10), Some(-412.0), "a negative height in signed magnitude");
        assert_eq!(r.value(0, 10, 0), Some(4421.0), "the north-east post");
        assert_eq!(r.value(0, 3, 8), None, "a void");
        assert_eq!(r.value(0, 4, 5), Some(-95.0));
        let sw = t.to_world(0.5, 10.5);
        assert!((sw.x + 122.0).abs() < 1e-9 && (sw.y - 37.0).abs() < 1e-9);
        // Its spacing isn't level 1's, the vertical datum and the one void are said.
        assert!(r.lost.iter().any(|l| l.contains("isn't the 3″ by 3″")), "{:?}", r.lost);
        assert!(r.lost.iter().any(|l| l.contains("EGM96 geoid")), "{:?}", r.lost);
        assert!(r.lost.iter().any(|l| l == "1 of its 121 heights is a void (-32767), read as no data"), "{:?}", r.lost);
        assert_eq!(r.lost.len(), 3, "{:?}", r.lost);
    }

    #[test]
    fn a_full_level_1_cell_says_only_its_vertical_datum() {
        let bytes = dted("0060000E", "0450000S", (30, 30), (1201, 1201), |i, j| Some(((i + j) % 300) as i16));
        let r = read("s45e006.dt1", &bytes).unwrap();
        assert_eq!((r.width, r.height), (1201, 1201));
        let d = 3.0 / 3600.0;
        assert!((r.transform.origin_x - (6.0 - d / 2.0)).abs() < 1e-12);
        assert!((r.transform.origin_y - (-44.0 + d / 2.0)).abs() < 1e-12);
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].contains("EGM96"), "{:?}", r.lost);
    }

    #[test]
    fn high_latitudes_have_wider_longitude_spacing() {
        // Level 1, zone II (50°–70°): 3″ by 6″, 601 longitude lines of 1201 points.
        let bytes = dted("0100000E", "0600000N", (60, 30), (601, 1201), |i, j| Some((i % 50 + j % 50) as i16));
        let r = read("n60e010.dt1", &bytes).unwrap();
        assert_eq!((r.width, r.height), (601, 1201));
        let (dx, dy) = (6.0 / 3600.0, 3.0 / 3600.0);
        assert!((r.transform.pixel_width - dx).abs() < 1e-15 && (r.transform.pixel_height + dy).abs() < 1e-15);
        assert!((r.transform.origin_x - (10.0 - dx / 2.0)).abs() < 1e-12);
        assert!((r.transform.origin_y - (61.0 + dy / 2.0)).abs() < 1e-12);
        assert_eq!(r.value(0, 600, 1200), Some(0.0 + 0.0), "the south-east post: column 600, point 0");
        assert_eq!(r.value(0, 7, 1200 - 9), Some(16.0));
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        // The same spacing at 40°N isn't what the spec sets there.
        let r = read("n40e010.dt1", &dted("0100000E", "0400000N", (60, 30), (601, 1201), |_, _| Some(1))).unwrap();
        assert!(r.lost.iter().any(|l| l.contains("3″ of latitude by 6″ of longitude") && l.contains("3″ by 3″")), "{:?}", r.lost);
    }

    #[test]
    fn bad_columns_and_cut_files_are_said() {
        let rows = 11;
        let mut bytes = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| Some((i + j) as i16));
        // Column 2's checksum is wrong; column 5's sentinel is.
        let c2 = at(2, rows) + 8;
        bytes[c2 + 1] ^= 1;
        bytes[at(5, rows)] = 0xab;
        let r = read("x.dt1", &bytes).unwrap();
        // Column 2's heights are read as stored: point 3's is 2 + 3 = 5, point 0's (the flipped
        // byte's) 2 + 0 + 1 = 3.
        assert_eq!(r.value(0, 2, 10 - 3), Some(5.0), "a column failing its checksum is read as stored");
        assert_eq!(r.value(0, 2, 10), Some(3.0));
        assert_eq!(r.value(0, 5, 3), None, "a damaged header is no data");
        assert_eq!(r.value(0, 4, 3), Some(11.0));
        assert!(
            r.lost.iter().any(|l| l
                == "1 of its 11 columns' checksum doesn't match its heights (some producers write them wrongly); its heights are read as stored"),
            "{:?}",
            r.lost
        );
        assert!(r.lost.iter().any(|l| l.starts_with("1 of its 11 columns has a damaged record header")), "{:?}", r.lost);
        assert!(!r.lost.iter().any(|l| l.contains("void")), "voids in bad columns aren't counted: {:?}", r.lost);
        // Every checksum wrong: the cell still opens, read as stored, and the count says so.
        let mut sums = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| Some((i + j) as i16));
        for i in 0..11 {
            sums[at(i, rows) + 12 + 2 * rows - 1] ^= 1;
        }
        let r = read("x.dt1", &sums).unwrap();
        assert_eq!(r.value(0, 7, 10), Some(7.0));
        assert!(
            r.lost.iter().any(|l| l
                == "11 of its 11 columns' checksums don't match their heights (some producers write them wrongly); their heights are read as stored"),
            "{:?}",
            r.lost
        );
        // Every count its place (the clean file) says nothing about counts.
        let clean = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, _| Some(i as i16));
        let r = read("x.dt2", &clean).unwrap();
        assert!(!r.lost.iter().any(|l| l.contains("count")), "{:?}", r.lost);
        // Cut in the middle of column 8: columns 8–10 are missing.
        let cut = &bytes[..at(8, rows) + 7];
        let r = read("x.dt2", cut).unwrap();
        assert_eq!(r.value(0, 8, 0), None);
        assert!(r.lost.iter().any(|l| l == "the file is cut short: 3 of its 11 columns have no heights, read as no data"), "{:?}", r.lost);
        // Bytes past the last column are said.
        let mut longer = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |_, _| Some(1));
        longer.extend([0; 5]);
        assert!(read("x.dt2", &longer).unwrap().lost.iter().any(|l| l == "5 bytes after its last column are ignored"));
        // No whole record, or no good one, is refused.
        assert!(read("x.dt2", &bytes[..HEADERS + 20]).unwrap_err().to_string().contains("no whole data record"));
        let mut none = dted("0000000E", "0000000N", (3600, 3600), (2, rows), |_, _| Some(1));
        none[at(0, rows)] = 0;
        none[at(1, rows)] = 0;
        assert!(read("x.dt2", &none).unwrap_err().to_string().contains("none starts with the 0xAA sentinel"));
    }

    #[test]
    fn a_file_that_grows_after_its_size_is_checked_says_so() {
        let clean = dted("0000000E", "0000000N", (3600, 3600), (11, 11), |_, _| Some(1));
        let len = clean.len() as u64;
        let after = |r: &Raster| r.lost.iter().filter(|l| l.contains("after its last column")).cloned().collect::<Vec<_>>();
        // Read as checked: nothing past the last column.
        assert!(after(&read_file("x.dt2", len, clean.as_slice()).unwrap()).is_empty());
        // 9 bytes written after the size was checked are read and said.
        let mut grown = clean.clone();
        grown.extend([0; 9]);
        assert_eq!(after(&read_file("x.dt2", len, grown.as_slice()).unwrap()), ["9 bytes after its last column are ignored"]);
        // A file larger than any cell is refused before it is read.
        let e = read_file("x.dt2", MAX_BYTES + 1, clean.as_slice()).unwrap_err().to_string();
        assert!(e.contains("larger than any DTED cell"), "{e}");
    }

    #[test]
    fn records_numbered_from_1_are_read_by_place() {
        let rows = 11;
        let mut b = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| Some((i * 100 + j) as i16));
        // Every longitude count one more than its place, the checksums made right.
        for i in 0..11 {
            let rec = at(i, rows);
            b[rec + 4..rec + 6].copy_from_slice(&(i as u16 + 1).to_be_bytes());
            let sum: u32 = b[rec..rec + 8 + 2 * rows].iter().map(|v| u32::from(*v)).sum();
            b[rec + 8 + 2 * rows..rec + 12 + 2 * rows].copy_from_slice(&sum.to_be_bytes());
        }
        let r = read("x.dt1", &b).unwrap();
        assert_eq!(r.value(0, 0, 10), Some(0.0), "column 0, point 0");
        assert_eq!(r.value(0, 10, 0), Some(1010.0), "column 10, point 10");
        assert_eq!(
            r.lost.iter().filter(|l| l.contains("longitude count")).collect::<Vec<_>>(),
            ["11 of its 11 columns' longitude counts number them from 1, not 0; they are read by their place in the file"]
        );
        assert!(!r.lost.iter().any(|l| l.contains("damaged") || l.contains("checksum")), "{:?}", r.lost);
        // One column numbered from 1 among columns numbered from 0 isn't a cell numbered from 1:
        // refused, naming it.
        let mut one = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |_, _| Some(1));
        renumber(&mut one, at(3, rows), rows, 4);
        let e = read("x.dt1", &one).unwrap_err().to_string();
        assert!(e.contains("the record at column 3 counts itself as column 4, not 3"), "{e}");
    }

    #[test]
    fn a_cell_missing_its_first_100_columns_is_refused() {
        // A full cell of 3601 longitude lines (1″ apart), partial-cell indicator 00, holding only
        // the 401 records counted 100 to 500, from the first data byte. By place, column 100's
        // heights would land in column 0.
        let rows = 11;
        let b = dted("0000000E", "0000000N", (10, 3600), (3601, rows), |i, j| Some((i % 1000 * 10 + j) as i16));
        let mut offset = b[..HEADERS].to_vec();
        offset.extend(&b[at(100, rows)..at(501, rows)]);
        let e = read("n00e000.dt2", &offset).unwrap_err().to_string();
        assert!(
            e.contains("n00e000.dt2") && e.contains("the record at column 0 counts itself as column 100, not 0") && e.contains("refused"),
            "{e}"
        );
        // The whole cell, every count its place, opens.
        assert!(read("n00e000.dt2", &b).is_ok());
    }

    /// Sets the longitude count of the record at `rec` to `count`, its checksum made right.
    fn renumber(b: &mut [u8], rec: usize, rows: usize, count: u16) {
        b[rec + 4..rec + 6].copy_from_slice(&count.to_be_bytes());
        let sum: u32 = b[rec..rec + 8 + 2 * rows].iter().map(|v| u32::from(*v)).sum();
        b[rec + 8 + 2 * rows..rec + 12 + 2 * rows].copy_from_slice(&sum.to_be_bytes());
    }

    #[test]
    fn counts_naming_columns_without_records_are_refused() {
        let rows = 11;
        let full = || dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| Some((i * 100 + j) as i16));
        let refused = |b: &[u8], place: usize, count: usize| {
            let e = read("n00e000.dt1", b).unwrap_err().to_string();
            assert!(
                e.contains("n00e000.dt1")
                    && e.contains(&format!("the record at column {place} counts itself as column {count}"))
                    && e.contains("refused"),
                "{e}"
            );
        };
        // The clean file: every count its place, nothing said about counts.
        let r = read("x.dt1", &full()).unwrap();
        assert_eq!(r.value(0, 10, 0), Some(1010.0));
        assert!(!r.lost.iter().any(|l| l.contains("longitude count")), "{:?}", r.lost);
        // Leading columns left out: only the records of columns 3 to 7, counted 3 to 7, from the
        // first data byte. By place, column 3's heights would land in column 0.
        let b = full();
        let mut omitted = b[..HEADERS].to_vec();
        omitted.extend(&b[at(3, rows)..at(8, rows)]);
        refused(&omitted, 0, 3);
        // Offset by many: every record counting its place + 5.
        let mut many = full();
        for i in 0..11 {
            renumber(&mut many, at(i, rows), rows, i as u16 + 5);
        }
        refused(&many, 0, 5);
        // Offset by one, but not a whole cell: counted 1 to 8 in a file cut short after 8
        // records, column 0 may have been left out.
        let mut one = full();
        for i in 0..11 {
            renumber(&mut one, at(i, rows), rows, i as u16 + 1);
        }
        refused(&one[..at(8, rows)], 0, 1);
        // Offset by one from the middle: column 5 left out, the records after it counting 6 on.
        let mut gap = full();
        for i in 5..10 {
            renumber(&mut gap, at(i, rows), rows, i as u16 + 1);
        }
        refused(&gap[..at(10, rows)], 5, 6);
        // Offset past the cell's last line: every count names no column, so none can be trusted.
        let mut past = full();
        for i in 0..11 {
            renumber(&mut past, at(i, rows), rows, i as u16 + 20);
        }
        refused(&past, 0, 20);
        // A count naming no column, or a column that has its own record, is refused too.
        let mut stray = full();
        renumber(&mut stray, at(2, rows), rows, 40);
        refused(&stray, 2, 40);
        let mut twice = full();
        renumber(&mut twice, at(6, rows), rows, 9);
        refused(&twice, 6, 9);
    }

    #[test]
    fn a_nonzero_latitude_count_is_refused() {
        // A full-grid record starts at latitude point 0 (3.12 f); one counting another point would
        // place its heights north or south of where they belong: refused, naming the file, column
        // and count.
        let rows = 11;
        let full = || dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| Some((i * 100 + j) as i16));
        let set = |b: &mut [u8], i: usize, raw: [u8; 2]| {
            let rec = at(i, rows);
            b[rec + 6..rec + 8].copy_from_slice(&raw);
            let sum: u32 = b[rec..rec + 8 + 2 * rows].iter().map(|v| u32::from(*v)).sum();
            b[rec + 8 + 2 * rows..rec + 12 + 2 * rows].copy_from_slice(&sum.to_be_bytes());
        };
        for (i, raw, point) in [(8, [0, 1], 1), (0, [0x80, 0x03], -3)] {
            let mut b = full();
            set(&mut b, i, raw);
            let e = read("n00e000.dt1", &b).unwrap_err().to_string();
            assert!(
                e.contains("n00e000.dt1")
                    && e.contains(&format!("the record at column {i} counts its first height as latitude point {point}, not 0"))
                    && e.contains("refused"),
                "{e}"
            );
        }
        // Every latitude count 0 (the clean file) opens and says nothing about counts.
        let r = read("n00e000.dt1", &full()).unwrap();
        assert_eq!(r.value(0, 10, 0), Some(1010.0));
        assert!(!r.lost.iter().any(|l| l.contains("count")), "{:?}", r.lost);
    }

    #[test]
    fn headers_are_checked_before_anything_is_held() {
        let good = dted("0000000E", "0000000N", (3600, 3600), (11, 11), |_, _| Some(1));
        let with = |at: usize, s: &str| {
            let mut b = good.clone();
            b[at..at + s.len()].copy_from_slice(s.as_bytes());
            b
        };
        for (b, why) in [
            (with(0, "XHL"), "no UHL"),
            (with(80, "XSI"), "DSI and ACC"),
            (with(728, "XCC"), "DSI and ACC"),
            (with(47, "9999"), "longitude lines"),
            (with(51, "0001"), "latitude points"),
            (with(47, "12x4"), "longitude lines"),
            (with(20, "0000"), "longitude interval"),
            (with(4, "1810000E"), "longitude of origin"),
            (with(4, "0006000E"), "longitude of origin"),
            (with(12, "0910000N"), "latitude of origin"),
            (with(12, "0000000Q"), "latitude of origin"),
            (with(20, "9999"), "more than the one-degree cell"),
            (with(12, "0895959N"), "more than the one-degree cell"),
        ] {
            let e = read("x.dt1", &b).unwrap_err().to_string();
            assert!(e.contains(why), "{why}: {e}");
        }
        assert!(read("x.dt1", &good[..100]).unwrap_err().to_string().contains("cut short"));
        assert!(read("x.dt1", b"").is_err());
    }

    #[test]
    fn a_dsi_that_disagrees_or_another_datum_is_said() {
        let mut b = dted("0000000E", "0000000N", (3600, 3600), (11, 11), |_, _| Some(1));
        b[DSI + 277..DSI + 281].copy_from_slice(b"0030");
        b[DSI + 141..DSI + 144].copy_from_slice(b"MSL");
        b[DSI + 144..DSI + 149].copy_from_slice(b"WGS72");
        let r = read("x.dt1", &b).unwrap();
        assert!(r.lost.iter().any(|l| l.contains("longitude interval 30 (the UHL's is 3600)")), "{:?}", r.lost);
        assert!(r.lost.iter().any(|l| l.contains("mean sea level (its vertical datum MSL)")), "{:?}", r.lost);
        assert!(r.lost.iter().any(|l| l.contains("\"WGS72\"")), "{:?}", r.lost);
    }

    #[test]
    fn a_level_2_cell_past_the_budget_is_read_at_every_other_post() {
        let bytes = dted("0000000E", "0000000N", (3600, 3600), (11, 11), |i, j| Some((i * 100 + j) as i16));
        let r = read_limited("x.dt2", &bytes, 100).unwrap();
        assert_eq!((r.width, r.height, r.overview, r.step), (6, 6, Some((11, 11)), 2));
        // Output (col 1, row 1) is column 2, point 10 - 2 = 8.
        assert_eq!(r.value(0, 1, 1), Some(208.0));
        // Cells are two posts wide, each centered on the post kept: column 2, point 8 is at 0.2°E,
        // 0.8°N.
        let t = r.transform;
        assert!((t.pixel_width - 0.2).abs() < 1e-12 && (t.origin_x + 0.1).abs() < 1e-12 && (t.origin_y - 1.1).abs() < 1e-12);
        let kept = t.to_world(1.5, 1.5);
        assert!((kept.x - 0.2).abs() < 1e-9 && (kept.y - 0.8).abs() < 1e-9, "{kept:?}");
    }

    #[test]
    fn a_step_that_doesnt_divide_the_posts_keeps_the_scale_exact() {
        // 9 × 13 posts at step 2, as a level 2 cell at 75°–80° (901 × 3601) is at a step: 5 × 7
        // cells, each exactly two posts across and down.
        let (cols, rows) = (9, 13);
        let bytes = dted("0000000E", "0000000N", (1800, 1800), (cols, rows), |i, j| Some((i * 100 + j) as i16));
        let r = read_limited("x.dt2", &bytes, 100).unwrap();
        assert_eq!((r.width, r.height, r.step), (5, 7, 2));
        // The layer says the file's own size; the scale is the step, exactly, though 9 and 13
        // aren't multiples of the size read (the warp and world files take `file_pixels`).
        assert_eq!(r.overview, Some((cols as u32, rows as u32)));
        assert_eq!((r.file_pixels(), r.file_origin()), ((2.0, 2.0), (-0.5, -0.5)));
        // The kept posts are columns 0, 2, …, 8 and points 12, 10, …, 0, 0.05° apart: the extent
        // reaches one post (half a cell) past the outer ones on every side.
        let e = r.extent();
        for (got, want) in [(e.min_x, -0.05), (e.max_x, 0.45), (e.min_y, -0.05), (e.max_y, 0.65)] {
            assert!((got - want).abs() < 1e-9, "{e:?}");
        }
        assert!(((e.min_x + e.max_x) / 2.0 - 0.2).abs() < 1e-9 && ((e.min_y + e.max_y) / 2.0 - 0.3).abs() < 1e-9, "{e:?}");
        assert_eq!(r.value(0, 4, 0), Some(812.0), "column 8, point 12");
        assert_eq!(r.value(0, 4, 6), Some(800.0), "column 8, point 0");
    }

    #[test]
    fn counts_at_a_step_are_the_whole_files() {
        // Voids and two's-complement heights in kept and skipped columns alike.
        let rows = 11;
        let mut b = dted("0000000E", "0000000N", (3600, 3600), (11, rows), |i, j| if (i + j) % 4 == 0 { None } else { Some(1) });
        for (i, j) in [(1, 2), (4, 3), (7, 7)] {
            let rec = at(i, rows);
            b[rec + 8 + 2 * j..rec + 10 + 2 * j].copy_from_slice(&(-20i16).to_be_bytes());
        }
        let whole = read("x.dt1", &b).unwrap();
        let stepped = read_limited("x.dt1", &b, 40).unwrap();
        assert_eq!(stepped.step, 2);
        let notes = |r: &Raster| r.lost.iter().filter(|l| l.contains("void") || l.contains("two's")).cloned().collect::<Vec<_>>();
        assert_eq!(notes(&whole), notes(&stepped));
        assert!(notes(&whole).iter().any(|l| l.starts_with("3 of its heights were written in two's complement")), "{:?}", whole.lost);
        assert!(notes(&whole).iter().any(|l| l.starts_with("30 of its 121 heights are voids")), "{:?}", whole.lost);
    }

    #[test]
    fn a_partial_cell_is_read_on_its_full_grid_and_refused_in_the_tape_layout() {
        let partial = |mut b: Vec<u8>| {
            b[DSI + 289..DSI + 291].copy_from_slice(b"50");
            b
        };
        // At 6′ spacing a whole cell is 11 × 11 posts. With the full grid (as on CD-ROM), a
        // partial cell's missing posts are voids, read as no data and said.
        let cdrom = partial(dted("1220000W", "0370000N", (3600, 3600), (11, 11), |i, _| (i < 5).then_some(100)));
        let r = read("n37.dt1", &cdrom).unwrap();
        assert_eq!(r.value(0, 0, 0), Some(100.0));
        assert_eq!(r.value(0, 8, 0), None);
        assert!(r.lost.iter().any(|l| l.contains("no data")), "{:?}", r.lost);
        // On tape the UHL still counts the whole cell, but the DSI only its 6 longitude lines
        // with heights: refused, not misplaced.
        let mut tape = partial(dted("1220000W", "0370000N", (3600, 3600), (11, 11), |_, _| Some(100)));
        tape[DSI + 285..DSI + 289].copy_from_slice(b"0006");
        let err = read("n37.dt1", &tape).unwrap_err().to_string();
        assert!(err.contains("partial cell (50% coverage) in the magnetic-tape layout"), "{err}");
        assert!(err.contains("only the 6 of 11 longitude lines"), "{err}");
        // On tape with a height on every line the DSI counts the whole cell too, but the record
        // that starts 5 points up is 10 bytes short, so the file is: refused, as is a partial cell
        // cut short, which can't be told from it.
        let mut tape = cdrom.clone();
        tape[at(0, 11) + 7] = 5;
        tape.drain(at(0, 11) + 8..at(0, 11) + 18);
        let err = read("n37.dt1", &tape).unwrap_err().to_string();
        assert!(
            err.contains("whose file is 10 bytes shorter than its full grid of 11 × 11 posts: cut short, or in the magnetic-tape layout"),
            "{err}"
        );
        let err = read("n37.dt1", &cdrom[..cdrom.len() - 3]).unwrap_err().to_string();
        assert!(err.contains("3 bytes shorter"), "{err}");
        // A complete cell cut short is still read as far as it goes, and a whole partial cell
        // with a damaged record is read with the damage said.
        let complete = dted("1220000W", "0370000N", (3600, 3600), (11, 11), |_, _| Some(100));
        let r = read("n37.dt1", &complete[..complete.len() - 3]).unwrap();
        assert!(r.lost.iter().any(|l| l.contains("the file is cut short")), "{:?}", r.lost);
        let mut damaged = cdrom.clone();
        damaged[at(3, 11)] = 0;
        let r = read("n37.dt1", &damaged).unwrap();
        assert!(r.lost.iter().any(|l| l.contains("damaged record header")), "{:?}", r.lost);
        // A complete cell's file (indicator 00) whose DSI disagrees is read, the difference said.
        let mut complete = dted("1220000W", "0370000N", (3600, 3600), (11, 11), |_, _| Some(100));
        complete[DSI + 285..DSI + 289].copy_from_slice(b"0006");
        let r = read("n37.dt1", &complete).unwrap();
        assert!(r.lost.iter().any(|l| l.contains("DSI record gives a different")), "{:?}", r.lost);
    }

    #[test]
    fn junk_is_refused() {
        let good =
            dted(
                "1220000W",
                "0370000N",
                (3600, 3600),
                (11, 11),
                |i, j| if (i + j) % 5 == 0 { None } else { Some(-(i as i16) * 7 + j as i16) },
            );
        let mut seed = 0x89020bu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // Every prefix is read or refused, never a panic.
        for cut in 0..=good.len() {
            let _ = read("x.dt1", &good[..cut]);
        }
        // Random bytes, alone and after good headers, and flipped bytes in a good file.
        for _ in 0..200 {
            let n = (next() % 8000) as usize;
            let junk: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            assert!(read("x.dt1", &junk).is_err());
            let mut after = good[..HEADERS].to_vec();
            after.extend(&junk);
            let _ = read("x.dt1", &after);
            let mut flipped = good.clone();
            for _ in 0..4 {
                let at = (next() % flipped.len() as u64) as usize;
                flipped[at] = next() as u8;
            }
            let _ = read("x.dt1", &flipped);
        }
    }

    /// A reader that gives at most `n` bytes a call, as a slow file or a pipe may.
    struct Trickle<'a>(&'a [u8], usize);

    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.1.min(buf.len()).min(self.0.len());
            let (now, rest) = self.0.split_at(n);
            buf[..n].copy_from_slice(now);
            self.0 = rest;
            Ok(n)
        }
    }

    /// What a read gave, for comparing two reads bit for bit.
    fn same(a: &Raster, b: &Raster) {
        assert_eq!(
            (a.width, a.height, a.transform, a.overview, a.step, a.posts),
            (b.width, b.height, b.transform, b.overview, b.step, b.posts)
        );
        assert_eq!(a.lost, b.lost);
        let bits = |r: &Raster| r.bands.iter().map(|b| b.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>()).collect::<Vec<_>>();
        assert_eq!(bits(a), bits(b));
    }

    #[test]
    fn a_cell_streamed_in_small_pieces_reads_as_its_bytes_do() {
        let bytes = dted("0060000E", "0450000N", (30, 30), (121, 121), |i, j| match (i + j) % 53 {
            0 => None,
            1 => Some(-12),
            _ => Some((i * 7 + j) as i16),
        });
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[1, 2, 3]);
        let cut = &bytes[..at(100, 121) + 9];
        for (b, max) in [(&bytes[..], max_samples()), (&bytes[..], 100), (&longer[..], max_samples()), (cut, max_samples())] {
            let whole = read_limited("x.dt1", b, max).unwrap();
            for n in [1, 5, 4096] {
                same(&whole, &read_from_limited("x.dt1", Trickle(b, n), max).unwrap());
            }
        }
        // Every refusal is the one the same bytes give: headers or records cut short, a record
        // counting another column or latitude point, a partial cell cut short.
        let mut offset = bytes.clone();
        offset[at(3, 121) + 5] = 9;
        let mut shifted = bytes.clone();
        shifted[at(4, 121) + 7] = 2;
        let mut partial = bytes.clone();
        partial[DSI + 289..DSI + 291].copy_from_slice(b"50");
        for b in [&bytes[..100], &bytes[..HEADERS + 20], &offset[..], &shifted[..], &partial[..partial.len() - 3], &[][..]] {
            let from_bytes = read("x.dt1", b).unwrap_err().to_string();
            assert_eq!(from_bytes, read_from("x.dt1", Trickle(b, 3)).unwrap_err().to_string());
        }
    }
}
