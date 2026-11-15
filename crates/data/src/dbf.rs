//! Shared Shapefile character encoding selection for whole-file and streamed reads.

use encoding_rs as rs;
use shapefile::dbase::{CodePageMark, UnicodeLossy, encoding::DynEncoding};

const MAX_CPG_BYTES: usize = 1024;

/// Keep the selection thread-safe; dbase's dynamic decoder is created per read because it
/// is `Send` but not `Sync`, whereas feature sources can be shared between readers.
pub(crate) enum DbfEncoding {
    Standard(&'static rs::Encoding),
    Oem(CodePageMark),
    Utf8,
}

impl DbfEncoding {
    /// Create a per-read decoder, preserving leading U+FEFF and replacing malformed
    /// bytes for UTF-8 field names and values regardless of the metadata source.
    pub(crate) fn decoder(&self) -> DynEncoding {
        match self {
            // DBF fields are not standalone files: preserve a leading U+FEFF as
            // content instead of letting EncodingRs strip it as a UTF-8 signature.
            Self::Standard(encoding) if *encoding == rs::UTF_8 => UnicodeLossy.into(),
            Self::Standard(encoding) => shapefile::dbase::encoding::EncodingRs::from(*encoding).into(),
            Self::Oem(mark) => mark.to_encoding().unwrap_or_else(|| UnicodeLossy.into()),
            Self::Utf8 => UnicodeLossy.into(),
        }
    }

    fn from_number(number: u16) -> Option<Self> {
        // DOS 437, 850, 852, 861 and 865 go to dbase's OEM adapter; DOS 866 and the other pages
        // decoded here keep encoding_rs, because dbase's yore adapter is strict for those and can
        // discard records containing malformed text. Any other page isn't decoded (`None`).
        let encoding = match number {
            437 => return Some(Self::Oem(CodePageMark::CP437)),
            850 => return Some(Self::Oem(CodePageMark::CP850)),
            852 => return Some(Self::Oem(CodePageMark::CP852)),
            861 => return Some(Self::Oem(CodePageMark::CP861)),
            865 => return Some(Self::Oem(CodePageMark::CP865)),
            866 => rs::IBM866,
            874 => rs::WINDOWS_874,
            932 => rs::SHIFT_JIS,
            936 => rs::GBK,
            949 => rs::EUC_KR,
            950 => rs::BIG5,
            10000 => rs::MACINTOSH,
            // WHATWG's x-mac-cyrillic is Mac Ukrainian (10017): the same Cyrillic letters as
            // Mac Cyrillic, and the nearest encoding there is.
            10007 => rs::X_MAC_CYRILLIC,
            1250..=1258 => return Self::from_label(&format!("windows-{number}")),
            65001 => rs::UTF_8,
            _ => return None,
        };
        Some(Self::Standard(encoding))
    }

    fn from_label(label: &str) -> Option<Self> {
        // dbase's name lookup with both decoder features enabled falls back to CP1252
        // for unknown labels. Resolve explicitly so unusable metadata is reported.
        let encoding = rs::Encoding::for_label(label.as_bytes())?;
        // UTF-16 and replacement encodings cannot represent DBF's ASCII padding and
        // NUL-terminated field names. Treat these labels as unsupported metadata.
        encoding.is_ascii_compatible().then_some(Self::Standard(encoding))
    }

