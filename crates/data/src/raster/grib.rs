//! GRIB files (`.grb`, `.grib`, `.grb2`, `.grib2`): weather and climate model output, read
//! without GDAL. Each field is a band.
//!
//! Read: editions 1 and 2, simple packing (GRIB1's grid point data with simple packing, GRIB2's
//! data representation template 5.0), on a regular latitude/longitude grid (GRIB1 data
//! representation type 0, GRIB2 grid definition template 3.0), in any scanning order, with or
//! without a bitmap. Following WMO's Manual on Codes (WMO-No. 306), Volume I.2, FM 92 GRIB.
//!
//! Not read yet, and said: complex, JPEG 2000, PNG and the other packings; other grids (Gaussian,
//! rotated, projected, quasi-regular). A message the reader can't read is left out with a notice
//! naming why; a file with nothing it can read is an error naming why.
//!
//! The fields on the grid of the first one read are the raster's bands, in file order; fields on
//! another grid are left out with a notice. Values are `(R + X × 2^E) / 10^D`, in the units the
//! file stores (kelvin stays kelvin), unpacked in `f64`; those `f32` changes are counted in
//! [`Raster::rounded`]. Points a bitmap masks are NaN. Rows are laid out north up,
//! and a grid round the whole globe runs from −180° (as GDAL lays it out).

use super::{Crs, Exact, GeoTransform, Raster, Uncounted, max_samples};
use crate::{DataError, Result};
use std::io::{Read, Seek, SeekFrom};

/// File extensions read as GRIB.
pub const EXTENSIONS: &[&str] = &["grb", "grib", "grb2", "grib2"];