    fn from_name(name: &str) -> Option<Self> {
        let upper = name.to_ascii_uppercase();
        if let Some(suffix) = upper.strip_prefix("8859") {
            return Self::from_label(&format!("iso-8859-{}", suffix.strip_prefix('-').unwrap_or(suffix)));
        }
        let prefixed = ["CP", "IBM", "OEM", "WINDOWS", "ANSI"].into_iter().find_map(|prefix| upper.strip_prefix(prefix));
        let number = match prefixed {
            Some(suffix) => suffix.strip_prefix('-').unwrap_or(suffix).trim(),
            None => &upper,
        }
        .parse::<u16>();
        match number {
            Ok(number) => Self::from_number(number),
            Err(_) => Self::from_label(&upper),
        }
    }
}

/// The code page a `.dbf`'s language driver ID (byte 29) names: the language driver table as GDAL's
/// Shapefile driver reads it (#1079), with dbase's own entries. `0x57`, which GDAL reads as
/// ISO-8859-1, is Windows-1252, its superset, as the shapefile code page documentation has it.
/// `None` for an ID no table names.
fn ldid_code_page(ldid: u8) -> Option<u16> {
    Some(match ldid {
        0x01 | 0x09 | 0x0B | 0x0D | 0x0F | 0x11 | 0x15 | 0x18 | 0x19 | 0x1B => 437,
        0x02 | 0x0A | 0x0E | 0x10 | 0x12 | 0x14 | 0x16 | 0x1A | 0x1D | 0x25 | 0x37 => 850,
        0x03 | 0x57 | 0x58 | 0x59 => 1252,
        0x04 => 10000,
        0x08 | 0x17 | 0x66 => 865,
        0x13 | 0x7B => 932,
        0x1C | 0x6C => 863,
        0x1F | 0x22 | 0x23 | 0x40 | 0x64 | 0x87 => 852,
        0x24 => 860,
        0x26 | 0x65 => 866,
        0x4D | 0x7A => 936,
        0x4E | 0x79 => 949,
        0x4F | 0x78 => 950,
        0x50 | 0x7C => 874,
        0x67 => 861,
        0x68 => 895,
        0x69 => 620,
        0x6A | 0x86 => 737,
        0x6B | 0x88 => 857,
        0x7D => 1255,
        0x7E => 1256,
        0x96 => 10007,
        0x97 => 10029,
        0xC8 => 1250,
        0xC9 => 1251,
        0xCA => 1254,
        0xCB => 1253,
        0xCC => 1257,
        0xF0 => 65001,
        _ => return None,
    })
}

pub(crate) fn encoding(cpg: Option<&[u8]>, ldid: u8, notes: &mut Vec<String>) -> DbfEncoding {
    if let Some(bytes) = cpg {
        let label = if bytes.len() <= MAX_CPG_BYTES { std::str::from_utf8(bytes).unwrap_or("") } else { "" };
        let label = label.trim().trim_start_matches('\u{feff}').trim();
        if let Some(encoding) = DbfEncoding::from_name(label) {
            return encoding;
        }
        notes.push("Shapefile: unsupported or empty .cpg; using the .dbf code page, or UTF-8 if unavailable".into());
    }
    // No language driver: UTF-8, as it always was, without a note.
    if ldid == 0 {
        return DbfEncoding::Utf8;
    }
    let page = ldid_code_page(ldid);
    if let Some(encoding) = page.and_then(DbfEncoding::from_number) {
        return encoding;
    }
    let named = page.map_or_else(String::new, |p| match page_name(p) {
        Some(name) => format!(" (code page {p}, {name})"),
        None => format!(" (code page {p})"),
    });
    notes.push(format!("Shapefile: unsupported .dbf code page 0x{ldid:02X}{named}; using UTF-8"));
    DbfEncoding::Utf8
}

/// The name of a code page a language driver may name and RustGIS can't decode, for the note.
fn page_name(page: u16) -> Option<&'static str> {
    Some(match page {
        620 => "DOS Polish (Mazovia)",
        737 => "DOS Greek",
        857 => "DOS Turkish",
        860 => "DOS Portuguese",
        863 => "DOS French Canadian",
        895 => "DOS Czech (Kamenický)",
        10029 => "Mac Central European",
        _ => return None,
    })
}

/// Makes a `.dbf` header's last-update date (bytes 1–3: years since 1900, month, day) one `dbase`
/// accepts, so a writer that leaves it zero doesn't make the file unreadable (#1272). Nothing reads
/// the date, so a placeholder (1 January of its year) is all it takes; a real date is kept.
pub(crate) fn readable_header_date(dbf: &mut [u8]) {
    if let Some([month, day]) = dbf.get_mut(2..4)
        && (!(1..=12).contains(month) || !(1..=31).contains(day))
    {
        (*month, *day) = (1, 1);
    }
}

/// Marks a `.dbf`'s date (`D`) columns as character columns in its header, so `dbase` hands their
/// bytes over as text for [`date`] to read instead of failing the whole record on a date it can't
/// read (`20260230`, `00000000`). Returns the positions of those columns.
pub(crate) fn dates_as_text(dbf: &mut [u8]) -> Vec<usize> {
    retype_columns(dbf, b'D', b'C')
}

/// Marks a `.dbf`'s floating point (`F`) columns as numeric (`N`) in its header: `dbase` reads the
/// same text of both, but an `F` column only as an `f32`, which rounds `1809878594` to
/// `1809878656` and a latitude after its seventh digit. As `N` it is read as an `f64`, as GDAL does.
pub(crate) fn floats_as_numeric(dbf: &mut [u8]) {
    retype_columns(dbf, b'F', b'N');
}