/// Whether a file name is a GRIB file's.
pub fn is_grib_name(name: &str) -> bool {
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// The bytes read at a time while looking for the next message ("GRIB"): files may put a
/// bulletin header or padding between messages.
const CHUNK: usize = 4 * 1024;

/// The most bits a packed value may have: GRIB allows more, no producer writes them.
const MAX_BITS: u32 = 32;

/// The smallest a message can be: GRIB1's indicator, its shortest sections and "7777".
const MIN_MESSAGE: u64 = 8 + 28 + 11 + 4;

/// The most sections a GRIB2 message is walked through: a real one has about eight a field, and
/// a message of more (sections of five octets, from a damaged or hostile file) is left out.
const MAX_SECTIONS: usize = 1024;

fn bad(m: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GRIB: {m}"))
}

/// Reads a GRIB file's bytes (an upload, a download, a byte range of a larger file).
pub fn read_bytes(bytes: &[u8], name: &str) -> Result<Raster> {
    read(std::io::Cursor::new(bytes), name)
}

/// Reads a GRIB file: its fields on the first readable one's grid, as bands.
pub fn read<R: Read + Seek>(source: R, name: &str) -> Result<Raster> {
    read_with_nodata(source, name, None)
}

/// [`read`], with the no-data value of a sidecar (`.aux.xml`) beside the file, whose values aren't
/// counted as rounded ([`Uncounted`]'s rule): GRIB has no no-data value of its own (a bitmap masks).
pub fn read_with_nodata<R: Read + Seek>(source: R, name: &str, sidecar_nodata: Option<f64>) -> Result<Raster> {
    read_limited(source, name, sidecar_nodata, max_samples())
}

/// [`read_with_nodata`], holding at most `limit` values.
fn read_limited<R: Read + Seek>(mut source: R, name: &str, sidecar_nodata: Option<f64>, limit: usize) -> Result<Raster> {
    let uncounted = Uncounted::new(None, sidecar_nodata);
    let file_len = source.seek(SeekFrom::End(0))?;
    source.seek(SeekFrom::Start(0))?;
    let mut fields: Vec<Field> = Vec::new();
    let mut skipped = Skipped::default();
    let mut at = 0u64;
    let mut messages = 0usize;
    let mut held = 0usize;
    let mut chunk = vec![0u8; CHUNK];
    let mut counted = Counted::default();
    // Progress in KiB of the file, counted as the search for each message reads it and as each
    // message is read; one message is decoded without a check (at most `max_samples` values).
    crate::progress::begin(usize::try_from(file_len / 1024).unwrap_or(usize::MAX));
    // Whether the bytes before the next "GRIB" are the rest of a message left out (whose length
    // isn't known), rather than something outside the messages.
    let mut in_skipped = false;
    // Whether the last message found said it runs past the end of the file: cut short, unless
    // another message follows it (its length is then damaged).
    let mut overrun = false;
    loop {
        let (found, junk) = find_message(&mut source, at, file_len, &mut chunk, &mut counted)?;
        if !in_skipped {
            skipped.junk += junk;
        }
        let Some(start) = found else {
            if overrun {
                skipped.cut += 1;
            }
            break;
        };
        if overrun {
            skipped.message("damaged".to_string());
            overrun = false;
        }
        let (len, edition) = message_header(&mut source, start)?;
        in_skipped = false;
        match (edition, len) {
            // Not a message GRIB 1 or 2 describes: its four letters are passed, and the search
            // goes on.
            (Some(e), _) if e != 1 && e != 2 => {
                skipped.message(format!("not GRIB 1 or 2 (edition {e})"));
                (at, in_skipped) = (start + 4, true);
                continue;
            }
            (Some(_), len) if len < MIN_MESSAGE => {
                skipped.message("damaged".to_string());
                (at, in_skipped) = (start + 4, true);
                continue;
            }
            // A message cut short (a byte range that ends early, a truncated download): what was
            // read before it stands, and the cut is said.
            (None, _) => {
                skipped.cut += 1;
                break;
            }
            // A length past the end of the file: the search goes on from inside the message, so a
            // message after it is still found (and this one said as damaged); without one, the
            // file ends inside it.
            (Some(_), len) if start.saturating_add(len) > file_len => {
                (at, in_skipped, overrun) = (start + 4, true, true);
                continue;
            }
            // A length that doesn't end on "7777" says the message wrongly: the search goes on
            // from inside it, so a message it would hide is still found.
            (Some(_), len) if !ends_message(&mut source, start + len)? => {
                skipped.message("damaged".to_string());
                (at, in_skipped) = (start + 4, true);
                continue;
            }
            _ => {}
        }
        // What the headers say, before the body is read: sections that end on a "7777" before the
        // message's length does say that length wrongly (it may hide the next message), so the
        // search goes on from inside it, as for a length that doesn't end on "7777".
        let header = header(&mut source, start, len, edition == Some(2))?;
        if header == Header::WrongLength {
            skipped.message("damaged".to_string());
            (at, in_skipped) = (start + 4, true);
            continue;
        }
        messages += 1;
        at = start + len;
        counted.add(len)?;
        // Past the memory limit, a message none of whose fields fits in the room left, or one
        // larger than those points can take (at most 32 bits each, a bitmap and a megabyte of
        // sections): only counted, never read. A message is read whole, so that size bounds the bytes
        // held: its fields are left out even where one of them would fit, and are said apart.
        // A grid this reader refuses, or too many sections, leaves the message out unread.
        match header {
            Header::Refused(why) => {
                skipped.add(why);
                continue;
            }
            Header::TooManySections => {
                skipped.message(format!("damaged (more than {MAX_SECTIONS} sections)"));
                continue;
            }
            Header::Points(_) | Header::Unknown | Header::WrongLength => {}
        }
        let room = limit.saturating_sub(held);
        let too_long = len > (room as u64).saturating_mul(5).saturating_add(1 << 20);
        if held >= limit || matches!(header, Header::Points(n) if n > room) {
            skipped.over_limit += count_fields(&mut source, start, len, edition == Some(2))?;
            continue;
        }
        if too_long {
            skipped.too_long += count_fields(&mut source, start, len, edition == Some(2))?;
            continue;
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len as usize).map_err(|_| bad("not enough memory for a message"))?;
        source.seek(SeekFrom::Start(start))?;
        source.by_ref().take(len).read_to_end(&mut bytes)?;
        let first = fields.first().map(|f| f.grid);
        let decoded = if edition == Some(1) { vec![grib1(&bytes, room, first, uncounted)] } else { grib2(&bytes, room, first, uncounted) };
        for d in decoded {
            match d {
                Ok(f) => {
                    let first = fields.first().map(|f| f.grid);
                    if first.is_some_and(|g| !g.same(&f.grid)) {
                        skipped.other_grid += 1;
                    } else if held + f.values.len() > limit {
                        skipped.over_limit += 1;
                    } else {
                        held += f.values.len();
                        fields.push(f);
                    }
                }
                Err(Skip::TooLarge) => skipped.over_limit += 1,
                Err(Skip::OtherGrid) => skipped.other_grid += 1,
                Err(Skip::Other(why)) => skipped.add(why),
                Err(Skip::Damaged(why)) => skipped.damaged(why),
            }
        }
    }
    if messages == 0 && skipped.messages.is_empty() {
        return Err(bad(if skipped.cut > 0 { "the first message is cut short" } else { "no GRIB message in the file" }));
    }
    if fields.is_empty() {
        return Err(bad(format!("nothing in it can be read here: {}", skipped.notes(limit).join("; "))));
    }
    // Each grid's notices, once: grids at the same places may still say different things (one
    // laid out from BAM's latitudes, one not). Past [`MAX_REASONS`] of them, the fields with more
    // are counted, so the list and the time spent on it stay small; a field on the grid of the one
    // before says what it said.
    let mut notes: Vec<String> = Vec::new();
    let mut unlisted = 0usize;
    let mut last: Option<(Grid, bool)> = None;
    for f in &fields {
        let more = match last {
            Some((g, more)) if g == f.grid => more,
            _ => {
                let mut more = false;
                for n in f.grid.notes() {
                    if notes.contains(&n) {
                        continue;
                    }
                    if notes.len() < MAX_REASONS {
                        notes.push(n);
                    } else {
                        more = true;
                    }
                }
                more
            }
        };
        last = Some((f.grid, more));
        unlisted += usize::from(more);
    }
    if unlisted > 0 {
        notes.push(format!("more notices about the grids of {} aren't listed", count(unlisted)));
    }
    let mut r = to_raster(fields, name)?;
    r.lost.extend(notes);
    r.lost.extend(skipped.notes(limit));
    Ok(r)
}

/// The offset of the next "GRIB" at or after `at` (`None` at the end of the file), and how many
/// of the bytes passed on the way aren't zeros (padding): a bulletin header, other data.
fn find_message<R: Read + Seek>(
    source: &mut R,
    at: u64,
    file_len: u64,
    chunk: &mut [u8],
    counted: &mut Counted,
) -> Result<(Option<u64>, u64)> {
    let mut from = at;
    let mut junk = 0u64;
    while from < file_len {
        source.seek(SeekFrom::Start(from))?;
        let n = usize::try_from(file_len - from).unwrap_or(usize::MAX).min(chunk.len());
        let window = chunk.get_mut(..n).unwrap_or_default();
        source.read_exact(window)?;
        // Each window looks at Cancel, and counts the bytes it passes (a message's own are counted
        // as it is read): a long run of other bytes can be stopped.
        if let Some(p) = window.windows(4).position(|w| w == b"GRIB") {
            counted.add(p as u64)?;
            junk += window.iter().take(p).filter(|b| **b != 0).count() as u64;
            return Ok((Some(from + p as u64), junk));
        }
        // The last three bytes may start a "GRIB" that the next chunk ends.
        let keep = if from + n as u64 >= file_len { 0 } else { 3.min(n) };
        counted.add((n - keep) as u64)?;
        junk += window.iter().take(n - keep).filter(|b| **b != 0).count() as u64;
        from += (n - keep).max(1) as u64;
    }
    Ok((None, junk))
}

/// A message's length and edition, from its indicator section: no edition when the file ends
/// inside the section, a length of 0 for an edition other than 1 and 2.
fn message_header<R: Read + Seek>(source: &mut R, start: u64) -> Result<(u64, Option<u8>)> {
    source.seek(SeekFrom::Start(start))?;
    let mut head = [0u8; 16];
    let mut got = 0;
    while got < head.len() {
        match source.read(head.get_mut(got..).unwrap_or_default())? {
            0 => break,
            n => got += n,
        }
    }
    if got < 8 {
        return Ok((0, None));
    }
    let edition = head[7];
    let len = match edition {
        1 => u64::from(be(&head, 4, 3)),
        2 if got == 16 => head[8..16].iter().fold(0u64, |v, &b| (v << 8) | u64::from(b)),
        2 => return Ok((0, None)),
        _ => 0,
    };
    Ok((len, Some(edition)))
}

/// Whether the four octets before `end` are "7777", the end of a message.
fn ends_message<R: Read + Seek>(source: &mut R, end: u64) -> Result<bool> {
    let mut tail = [0u8; 4];
    source.seek(SeekFrom::Start(end.saturating_sub(4)))?;
    source.read_exact(&mut tail)?;
    Ok(&tail == b"7777")
}

/// The bytes counted for the progress, in KiB, with what is left of a KiB carried over.
#[derive(Default)]
struct Counted(u64);

impl Counted {
    /// Counts `n` more bytes, and looks at Cancel.
    fn add(&mut self, n: u64) -> Result<()> {
        self.0 = self.0.saturating_add(n);
        let kib = self.0 / 1024;
        self.0 %= 1024;
        crate::progress::step(usize::try_from(kib).unwrap_or(usize::MAX))
    }
}

/// What a message's headers say before its body is read.
#[derive(Debug, PartialEq)]
enum Header {
    /// How many points the smallest of its fields covers (GRIB1's grid description, GRIB2's
    /// number of data points of the grid each data section follows): when not even that one fits
    /// in the room left, the message is left out unread; each of the others is admitted or not as
    /// it is decoded, unless the message's bytes are more than the room left lets be read whole.
    Points(usize),
    /// Sections that end on a "7777" before the message's length does, or don't chain to its
    /// end: the length is damaged.
    WrongLength,
    /// Headers the decoder will find damaged: it says why.
    Unknown,
    /// A GRIB1 grid this reader doesn't read, with the reason the decoder gives.
    Refused(String),
    /// A GRIB2 message of more than [`MAX_SECTIONS`] sections.
    TooManySections,
}

/// A message's [`Header`], from its headers alone.
fn header<R: Read + Seek>(source: &mut R, start: u64, len: u64, grib2: bool) -> Result<Header> {
    let end = start + len;
    let mut head = [0u8; 16];
    let read = |source: &mut R, at: u64, out: &mut [u8]| -> Result<bool> {
        if at + out.len() as u64 > end {
            return Ok(false);
        }
        source.seek(SeekFrom::Start(at))?;
        source.read_exact(out)?;
        Ok(true)
    };
    if !grib2 {
        // The product definition's length, grid number and flags, then the grid description's
        // type and sizes: the same refusals as `grib1` and `grib1_grid`, in the same words.
        if !read(source, start + 8, &mut head[..8])? {
            return Ok(Header::Unknown);
        }
        // The sections the flags say are there (product definition, grid description, bitmap,
        // binary data), by their lengths: ending on a "7777" before the message's end, or broken
        // (a section shorter than its length octets, or past the message's end), the message's
        // length can't be trusted (it may hide the next message).
        let flags = head[7];
        let mut at = start + 8 + u64::from(be(&head, 0, 3));
        let mut chained = true;
        for there in [flags & 0x80 != 0, flags & 0x40 != 0, true] {
            if !there {
                continue;
            }
            let mut size = [0u8; 3];
            if !read(source, at, &mut size)? || be(&size, 0, 3) < 3 {
                chained = false;
                break;
            }
            at += u64::from(be(&size, 0, 3));
        }
        let mut tail = [0u8; 4];
        if !chained || (at + 4 < end && read(source, at, &mut tail)? && &tail == b"7777") {
            return Ok(Header::WrongLength);
        }
        if flags & 0x80 == 0 {
            return Ok(Header::Refused(format!("predefined grids (grid {})", head[6])));
        }
        let gds = start + 8 + u64::from(be(&head, 0, 3));
        if !read(source, gds, &mut head[..10])? {
            return Ok(Header::Unknown);
        }
        let kind = head[5];
        if kind != 0 {
            return Ok(Header::Refused(format!("GRIB1 grids of data representation type {kind} (not latitude/longitude)")));
        }
        let (ni, nj) = (be(&head, 6, 2) as usize, be(&head, 8, 2) as usize);
        if ni == 0xffff || nj == 0xffff || ni == 0 || nj == 0 {
            return Ok(Header::Refused("quasi-regular grids".to_string()));
        }
        return Ok(Header::Points(ni.saturating_mul(nj)));
    }
    let (mut at, mut points, mut fewest, mut sections) = (start + 16, 0usize, None::<usize>, 0usize);
    // Each section's 5-octet header (a constant field's data section is no longer, just before
    // "7777"), and the grid's number of data points (octets 7 to 10) when it is section 3. The
    // last "7777" can't be read with a fifth octet: one that can comes before the message's end.
    // Sections that don't end on the last "7777" (one shorter than its header, or past the
    // message's end) break the message's length as one: it may hide the next message.
    while read(source, at, &mut head[..5])? {
        if &head[..4] == b"7777" {
            return Ok(Header::WrongLength);
        }
        let size = u64::from(be(&head, 0, 4));
        if size < 5 {
            return Ok(Header::WrongLength);
        }
        sections += 1;
        if sections > MAX_SECTIONS {
            return Ok(Header::TooManySections);
        }
        let mut count = [0u8; 4];
        match head[4] {
            3 if read(source, at + 6, &mut count)? => points = be(&count, 0, 4) as usize,
            7 => fewest = Some(fewest.map_or(points, |n| n.min(points))),
            _ => {}
        }
        at += size;
    }
    if at != end - 4 {
        return Ok(Header::WrongLength);
    }
    Ok(Header::Points(fewest.unwrap_or(0)))
}

/// How many fields a message holds: one in GRIB1, one a data section (7) in GRIB2, counted from
/// the sections' headers alone.
fn count_fields<R: Read + Seek>(source: &mut R, start: u64, len: u64, grib2: bool) -> Result<usize> {
    if !grib2 {
        return Ok(1);
    }
    let (mut at, end, mut n) = (start + 16, start + len, 0usize);
    for _ in 0..MAX_SECTIONS {
        if at + 5 > end {
            break;
        }
        source.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 5];
        source.read_exact(&mut head)?;
        if head.starts_with(b"7777") {
            break;
        }
        let size = u64::from(be(&head, 0, 4));
        if size < 5 {
            break;
        }
        n += usize::from(head[4] == 7);
        at += size;
    }
    Ok(n.max(1))
}