/// The number of decimal places declared for each field in the `.dbf` header.
pub(crate) fn field_decimals(dbf: &[u8]) -> Vec<u8> {
    let header_len = usize::from(u16::from_le_bytes([dbf.get(8).copied().unwrap_or(0), dbf.get(9).copied().unwrap_or(0)]));
    let mut decimals = Vec::new();
    for start in (32..header_len.min(dbf.len())).step_by(32) {
        if dbf.get(start).is_none_or(|b| *b == 0x0D) {
            break;
        }
        decimals.push(dbf.get(start + 17).copied().unwrap_or(0));
    }
    decimals
}

/// Changes the type of the `.dbf` columns whose type letter is `from` (either case) to `to` in the
/// header, and returns their positions among the columns.
fn retype_columns(dbf: &mut [u8], from: u8, to: u8) -> Vec<usize> {
    let header_len = usize::from(u16::from_le_bytes([dbf.get(8).copied().unwrap_or(0), dbf.get(9).copied().unwrap_or(0)]));
    let mut changed = Vec::new();
    for (k, start) in (32..header_len.min(dbf.len())).step_by(32).enumerate() {
        if dbf.get(start).is_none_or(|b| *b == 0x0D) {
            break;
        }
        if let Some(ty) = dbf.get_mut(start + 11)
            && ty.eq_ignore_ascii_case(&from)
        {
            *ty = to;
            changed.push(k);
        }
    }
    changed
}

/// A column of a `.dbf` as its header describes it: its name's bytes (up to the first NUL),
/// type letter, length in bytes and decimals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Column {
    pub name: Vec<u8>,
    pub ty: u8,
    pub len: u8,
    pub decimals: u8,
}

#[cfg(not(target_arch = "wasm32"))]
/// A `.dbf` read as its bytes, without decoding a value: its columns, and each record that isn't
/// marked deleted, as the bytes of each of its columns. `None` when the header doesn't describe
/// records that fit the file.
pub(crate) struct RawTable<'a> {
    pub columns: Vec<Column>,
    pub records: Vec<Vec<&'a [u8]>>,
    /// The records marked deleted, which aren't among `records`.
    pub deleted: usize,
}

#[cfg(not(target_arch = "wasm32"))]
/// Reads `dbf` as a [`RawTable`]: what a rewrite is compared with, byte for byte, where a
/// decoded value would hide a difference (text with leading spaces, which a read trims; a code
/// page's bytes).
pub(crate) fn raw_table(dbf: &[u8]) -> Option<RawTable<'_>> {
    let le16 = |at: usize| Some(usize::from(u16::from_le_bytes([*dbf.get(at)?, *dbf.get(at + 1)?])));
    let count = usize::try_from(u32::from_le_bytes(dbf.get(4..8)?.try_into().ok()?)).ok()?;
    let (header_len, record_len) = (le16(8)?, le16(10)?);
    let mut columns = Vec::new();
    for start in (32..header_len.min(dbf.len())).step_by(32) {
        let d = dbf.get(start..start + 32).filter(|d| d.first() != Some(&0x0D));
        let Some(d) = d else { break };
        let name = d.get(..11)?;
        let name = name.get(..name.iter().position(|b| *b == 0).unwrap_or(11))?.to_vec();
        columns.push(Column { name, ty: *d.get(11)?, len: *d.get(16)?, decimals: *d.get(17)? });
    }
    let width: usize = 1 + columns.iter().map(|c| usize::from(c.len)).sum::<usize>();
    if width != record_len || record_len == 0 {
        return None;
    }
    let body = dbf.get(header_len..)?;
    // Every byte must belong to a counted record or the optional EOF marker. A rewrite
    // must not discard uncounted records or other data left past the declared body.
    let trailing = body.get(count.checked_mul(record_len)?..)?;
    if !trailing.is_empty() && trailing != [0x1a] {
        return None;
    }
    let (mut records, mut deleted) = (Vec::new(), 0);
    for record in body.chunks_exact(record_len).take(count) {
        if record.first() == Some(&b'*') {
            deleted += 1;
            continue;
        }
        let mut at = 1;
        let mut values = Vec::with_capacity(columns.len());
        for c in &columns {
            let end = at + usize::from(c.len);
            values.push(record.get(at..end)?);
            at = end;
        }
        records.push(values);
    }
    Some(RawTable { columns, records, deleted })
}