/// Why a field isn't read: past the memory limit, something this reader doesn't read yet (a
/// packing, a grid), or damage; each reason is said once with a count.
#[derive(Clone)]
enum Skip {
    TooLarge,
    /// On another grid than the first field kept: told before it is decoded, so it takes none of
    /// the room a later field on that grid may need.
    OtherGrid,
    Other(String),
    Damaged(String),
}

/// What was left out of a file, for its notices.
#[derive(Default)]
struct Skipped {
    /// Each reason, with how many fields it left out, in the order first met.
    reasons: Vec<(String, usize)>,
    /// Each reason a whole message was left out (another edition, damaged), with a count.
    messages: Vec<(String, usize)>,
    /// Each kind of damage that left a field out, with a count.
    damage: Vec<(String, usize)>,
    other_grid: usize,
    over_limit: usize,
    /// Fields of messages larger than the room left lets be read whole.
    too_long: usize,
    cut: usize,
    /// Bytes outside every message that aren't zeros.
    junk: u64,
}

impl Skipped {
    fn add(&mut self, why: String) {
        tally(&mut self.reasons, why);
    }

    fn message(&mut self, why: String) {
        tally(&mut self.messages, why);
    }

    fn damaged(&mut self, why: String) {
        tally(&mut self.damage, why);
    }

    fn notes(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = self.reasons.iter().map(|(r, n)| format!("{} left out: {r}, not read yet", count(*n))).collect();
        out.extend(self.damage.iter().map(|(r, n)| format!("{} left out: damaged ({r})", count(*n))));
        out.extend(self.messages.iter().map(|(r, n)| {
            let what = if *n == 1 { "1 message".to_string() } else { format!("{n} messages") };
            format!("{what} left out: {r}")
        }));
        if self.other_grid > 0 {
            out.push(format!("{} left out: on another grid than the first field's", count(self.other_grid)));
        }
        if self.over_limit > 0 {
            out.push(format!(
                "{} left out: the bands read hold the most this computer keeps in memory ({limit} values); split the file with a GRIB tool (wgrib2 or CDO) and add its parts",
                count(self.over_limit)
            ));
        }
        if self.too_long > 0 {
            out.push(format!(
                "{} left out: their message is larger than the memory left lets be read whole, even where one of its fields would fit; split the file with a GRIB tool (wgrib2 or CDO) and add its parts",
                count(self.too_long)
            ));
        }
        if self.cut > 0 {
            out.push("the file ends inside a message, which is left out".to_string());
        }
        if self.junk == 1 {
            out.push("1 byte outside its messages isn't GRIB, and is passed over".to_string());
        } else if self.junk > 1 {
            out.push(format!("{} bytes outside its messages aren't GRIB, and are passed over", self.junk));
        }
        out
    }
}

/// The most reasons of one kind said apart: past them (a file of messages of a thousand grid
/// templates), the rest are counted under [`OTHER_REASONS`], so the list and the time spent on it
/// stay small.
const MAX_REASONS: usize = 16;

const OTHER_REASONS: &str = "other kinds than those above";

/// One more of `why` in `list`.
fn tally(list: &mut Vec<(String, usize)>, why: String) {
    if let Some((_, n)) = list.iter_mut().find(|(r, _)| *r == why) {
        *n += 1;
    } else if list.len() < MAX_REASONS {
        list.push((why, 1));
    } else if let Some((_, n)) = list.iter_mut().find(|(r, _)| r == OTHER_REASONS) {
        *n += 1;
    } else {
        list.push((OTHER_REASONS.to_string(), 1));
    }
}

fn count(n: usize) -> String {
    if n == 1 { "1 field".to_string() } else { format!("{n} fields") }
}

/// A regular latitude/longitude grid, as the file gives it: degrees of the first and last points'
/// centres, the increments, and the scanning mode (WMO flag table 3.4).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Grid {
    ni: usize,
    nj: usize,
    lat1: f64,
    lon1: f64,
    lat2: f64,
    lon2: f64,
    dx: f64,
    dy: f64,
    scan: u8,
    /// The earth's shape when it isn't WGS 84's (or near enough): its description.
    earth: Option<&'static str>,
}