/// A date (`D`) field's value, `YYYYMMDD`: `Ok(None)` when it's blank or zeros, as writers leave
/// a date empty, and `Err(())` when it isn't a date (`20260230`, `2026-3-5`). A year padded with
/// spaces (`   10101`, year 1, as GDAL writes it with `%4d`) is read as the zeros it stands for.
pub(crate) fn date(field: &[u8]) -> std::result::Result<Option<crate::date::Date>, ()> {
    let t = field.trim_ascii();
    if t.is_empty() || t == b"00000000" || field.iter().all(|b| *b == 0) {
        return Ok(None);
    }
    let padded: [u8; 8];
    let t = match <[u8; 8]>::try_from(field) {
        Ok(mut f) if t.len() != 8 => {
            f.iter_mut().take(4).take_while(|b| **b == b' ').for_each(|b| *b = b'0');
            padded = f;
            &padded[..]
        }
        _ => t,
    };
    let [y0, y1, y2, y3, m0, m1, d0, d1] = *t else { return Err(()) };
    let digits = |b: &[u8]| -> std::result::Result<i64, ()> {
        b.iter().try_fold(0i64, |n, c| if c.is_ascii_digit() { Ok(n * 10 + i64::from(c - b'0')) } else { Err(()) })
    };
    crate::date::Date::from_ymd(digits(&[y0, y1, y2, y3])?, digits(&[m0, m1])?, digits(&[d0, d1])?).map(Some).ok_or(())
}

/// A code-page label should be a few bytes. Bound reads even for a hostile sidecar.
pub(crate) fn read_cpg(path: &std::path::Path, notes: &mut Vec<String>) -> Option<Vec<u8>> {
    match try_read_cpg(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            notes.push(format!("Shapefile: could not read .cpg ({e}); using the .dbf code page, or UTF-8 if unavailable"));
            None
        }
    }
}

/// Every ASCII case of the `.cpg` extension, the conventional spellings first: tried in turn,
/// without scanning a potentially large directory, and the first there wins.
pub(crate) const CPG_SPELLINGS: [&str; 8] = ["cpg", "CPG", "Cpg", "cPg", "cpG", "CPg", "CpG", "cPG"];