const SCAN_I_NEGATIVE: u8 = 0x80;
const SCAN_J_POSITIVE: u8 = 0x40;
const SCAN_J_CONSECUTIVE: u8 = 0x20;
const SCAN_ALTERNATE: u8 = 0x10;

impl Grid {
    /// Whether two grids put their points at the same places, whatever order they scan them in
    /// (each band is placed by its own grid): the same sizes, increments and north-west centre,
    /// after BAM's latitudes are laid out ([`Grid::north`]).
    fn same(&self, o: &Grid) -> bool {
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-6;
        self.ni == o.ni
            && self.nj == o.nj
            && close(self.north(), o.north())
            && close(self.west(), o.west())
            && close(self.dx, o.dx)
            && close(self.dy, o.dy)
    }

    fn goes_north(&self) -> bool {
        self.scan & SCAN_J_POSITIVE != 0
    }

    /// Whether the first and last latitudes run against the scanning mode (CPTEC's BAM files
    /// give −90° first and +90° last, the grid's edges, and scan north to south): rows then follow
    /// the scanning mode, centred between the two latitudes.
    fn latitudes_disagree(&self) -> bool {
        self.nj > 1 && self.lat1 != self.lat2 && (self.lat2 > self.lat1) != self.goes_north()
    }

    /// The centre latitude of the northernmost row.
    fn north(&self) -> f64 {
        if self.latitudes_disagree() {
            (self.lat1 + self.lat2) / 2.0 + (self.nj.saturating_sub(1)) as f64 * self.dy / 2.0
        } else if self.goes_north() {
            self.lat1 + (self.nj.saturating_sub(1)) as f64 * self.dy
        } else {
            self.lat1
        }
    }

    /// The centre longitude of the westernmost column, in −180..180 (a grid given from 0 to 360°
    /// east of 180° is moved west of the antimeridian, as GDAL moves one).
    fn west(&self) -> f64 {
        let w = if self.scan & SCAN_I_NEGATIVE != 0 { self.lon1 - (self.ni.saturating_sub(1)) as f64 * self.dx } else { self.lon1 };
        let w = w.rem_euclid(360.0);
        if w >= 180.0 { w - 360.0 } else { w }
    }

    /// Whether the columns go round the whole globe.
    fn whole_turn(&self) -> bool {
        (self.columns() as f64 * self.dx - 360.0).abs() <= turn_tolerance(self.columns(), self.dx)
    }

    /// Whether the last column is the first again, 360° on (a grid given from 0° to 360°
    /// inclusive): it is left out, and the grid is a whole turn.
    fn repeats_first_column(&self) -> bool {
        self.ni > 1 && ((self.ni - 1) as f64 * self.dx - 360.0).abs() <= turn_tolerance(self.ni - 1, self.dx)
    }

    /// The columns laid out: all of them, less a last one that repeats the first.
    fn columns(&self) -> usize {
        if self.repeats_first_column() { self.ni - 1 } else { self.ni }
    }

    /// The column and row (north up) of the `k`th value in scanning order.
    fn place(&self, k: usize) -> (usize, usize) {
        let (i, j) = if self.scan & SCAN_J_CONSECUTIVE != 0 { (k / self.nj, k % self.nj) } else { (k % self.ni, k / self.ni) };
        let alternate = self.scan & SCAN_ALTERNATE != 0;
        let (i, j) = if alternate && self.scan & SCAN_J_CONSECUTIVE == 0 && j % 2 == 1 {
            (self.ni - 1 - i, j)
        } else if alternate && self.scan & SCAN_J_CONSECUTIVE != 0 && i % 2 == 1 {
            (i, self.nj - 1 - j)
        } else {
            (i, j)
        };
        let col = if self.scan & SCAN_I_NEGATIVE != 0 { self.ni - 1 - i } else { i };
        let row = if self.goes_north() { self.nj - 1 - j } else { j };
        (col, row)
    }

    fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.latitudes_disagree() {
            let north = self.north();
            out.push(format!(
                "the grid gives its first row at {}° and its last at {}° but scans {}: its {} rows of {}° are laid out as the scanning mode says, centred between the two, from {}° to {}°",
                self.lat1,
                self.lat2,
                if self.goes_north() { "south to north" } else { "north to south" },
                self.nj,
                self.dy,
                round6(north),
                round6(north - (self.nj.saturating_sub(1)) as f64 * self.dy),
            ));
        }
        if self.repeats_first_column() {
            out.push(format!("its last column repeats the first, 360° on, and is left out: {} columns are read", self.ni - 1));
        }
        if let Some(earth) = self.earth {
            out.push(format!("its earth is {earth}; latitudes and longitudes are read as WGS 84's"));
        }
        out
    }
}

/// How far `n` steps of a grid's increment may come from 360° and still be a whole turn: half a
/// millidegree a step, as GRIB1 rounds increments to millidegrees (GRIB2's are finer). A grid whose
/// increment doesn't divide 360° (seven columns of 50°) isn't one. Never half a step or more: a
/// grid a whole step short of 360° (1440 columns of 0.25° from 0° to 359.75°) isn't one either.
fn turn_tolerance(n: usize, dx: f64) -> f64 {
    (n as f64 * 0.0005 + 1e-9).min(dx * 0.5)
}

/// `v` to a millionth, for a notice (89.91, not 89.91000000000001).
fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// One field: its band name, grid and values in scanning order (NaN where a bitmap masks).
struct Field {
    name: String,
    grid: Grid,
    values: Vec<f32>,
    /// Values `f32` changed: decimals (`D` > 0) and sums with more digits than it holds.
    rounded: u64,
    /// Values past `f32`'s range, which have no value.
    past: u64,
}

/// The raster of fields on one grid: north up, a whole turn of longitude from −180°. Cancel is
/// looked at before each band is placed; one band's placement (at most `max_samples` values, one
/// pass) runs without a check, and so does the statistics pass `Raster::new` makes over the
/// bands.
fn to_raster(fields: Vec<Field>, name: &str) -> Result<Raster> {
    let Some(grid) = fields.first().map(|f| f.grid) else { return Err(bad("no field")) };
    let (w, h) = (grid.columns(), grid.nj);
    let west = grid.west();
    // A whole turn starting east of −180°: the column at 180° moves to the first, so the columns
    // run from −180°.
    let roll = if grid.whole_turn() && west > -180.0 + grid.dx * 0.5 {
        let at_180 = ((180.0 - west) / grid.dx).round() as usize % w.max(1);
        (w - at_180) % w.max(1)
    } else {
        0
    };
    let origin_x = if roll > 0 { west - (roll as f64) * grid.dx } else { west } - grid.dx / 2.0;
    let origin_y = grid.north() + grid.dy / 2.0;
    let mut names: Vec<String> = Vec::with_capacity(fields.len());
    // Each name's first band: a field named as one before it (the same message twice, a GRIB1
    // ensemble's members, octets a name doesn't read) is named by its band number too.
    let mut taken = std::collections::HashSet::with_capacity(fields.len());
    let mut rounded = Vec::with_capacity(fields.len());
    let mut past = 0u64;
    let mut bands = Vec::with_capacity(fields.len());
    for f in fields {
        crate::progress::check()?;
        let mut band = super::filled(w, h, f32::NAN)?;
        // Each band by its own grid: the same places as the first's, maybe in another order.
        for (k, v) in f.values.iter().enumerate() {
            let (col, row) = f.grid.place(k);
            // A repeated first column, 360° on, is the last (west to east); it isn't laid out.
            if col >= w {
                continue;
            }
            let col = (col + roll) % w.max(1);
            if let Some(cell) = band.get_mut(row * w + col) {
                *cell = *v;
            }
        }
        let name = if taken.contains(&f.name) { format!("{}:band={}", f.name, names.len() + 1) } else { f.name };
        taken.insert(name.clone());
        names.push(name);
        rounded.push(f.rounded);
        past += f.past;
        bands.push(band);
    }
    let size = |n: usize| u32::try_from(n).map_err(|_| bad("the grid is too large"));
    let transform = GeoTransform { origin_x, origin_y, pixel_width: grid.dx, pixel_height: -grid.dy };
    let mut r = Raster::new(name, size(w)?, size(h)?, bands, transform, Crs::LonLat, None, Default::default(), "Float32")?;
    for (b, n) in r.bands.iter_mut().zip(names) {
        b.name = n;
    }
    if rounded.iter().any(|n| *n > 0) {
        r.rounded = rounded;
    }
    if past > 0 {
        let (what, has) = if past == 1 { ("1 value".to_string(), "has") } else { (format!("{past} values"), "have") };
        r.lost.push(format!("{what} past the range of 32-bit floats (±3.4 × 10^38) {has} no value"));
    }
    Ok(r)
}

/// Octets `o..o + n` (0-based offset `o`) of `b`, unsigned, big-endian; 0 past the end.
fn be(b: &[u8], o: usize, n: usize) -> u32 {
    b.get(o..o + n).map_or(0, |s| s.iter().fold(0u32, |v, &x| (v << 8) | u32::from(x)))
}

/// Octets `o..o + n` of `b`, a sign bit and magnitude (GRIB's signed integers).
fn signed(b: &[u8], o: usize, n: usize) -> i64 {
    let v = i64::from(be(b, o, n));
    let sign = 1i64 << (8 * n - 1);
    if v & sign != 0 { -(v & (sign - 1)) } else { v }
}

/// An IBM System/360 single-precision number (GRIB1's reference value).
fn ibm(v: u32) -> f64 {
    let sign = if v & 0x8000_0000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((v >> 24) & 0x7f) as i32 - 64;
    let mantissa = f64::from(v & 0x00ff_ffff) / f64::from(1u32 << 24);
    sign * mantissa * 16f64.powi(exponent)
}

/// The `points` values of a field in scanning order, written straight into one vector: the
/// `count` values `next` gives (in `f64`), held as `f32`, over the points a bitmap keeps (NaN at
/// the others), or all of them without one. Also how many of them `f32` rounded, and how many
/// are past its range (about ±3.4 × 10^38, `f64`'s too): those have no value (NaN) rather than
/// an infinity. A sidecar's no-data value isn't counted either way ([`Uncounted`]'s rule), and
/// neither is a point of a repeated first column (`grid`'s, which [`to_raster`] doesn't lay out).
/// The values are never held apart from the field, so it costs 4 bytes a point.
fn field_values(
    (points, grid): (usize, &Grid),
    mask: Option<&[u8]>,
    count: usize,
    uncounted: Uncounted,
    mut next: impl FnMut() -> f64,
) -> std::result::Result<(Vec<f32>, u64, u64), Skip> {
    match mask {
        None if count != points => return Err(Skip::Damaged("a field whose values don't fill its grid".to_string())),
        Some(bits) if (bits.len() as u64) * 8 < points as u64 => {
            return Err(Skip::Damaged("a bitmap shorter than its grid".to_string()));
        }
        _ => {}
    }
    let mut out = Vec::new();
    out.try_reserve_exact(points).map_err(|_| Skip::TooLarge)?;
    let (mut rounded, mut past, mut left) = (0u64, 0u64, count);
    let (repeats, columns) = (grid.repeats_first_column(), grid.columns());
    for k in 0..points {
        let kept = mask.is_none_or(|bits| bits.get(k / 8).is_some_and(|b| b & (0x80 >> (k % 8)) != 0));
        if kept && left > 0 {
            left -= 1;
            let wide = next();
            let v = wide as f32;
            let laid = u64::from(!repeats || grid.place(k).0 < columns);
            match uncounted.decides(v, || Some(Exact::Float(wide))) {
                // The no-data value itself (masked when the sidecar is applied).
                Some(true) => out.push(if v.is_finite() { v } else { f32::NAN }),
                _ if !v.is_finite() => {
                    past += laid;
                    out.push(f32::NAN);
                }
                // A value that became the no-data value.
                Some(false) => {
                    rounded += laid;
                    out.push(v);
                }
                None => {
                    rounded += laid * u64::from(f64::from(v) != wide);
                    out.push(v);
                }
            }
        } else {
            out.push(f32::NAN);
        }
    }
    Ok((out, rounded, past))
}

/// A field's `count` values of `bits` bits each from `data`, big-endian, unpacked as
/// `(R + X × 2^E) / 10^D` in `f64` and held as `f32`, over the `points` of its grid
/// ([`field_values`]); with fewer bits there than that, an error.
fn unpack(
    data: &[u8],
    (count, bits): (usize, u32),
    (r, e, d): (f64, i64, i64),
    mask: Option<&[u8]>,
    at: (usize, &Grid),
    uncounted: Uncounted,
) -> std::result::Result<(Vec<f32>, u64, u64), Skip> {
    if bits > MAX_BITS {
        return Err(Skip::Other(format!("values of {bits} bits")));
    }
    let need = (count as u64).saturating_mul(u64::from(bits)).div_ceil(8);
    if (data.len() as u64) < need {
        return Err(Skip::Damaged("a data section shorter than its values".to_string()));
    }
    reference(r)?;
    // 2^E and 10^D, where `f64` holds them. Past that (no producer writes such an E or D, but
    // they are kept), `R + X × 2^E` is divided by [`scaled_by`] where `f64` holds it (so a sum of
    // 0 stays 0); where it is past `f64` but the division brings it back, the value is made as
    // `R / 10^D + X × (2^E / 10^D)`, each by [`scaled_by`]. A value past `f64` either way is past
    // `f32`'s range.
    let two = (-1000..=1000).contains(&e).then(|| 2f64.powi(e as i32));
    let ten = decimal(d);
    let (base, step) = (scaled_by(r, 0, d), scaled_by(1.0, e, d));
    let mut bit = 0u64;
    field_values(at, mask, count, uncounted, || {
        // The 8 octets from the value's first: at most 7 bits before it, and its 32 at most.
        let byte = (bit / 8) as usize;
        let word = (0..8).fold(0u64, |w, i| (w << 8) | u64::from(data.get(byte + i).copied().unwrap_or(0)));
        let x = ((word << (bit % 8)) >> (64 - bits)) as f64;
        bit += u64::from(bits);
        // X = 0 is R whatever E is (never 0 × ∞).
        let sum = if x == 0.0 { r } else { two.map_or(f64::INFINITY, |s| r + x * s) };
        let wide = ten.map_or(f64::INFINITY, |ten| sum / ten);
        // A sum that isn't 0 divided down to 0 is made by `scaled_by`, so it is counted as
        // rounded, never read as an exact 0.
        if wide.is_finite() && (wide != 0.0 || sum == 0.0) {
            wide
        } else if sum.is_finite() {
            scaled_by(sum, 0, d)
        } else {
            base + x * step
        }
    })
}

/// 10^`d`, the decimal scale factor's divisor, where `f64` holds it at full precision.
fn decimal(d: i64) -> Option<f64> {
    let ten = 10f64.powi(d.clamp(-1000, 1000) as i32);
    ten.is_normal().then_some(ten)
}