fn try_read_cpg(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    for ext in CPG_SPELLINGS {
        match std::fs::File::open(path.with_extension(ext)) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CPG_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                return Ok(Some(bytes));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.dbf` date is a day, blank or zeros are empty, and anything else isn't a date.
    #[test]
    fn dbf_dates() {
        let day = |y, m, d| Ok(crate::date::Date::from_ymd(y, m, d));
        assert_eq!(date(b"20260305"), day(2026, 3, 5));
        assert_eq!(date(b"00010101"), day(1, 1, 1));
        // GDAL pads the year with spaces (`%4d%02d%02d`).
        assert_eq!(date(b"   10101"), day(1, 1, 1));
        assert_eq!(date(b"  990101"), day(99, 1, 1));
        assert_eq!(date(b" 9991231"), day(999, 12, 31));
        for empty in [&b""[..], b"        ", b"00000000", b"\0\0\0\0\0\0\0\0"] {
            assert_eq!(date(empty), Ok(None), "{empty:?}");
        }
        for bad in [
            &b"20260230"[..],
            b"20261301",
            b"2026-3-5",
            b"2026030",
            b"202603050",
            b"abcdefgh",
            b"00000101",
            b"0000    ",
            b"  2 0101",
            b"    0101",
            b"   1 101",
        ] {
            assert_eq!(date(bad), Err(()), "{bad:?}");
        }
    }

    /// Date columns are marked as character columns, and their positions given; others are kept.
    #[test]
    fn date_columns_read_as_text() {
        let mut dbf = vec![0u8; 32 + 3 * 32 + 1];
        dbf[8] = (dbf.len()) as u8;
        for (k, ty) in b"CDd".iter().copied().enumerate() {
            dbf[32 + k * 32 + 11] = ty;
        }
        dbf[32 + 3 * 32] = 0x0D;
        assert_eq!(dates_as_text(&mut dbf), [1, 2]);
        assert_eq!([dbf[43], dbf[75], dbf[107]], [b'C', b'C', b'C']);
        assert!(dates_as_text(&mut [0u8; 10]).is_empty());
    }

    /// Float columns are marked as numeric columns; others, and what follows the header, are kept.
    #[test]
    fn float_columns_read_as_numeric() {
        let mut dbf = vec![0u8; 32 + 3 * 32 + 2];
        dbf[8] = (32 + 3 * 32 + 1) as u8;
        for (k, ty) in b"FfC".iter().copied().enumerate() {
            dbf[32 + k * 32 + 11] = ty;
        }
        dbf[32 + 3 * 32] = 0x0D;
        dbf[32 + 3 * 32 + 1] = b'F';
        floats_as_numeric(&mut dbf);
        assert_eq!([dbf[43], dbf[75], dbf[107], dbf[129]], [b'N', b'N', b'C', b'F']);
        floats_as_numeric(&mut [0u8; 10]);
    }

    /// A header date that isn't one gets a placeholder; a real one, and a header cut too short
    /// to have one, are left as they are.
    #[test]
    fn a_header_date_that_isnt_one_is_made_readable() {
        for (date, readable) in [
            ([0, 0, 0], [0, 1, 1]),
            ([126, 0, 1], [126, 1, 1]),
            ([126, 11, 0], [126, 1, 1]),
            ([126, 13, 1], [126, 1, 1]),
            ([126, 2, 32], [126, 1, 1]),
            ([126, 11, 1], [126, 11, 1]),
            ([255, 12, 31], [255, 12, 31]),
        ] {
            let mut header = [3, date[0], date[1], date[2], 9];
            readable_header_date(&mut header);
            assert_eq!(header, [3, readable[0], readable[1], readable[2], 9], "{date:?}");
        }
        let mut short = [3, 0, 0];
        readable_header_date(&mut short);
        assert_eq!(short, [3, 0, 0]);
    }

    /// Every language driver dbase's own table names and RustGIS read before keeps its code page:
    /// the table replaced dbase's, and must not lose or change one of its entries.
    #[test]
    fn every_language_driver_dbase_names_keeps_its_code_page() {
        for ldid in 0..=u8::MAX {
            let before = match CodePageMark::from(ldid) {
                CodePageMark::CP437 => 437,
                CodePageMark::CP850 => 850,
                CodePageMark::CP852 => 852,
                CodePageMark::CP861 => 861,
                CodePageMark::CP865 => 865,
                CodePageMark::CP866 => 866,
                CodePageMark::CP874 => 874,
                CodePageMark::CP932 => 932,
                CodePageMark::CP936 => 936,
                CodePageMark::CP949 => 949,
                CodePageMark::CP950 => 950,
                CodePageMark::CP1250 => 1250,
                CodePageMark::CP1251 => 1251,
                CodePageMark::CP1252 => 1252,
                CodePageMark::CP1253 => 1253,
                CodePageMark::CP1254 => 1254,
                CodePageMark::CP1255 => 1255,
                CodePageMark::CP1256 => 1256,
                CodePageMark::Utf8 => 65001,
                // Pages dbase names that RustGIS can't decode: the table names them too.
                CodePageMark::CP620 => 620,
                CodePageMark::CP737 => 737,
                CodePageMark::CP857 => 857,
                CodePageMark::CP895 => 895,
                // Not a language driver: only 0, which the table leaves out (read as UTF-8, no note).
                CodePageMark::Undefined => {
                    assert_eq!(ldid, 0, "LDID 0x{ldid:02X}");
                    continue;
                }
                // An ID dbase doesn't know. (No other arm: a variant a dbase update adds fails to
                // compile here.)
                CodePageMark::Invalid => continue,
            };
            assert_eq!(ldid_code_page(ldid), Some(before), "LDID 0x{ldid:02X}");
            let decoded = !matches!(before, 620 | 737 | 857 | 895);
            assert_eq!(DbfEncoding::from_number(before).is_some(), decoded, "LDID 0x{ldid:02X}");
        }
    }

    #[test]
    fn a_page_rustgis_cant_decode_is_named_in_the_note() {
        let mut notes = Vec::new();
        assert!(matches!(encoding(None, 0x6C, &mut notes), DbfEncoding::Utf8));
        assert_eq!(notes, ["Shapefile: unsupported .dbf code page 0x6C (code page 863, DOS French Canadian); using UTF-8"]);
        let mut notes = Vec::new();
        encoding(None, 0xFF, &mut notes);
        assert_eq!(notes, ["Shapefile: unsupported .dbf code page 0xFF; using UTF-8"]);
    }
}