/// `v` × 2^`e` / 10^`d`, by steps of at most 2^±500 and 10^±150, each taken toward 1 while one
/// that way is left: the result is past `f64` only when the value is. A value too small for
/// `f64` that isn't 0 is kept as its smallest, so it is counted as rounded to 0 in `f32`, never
/// read as an exact 0.
fn scaled_by(mut v: f64, mut e: i64, mut d: i64) -> f64 {
    let sign = v.signum();
    let zero = v == 0.0;
    while (e != 0 || d != 0) && v != 0.0 && v.is_finite() {
        // Toward 1: down by 2^-n (e < 0) or 10^-n (d > 0) from above 1, up from below.
        let down = v.abs() > 1.0;
        let two = if down { e < 0 } else { e > 0 };
        let ten = if down { d > 0 } else { d < 0 };
        if two || (!ten && e != 0) {
            let s = e.clamp(-500, 500);
            v *= 2f64.powi(s as i32);
            e -= s;
        } else {
            let s = d.clamp(-150, 150);
            v /= 10f64.powi(s as i32);
            d -= s;
        }
    }
    if v == 0.0 && !zero { sign * f64::from_bits(1) } else { v }
}

/// A reference value that is a number: GRIB2's IEEE bits may give an infinity or NaN, which no
/// value can be made from.
fn reference(r: f64) -> std::result::Result<(), Skip> {
    if r.is_finite() { Ok(()) } else { Err(Skip::Damaged("a reference value that isn't a number".to_string())) }
}

/// A field packed in no bits: its reference value `R / 10^D` at every point a bitmap keeps
/// ([`field_values`]); past `f32`'s range, the points have no value and are said.
fn constant(
    (r, d): (f64, i64),
    count: usize,
    mask: Option<&[u8]>,
    at: (usize, &Grid),
    uncounted: Uncounted,
) -> std::result::Result<(Vec<f32>, u64, u64), Skip> {
    reference(r)?;
    // As in `unpack`: an `R` that isn't 0 divided down to 0 is counted as rounded.
    let v = decimal(d).map(|ten| r / ten).filter(|v| *v != 0.0 || r == 0.0).unwrap_or_else(|| scaled_by(r, 0, d));
    field_values(at, mask, count, uncounted, || v)
}

/// `n` bitmap bits set among the first `points`.
fn kept(bits: &[u8], points: usize) -> usize {
    (0..points).filter(|k| bits.get(k / 8).is_some_and(|b| b & (0x80 >> (k % 8)) != 0)).count()
}

/// The reference time, `d=YYYYMMDDHH` as wgrib's inventories write it (with the century).
fn date(year: i64, month: u32, day: u32, hour: u32) -> String {
    format!("d={year:04}{month:02}{day:02}{hour:02}")
}

/// A section of `b` at `at`: its bytes, by the 3-octet length GRIB1 sections start with.
fn section1(b: &[u8], at: usize) -> std::result::Result<&[u8], Skip> {
    let len = be(b, at, 3) as usize;
    if len < 3 {
        return Err(Skip::Damaged("a damaged section".to_string()));
    }
    b.get(at..at + len).ok_or_else(|| Skip::Damaged("a section past the end of its message".to_string()))
}

/// A GRIB1 message (WMO FM 92 GRIB edition 1): its one field.
fn grib1(b: &[u8], room: usize, first: Option<Grid>, uncounted: Uncounted) -> std::result::Result<Field, Skip> {
    let pds = section1(b, 8)?;
    if pds.len() < 28 {
        return Err(Skip::Damaged("a damaged product definition section".to_string()));
    }
    let flags = pds[7];
    if flags & 0x80 == 0 {
        return Err(Skip::Other(format!("predefined grids (grid {})", pds[6])));
    }
    let d = signed(pds, 26, 2);
    let century = i64::from(pds[24]);
    let year = (century - 1) * 100 + i64::from(pds[12]);
    let name = format!(
        "{}:kpds5={}:kpds6={}:kpds7={}:TR={}:P1={}:P2={}:TimeU={}",
        date(year, u32::from(pds[13]), u32::from(pds[14]), u32::from(pds[15])),
        pds[8],
        pds[9],
        be(pds, 10, 2),
        pds[20],
        pds[18],
        pds[19],
        pds[17],
    );
    let gds_at = 8 + pds.len();
    let gds = section1(b, gds_at)?;
    let grid = grib1_grid(gds)?;
    if first.is_some_and(|g| !g.same(&grid)) {
        return Err(Skip::OtherGrid);
    }
    let points = grid.ni.checked_mul(grid.nj).filter(|n| *n <= room).ok_or(Skip::TooLarge)?;
    let mut at = gds_at + gds.len();
    let bitmap = if flags & 0x40 != 0 {
        let bms = section1(b, at)?;
        // Its length, unused bits and table reference come before the bits.
        if bms.len() < 6 {
            return Err(Skip::Damaged("a damaged bitmap section".to_string()));
        }
        if be(bms, 4, 2) != 0 {
            return Err(Skip::Other("predefined bitmaps".to_string()));
        }
        at += bms.len();
        Some(bms.get(6..).unwrap_or_default())
    } else {
        None
    };
    let bds = section1(b, at)?;
    // Its flags, E, R and the bits a value come before the values (none, for a constant field).
    if bds.len() < 11 {
        return Err(Skip::Damaged("a damaged binary data section".to_string()));
    }
    let bds_flags = bds[3];
    if bds_flags & 0x80 != 0 {
        return Err(Skip::Other("spherical harmonics".to_string()));
    }
    if bds_flags & 0x40 != 0 {
        return Err(Skip::Other("GRIB1 second-order (complex) packing".to_string()));
    }
    let e = signed(bds, 4, 2);
    let r = ibm(be(bds, 6, 4));
    let bits = u32::from(bds[10]);
    let count = bitmap.map_or(points, |m| kept(m, points));
    let data = bds.get(11..).unwrap_or_default();
    let (values, rounded, past) = if bits == 0 {
        constant((r, d), count, bitmap, (points, &grid), uncounted)?
    } else {
        unpack(data, (count, bits), (r, e, d), bitmap, (points, &grid), uncounted)?
    };
    Ok(Field { name, grid, values, rounded, past })
}

/// A GRIB1 grid description section: a regular latitude/longitude grid only.
fn grib1_grid(g: &[u8]) -> std::result::Result<Grid, Skip> {
    if g.len() < 28 {
        return Err(Skip::Damaged("a damaged grid description section".to_string()));
    }
    let kind = g[5];
    if kind != 0 {
        return Err(Skip::Other(format!("GRIB1 grids of data representation type {kind} (not latitude/longitude)")));
    }
    let (ni, nj) = (be(g, 6, 2) as usize, be(g, 8, 2) as usize);
    // A missing row or column length: a quasi-regular grid.
    if ni == 0xffff || nj == 0xffff || ni == 0 || nj == 0 {
        return Err(Skip::Other("quasi-regular grids".to_string()));
    }
    let deg = |o: usize| signed(g, o, 3) as f64 / 1000.0;
    let (lat1, lon1, lat2, lon2) = (deg(10), deg(13), deg(17), deg(20));
    let res = g[16];
    let scan = g[27];
    let (di, dj) = (be(g, 23, 2), be(g, 25, 2));
    let given = res & 0x80 != 0;
    let dx = if given && di != 0xffff { f64::from(di) / 1000.0 } else { spacing(lon_span(lon1, lon2, scan), ni) };
    let dy = if given && dj != 0xffff { f64::from(dj) / 1000.0 } else { spacing((lat2 - lat1).abs(), nj) };
    let earth =
        Some(if res & 0x40 != 0 { "the IAU 1965 spheroid (GRIB1's oblate earth)" } else { "a sphere of radius 6,367,470 m (GRIB1's)" });
    checked(Grid { ni, nj, lat1, lon1, lat2, lon2, dx, dy, scan, earth })
}

/// The distance from the first longitude to the last in the direction the grid scans its columns
/// (east, or west when the scanning mode says), wrapping round the globe. The same longitude first
/// and last is a whole turn: a regular grid of more than one column that ends where it starts has
/// its last column on its first, 360° on ([`Grid::repeats_first_column`], said and left out).
fn lon_span(first: f64, last: f64, scan: u8) -> f64 {
    let d = (if scan & SCAN_I_NEGATIVE != 0 { first - last } else { last - first }).rem_euclid(360.0);
    if d == 0.0 { 360.0 } else { d }
}

/// An increment the file leaves out: the span from the first point to the last over the steps
/// between them. A grid one point across has no steps, and the file doesn't say how wide its
/// point is: NaN, which [`checked`] refuses by name.
fn spacing(span: f64, points: usize) -> f64 {
    if points > 1 { span / (points - 1) as f64 } else { f64::NAN }
}

/// A grid whose increments and corners are usable, or why it isn't.
fn checked(g: Grid) -> std::result::Result<Grid, Skip> {
    // Flag table 3.4's bits 5 to 8: rows offset by half a cell, or one point short; GRIB1 leaves
    // them 0. Placing such a grid's points on one grid of cells would move them.
    if g.scan & 0x0f != 0 {
        return Err(Skip::Other("grids whose rows are offset or shortened (scanning mode bits 5 to 8)".to_string()));
    }
    if g.dx.is_nan() || g.dy.is_nan() {
        return Err(Skip::Other("grids one point across that don't give their spacing".to_string()));
    }
    let ok = g.dx.is_finite()
        && g.dy.is_finite()
        && g.dx > 0.0
        && g.dy > 0.0
        && g.lat1.abs() <= 90.0
        && g.lat2.abs() <= 90.0
        && g.ni as f64 * g.dx <= 360.0 + g.dx
        && g.nj as f64 * g.dy <= 180.0 + g.dy;
    let north = g.north();
    let south = north - (g.nj.saturating_sub(1)) as f64 * g.dy;
    if ok && north <= 90.0 + 1e-6 && south >= -90.0 - 1e-6 {
        Ok(g)
    } else {
        Err(Skip::Damaged("a latitude/longitude grid that isn't on the globe".to_string()))
    }
}

/// A GRIB2 section at `at`: its number and bytes.
fn section2(b: &[u8], at: usize) -> Option<(u8, &[u8])> {
    let len = be(b, at, 4) as usize;
    let s = b.get(at..at.checked_add(len)?)?;
    (len >= 5).then(|| (s[4], s))
}

/// A GRIB2 message (WMO FM 92 GRIB edition 2): each of its fields. A grid (section 3) may serve
/// the fields after it, but each field has its own sections 4 to 7 (WMO's regulation 92.1.3): a
/// field without its own product, packing or bitmap section is damaged, never read with the one
/// before it, and so are sections 4 to 6 without a section 7 after them, whether the message ends
/// on them or another field's sections follow (never read with that field's). `first`: the
/// grid of the fields kept, if any; a field on another is left out before it is decoded.
fn grib2(b: &[u8], room: usize, mut first: Option<Grid>, uncounted: Uncounted) -> Vec<std::result::Result<Field, Skip>> {
    let mut out = Vec::new();
    let discipline = b.get(6).copied().unwrap_or(255);
    let mut at = 16usize;
    let mut reference = String::from("d=?");
    let mut grid: Option<std::result::Result<Grid, Skip>> = None;
    let mut product: Option<String> = None;
    let mut packing: Option<std::result::Result<(usize, f64, i64, i64, u32), Skip>> = None;
    let mut bitmap: Option<std::result::Result<Option<&[u8]>, Skip>> = None;
    let mut previous_bitmap: Option<&[u8]> = None;
    let mut held = 0usize;
    let mut sections = 0usize;
    // Whether the sections stop at damage (said already) rather than at the message's end.
    let mut broken = false;
    while b.get(at..at + 4).is_some_and(|m| m != b"7777") {
        sections += 1;
        if sections > MAX_SECTIONS {
            out.push(Err(Skip::Damaged(format!("a message of more than {MAX_SECTIONS} sections"))));
            broken = true;
            break;
        }
        let Some((number, s)) = section2(b, at) else {
            out.push(Err(Skip::Damaged("a damaged GRIB2 section".to_string())));
            broken = true;
            break;
        };
        at += s.len();
        // A field's product, packing or bitmap given again before its data section: the field
        // before ends without one, and gives nothing to the field after it.
        let again = match number {
            4 => product.is_some() || packing.is_some() || bitmap.is_some(),
            5 => packing.is_some() || bitmap.is_some(),
            6 => bitmap.is_some(),
            _ => false,
        };
        if again {
            out.push(Err(Skip::Damaged("a field without its data section".to_string())));
            (product, packing, bitmap) = (None, None, None);
        }
        match number {
            1 => {
                reference = date(
                    i64::from(be(s, 12, 2)),
                    u32::from(s.get(14).copied().unwrap_or(0)),
                    u32::from(s.get(15).copied().unwrap_or(0)),
                    u32::from(s.get(16).copied().unwrap_or(0)),
                );
            }
            3 => grid = Some(grib2_grid(s)),
            4 => product = Some(grib2_product(s)),
            5 => packing = Some(grib2_packing(s)),
            6 => {
                bitmap = Some(match s.get(5).copied() {
                    None => Err(Skip::Damaged("a damaged bitmap section".to_string())),
                    Some(255) => Ok(None),
                    Some(0) => {
                        let bits = s.get(6..).unwrap_or_default();
                        previous_bitmap = Some(bits);
                        Ok(Some(bits))
                    }
                    Some(254) => {
                        previous_bitmap.map(Some).ok_or_else(|| Skip::Damaged("a bitmap that refers to none before it".to_string()))
                    }
                    Some(_) => Err(Skip::Other("predefined bitmaps".to_string())),
                });
            }
            7 => {
                // This field's sections 4 to 6, taken: the next field gives its own.
                let (product, packing, bitmap) = (product.take(), packing.take(), bitmap.take());
                let field = (|| {
                    let grid = grid.clone().ok_or_else(|| Skip::Damaged("a field without a grid".to_string()))??;
                    let product = product.ok_or_else(|| Skip::Damaged("a field without its product definition".to_string()))?;
                    let (count, r, e, d, bits) = packing.ok_or_else(|| Skip::Damaged("a field without its packing".to_string()))??;
                    let mask = bitmap.ok_or_else(|| Skip::Damaged("a field without its bitmap section".to_string()))??;
                    if first.is_some_and(|g| !g.same(&grid)) {
                        return Err(Skip::OtherGrid);
                    }
                    let points = grid.ni.checked_mul(grid.nj).filter(|n| held.saturating_add(*n) <= room).ok_or(Skip::TooLarge)?;
                    if mask.is_some_and(|m| kept(m, points) != count) || (mask.is_none() && count != points) {
                        return Err(Skip::Damaged("a field whose values don't fill its grid".to_string()));
                    }
                    let (values, rounded, past) = if bits == 0 {
                        constant((r, d), count, mask, (points, &grid), uncounted)?
                    } else {
                        unpack(s.get(5..).unwrap_or_default(), (count, bits), (r, e, d), mask, (points, &grid), uncounted)?
                    };
                    held += points;
                    first.get_or_insert(grid);
                    Ok(Field { name: format!("{reference}:discipline={discipline}:{product}"), grid, values, rounded, past })
                })();
                out.push(field);
            }
            _ => {}
        }
    }
    // A field's sections the message ends on, without its data section.
    if !broken && (product.is_some() || packing.is_some() || bitmap.is_some()) {
        out.push(Err(Skip::Damaged("a field without its data section".to_string())));
    }
    if out.is_empty() {
        out.push(Err(Skip::Damaged("a GRIB2 message without a data section".to_string())));
    }
    out
}

/// A GRIB2 grid definition section: template 3.0 (regular latitude/longitude) only.
fn grib2_grid(s: &[u8]) -> std::result::Result<Grid, Skip> {
    if s.get(5).copied().unwrap_or(1) != 0 {
        return Err(Skip::Other("predefined grids".to_string()));
    }
    if s.get(10).copied().unwrap_or(0) != 0 {
        return Err(Skip::Other("quasi-regular grids".to_string()));
    }
    let template = be(s, 12, 2);
    if template != 0 {
        return Err(Skip::Other(format!("GRIB2 grid template 3.{template} (not regular latitude/longitude)")));
    }
    if s.len() < 72 {
        return Err(Skip::Damaged("a damaged grid definition section".to_string()));
    }
    let earth = match s[14] {
        // GRS 80, WGS 84: near enough.
        4 | 5 => None,
        0 => Some("a sphere of radius 6,367,470 m"),
        6 => Some("a sphere of radius 6,371,229 m"),
        8 => Some("a sphere of radius 6,371,200 m"),
        1 => Some("a sphere of a radius the file gives"),
        2 => Some("the IAU 1965 spheroid"),
        _ => Some("a spheroid the file gives"),
    };
    let (ni, nj) = (be(s, 30, 4) as usize, be(s, 34, 4) as usize);
    if ni == 0 || nj == 0 || ni == u32::MAX as usize || nj == u32::MAX as usize {
        return Err(Skip::Other("quasi-regular grids".to_string()));
    }
    let (basic, sub) = (be(s, 38, 4), be(s, 42, 4));
    let unit = if basic == 0 || basic == u32::MAX {
        1e-6
    } else if sub == 0 || sub == u32::MAX {
        return Err(Skip::Damaged("a grid with no angle subdivisions".to_string()));
    } else {
        f64::from(basic) / f64::from(sub)
    };
    let deg = |o: usize| signed(s, o, 4) as f64 * unit;
    let (lat1, lon1, lat2, lon2) = (deg(46), deg(50), deg(55), deg(59));
    // Code table 3.3: bit 3 (0x20) says the i direction's increments are given, bit 4 (0x10)
    // the j direction's.
    let res = s[54];
    let scan = s[71];
    let (di, dj) = (be(s, 63, 4), be(s, 67, 4));
    let dx = if res & 0x20 != 0 && di != u32::MAX { f64::from(di) * unit } else { spacing(lon_span(lon1, lon2, scan), ni) };
    let dy = if res & 0x10 != 0 && dj != u32::MAX { f64::from(dj) * unit } else { spacing((lat2 - lat1).abs(), nj) };
    checked(Grid { ni, nj, lat1, lon1, lat2, lon2, dx, dy, scan, earth })
}

/// A GRIB2 product definition section, as part of a band name: the parameter's category and
/// number (its discipline is section 0's), the template, and for templates 4.0 to 4.15 the first
/// fixed surface, the forecast time and, for a layer between two surfaces, the second. What tells
/// the fields of one template apart follows: an ensemble's member, a derived forecast's kind, a
/// probability's limits, a percentile, and a statistic's process and period.
fn grib2_product(s: &[u8]) -> String {
    let template = be(s, 7, 2);
    let (category, number) = (s.get(9).copied().unwrap_or(255), s.get(10).copied().unwrap_or(255));
    let mut name = format!("category={category}:number={number}:template=4.{template}");
    // Templates 4.0 to 4.15 share these octets; a surface's scaled value is unsigned.
    if template <= 15 && s.len() >= 28 {
        name.push_str(&format!(":surface={}:value={}:TimeU={}:forecast={}", s[22], scaled(s, 23, false), s[17], be(s, 18, 4)));
        // The second fixed surface (255: none).
        if let Some(&second) = s.get(28).filter(|t| **t != 255 && s.len() >= 34) {
            name.push_str(&format!(":surface2={second}:value2={}", scaled(s, 29, false)));
        }
    }
    // An ensemble's member (templates 4.1 and 4.11): its type and perturbation number.
    if matches!(template, 1 | 11) && s.len() >= 37 {
        name.push_str(&format!(":ensemble={}:member={}", s[34], s[35]));
    }
    // A derived forecast (4.2, 4.12): its kind (code table 4.7).
    if matches!(template, 2 | 12) && s.len() >= 35 {
        name.push_str(&format!(":derived={}", s[34]));
    }
    // A probability (4.5, 4.9): its type (code table 4.9) and limits, whose scaled values are
    // signed.
    if matches!(template, 5 | 9) && s.len() >= 47 {
        name.push_str(&format!(":probability={}:lower={}:upper={}", s[36], scaled(s, 37, true), scaled(s, 42, true)));
    }
    // A percentile (4.6, 4.10).
    if matches!(template, 6 | 10) && s.len() >= 35 {
        name.push_str(&format!(":percentile={}", s[34]));
    }
    // A statistic over a period (4.8 to 4.12): its first time range's process (code table 4.10),
    // time unit and length, after the period's end, how many ranges and the values missing. The
    // period starts at the forecast time, so two accumulations from one forecast time over
    // different periods are named apart.
    let period = match template {
        8 => Some(34),
        9 => Some(47),
        10 => Some(35),
        11 => Some(37),
        12 => Some(36),
        _ => None,
    };
    if let Some(at) = period.filter(|at| s.len() >= at + 19) {
        name.push_str(&format!(":statistic={}:rangeU={}:range={}", s[at + 12], s[at + 14], be(s, at + 15, 4)));
    }
    name
}

/// The value at octets `o + 1..o + 5` scaled by 10^-(the signed scale factor at octet `o`), as
/// part of a band name: `missing` when either is all ones (GRIB2's missing value). `value_signed`:
/// whether the scaled value is signed (a probability's limits) or not (a fixed surface's, as
/// ecCodes reads them).
fn scaled(s: &[u8], o: usize, value_signed: bool) -> String {
    let raw = be(s, o + 1, 4);
    if s.get(o) == Some(&0xff) || raw == u32::MAX {
        return "missing".to_string();
    }
    let v = if value_signed { signed(s, o + 1, 4) as f64 } else { f64::from(raw) };
    round6(v / 10f64.powi(signed(s, o, 1) as i32)).to_string()
}

/// A GRIB2 data representation section: template 5.0 (simple packing) only, as the number of
/// values, R, E, D and the bits a value.
fn grib2_packing(s: &[u8]) -> std::result::Result<(usize, f64, i64, i64, u32), Skip> {
    let template = be(s, 9, 2);
    if template != 0 {
        let kind = match template {
            2 | 3 => "complex packing",
            4 => "IEEE floating point",
            40 => "JPEG 2000",
            41 => "PNG",
            42 => "CCSDS",
            _ => "a packing",
        };
        return Err(Skip::Other(format!("GRIB2 {kind} (template 5.{template})")));
    }
    if s.len() < 21 {
        return Err(Skip::Damaged("a damaged data representation section".to_string()));
    }
    let r = f64::from(f32::from_bits(be(s, 11, 4)));
    Ok((be(s, 5, 4) as usize, r, signed(s, 15, 2), signed(s, 17, 2), u32::from(s[19])))
}

#[cfg(test)]
#[path = "grib_tests.rs"]
mod tests;
