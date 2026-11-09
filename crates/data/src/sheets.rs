//! Spreadsheets: Excel (`.xlsx`, `.xlsm`, `.xlsb`, `.xls`) and OpenDocument (`.ods`) workbooks,
//! read in pure Rust (in the browser too). A sheet is read like delimited text: its first
//! non-empty row names the columns, and x/y or WKT columns (found, or given with
//! [`CsvOptions`]) make the geometry; a sheet without them is a table.

use std::io::Cursor;

use calamine::{Data, ExcelDateTime, ExcelDateTimeType, Reader, Sheets, open_workbook_auto_from_rs};

use crate::io::{Columns, CsvOptions, columns_of, read_rows};
use crate::{DataError, Dataset, ReadReport, Result};

mod biff;

/// Spreadsheet file types.
pub const EXTENSIONS: &[&str] = &["xlsx", "xlsm", "xlsb", "xls", "ods"];

/// The most cells read from one sheet (calamine holds a sheet as a dense grid, and its cells are
/// copied as text).
const MAX_CELLS: usize = if cfg!(target_arch = "wasm32") { 4_000_000 } else { 20_000_000 };

/// Whether a file name is a spreadsheet.
pub fn is_sheet_name(name: &str) -> bool {
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// The sheets of a workbook, in order.
pub fn sheet_names(bytes: &[u8]) -> Result<Vec<String>> {
    check_expansion(bytes)?;
    check_compound(bytes)?;
    guarded(|| {
        let wb = open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| DataError::Format(format!("spreadsheet: {e}")))?;
        Ok(wb.sheet_names())
    })
}

/// Runs calamine on a workbook, turning a panic into an error. calamine indexes some of a file's
/// tables without checking them (an `.xls` sector chain that points past its allocation table
/// panics, even in 0.38), so a damaged file is refused rather than taking the app down. In the
/// browser, where a panic can't be caught, this guards nothing.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(DataError::Format("spreadsheet: the file is damaged and can't be read".into())))
}

/// Reads one sheet of a workbook (`sheet` by name, without regard to case; the first when `None`),
/// named `<name>` (or `<name> <sheet>` when the workbook has several). The report names the other
/// sheets.
pub fn read(bytes: &[u8], name: &str, sheet: Option<&str>, opts: &CsvOptions) -> Result<(Dataset, ReadReport)> {
    let g = grid(bytes, sheet)?;
    let title = if g.names.len() > 1 { format!("{name} {}", g.pick) } else { name.to_string() };
    let (ds, mut report) = read_rows(&g.headers, &g.rows, 0, &title, opts)?;
    let others: Vec<&str> = g.names.iter().filter(|n| **n != g.pick).map(String::as_str).take(20).collect();
    if !others.is_empty() {
        report.messages.push(format!("the workbook has more sheets ({}): add one with \"layer\"", others.join(", ")));
    }
    Ok((ds, report))
}

/// The columns of one sheet of a workbook (`sheet` as [`read`] takes it), and those its geometry
/// is read from when none are given.
pub fn columns(bytes: &[u8], sheet: Option<&str>) -> Result<Columns> {
    let g = grid(bytes, sheet)?;
    Ok(columns_of(&g.headers, &g.rows))
}

/// A sheet as text: the workbook's sheets, the one picked, its column names and its rows.
struct Grid {
    names: Vec<String>,
    pick: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn grid(bytes: &[u8], sheet: Option<&str>) -> Result<Grid> {
    check_expansion(bytes)?;
    check_compound(bytes)?;
    guarded(|| grid_unguarded(bytes, sheet))
}

fn grid_unguarded(bytes: &[u8], sheet: Option<&str>) -> Result<Grid> {
    let mut wb = open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| DataError::Format(format!("spreadsheet: {e}")))?;
    let names = wb.sheet_names();
    let pick = match sheet.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(s))
            .cloned()
            .ok_or_else(|| DataError::Format(format!("no sheet '{s}' (sheets: {})", names.join(", "))))?,
        None => names.first().cloned().ok_or_else(|| DataError::Format("the workbook has no sheets".into()))?,
    };
    check_size(&mut wb, &pick)?;
    let range = wb.worksheet_range(&pick).map_err(|e| DataError::Format(format!("sheet '{pick}': {e}")))?;
    let (h, w) = range.get_size();
    if h.saturating_mul(w) > MAX_CELLS {
        return Err(DataError::Format(format!("sheet '{pick}' is too large ({h} × {w} cells)")));
    }
    let mut rows = range.rows().map(|r| r.iter().map(cell).collect::<Vec<String>>());
    // The first row with anything in it names the columns.
    let headers: Vec<String> = rows.by_ref().find(|r| r.iter().any(|c| !c.trim().is_empty())).unwrap_or_default();
    if headers.is_empty() {
        return Err(DataError::Format(format!("sheet '{pick}' is empty")));
    }
    // Columns without a heading are named by their letter, as the spreadsheet shows them (the
    // range starts at its first column with anything in it).
    let first_col = range.start().map(|(_, c)| c as usize).unwrap_or(0);
    let headers: Vec<String> = headers
        .into_iter()
        .enumerate()
        .map(|(i, h)| if h.trim().is_empty() { column_letter(first_col + i) } else { h.trim().to_string() })
        .collect();
    let rows: Vec<Vec<String>> = rows.filter(|r| r.iter().any(|c| !c.trim().is_empty())).collect();
    Ok(Grid { names, pick, headers, rows })
}

/// A zipped workbook (`.xlsx`, `.ods`) that would inflate past [`crate::zip::MAX_TOTAL`] is
/// refused before it is read: the archive's own reader inflates each part within that budget,
/// checking its size and checksum against what the archive says (calamine trusts the headers).
fn check_expansion(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(());
    }
    crate::zip::read_matching_within(bytes, |n| n.ends_with(".xml") || n.ends_with(".bin"), crate::zip::MAX_TOTAL)
        .map(|_| ())
        .map_err(|e| DataError::Format(format!("spreadsheet: {e}")))
}

/// The signature of a compound file (`.xls`).
const COMPOUND: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
/// Sector ids from here up are markers, not sectors.
const RESERVED_SECTORS: u32 = 0xFFFF_FFFA;
const DIFAT_SECTOR: u32 = 0xFFFF_FFFC;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;

/// Refuses a compound file (`.xls`) whose sector tables calamine can't walk safely. calamine
/// follows each chain of sectors through an allocation table without checking it: a chain that
/// loops runs forever, growing memory as it goes, and one that reaches past its table or its data
/// panics (still so in calamine 0.38). The tables are rebuilt here as calamine builds them (the
/// FAT, the directory, the mini FAT), and each chain calamine reads (the directory, the mini FAT,
/// the mini stream and the streams it opens) is replayed as it reads it, to the length it reads.
/// Other chains, and unused entries with junk in them, are never followed and aren't checked.
/// Other files pass unchecked.
fn check_compound(bytes: &[u8]) -> Result<()> {
    if !bytes.starts_with(COMPOUND) {
        return Ok(());
    }
    let u16_at = |at: usize| bytes.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| bytes.get(at..at + 4).map_or(END_OF_CHAIN, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let size: usize = match u16_at(30) {
        0x09 => 512,
        0x0C => 4096,
        // calamine refuses the rest itself.
        _ => return Ok(()),
    };
    // The sectors follow the header, which takes one sector; calamine reads a sector cut short by
    // the end of the file as far as it goes.
    let body = bytes.get(size..).unwrap_or(&[]);
    let sector = |id: u32| -> &[u8] {
        let start = (id as usize).saturating_mul(size);
        body.get(start..start.saturating_add(size).min(body.len())).unwrap_or(&[])
    };
    let max_sector = body.len() / size;
    // The FAT's sectors: 109 listed in the header, more in a chain of DIFAT sectors whose last
    // entry links to the next (calamine takes the link off the end of the list it has gathered).
    let mut difat = words(bytes.get(76..512).unwrap_or(&[]));
    let mut next = u32_at(68);
    let mut hops = 0usize;
    while next < RESERVED_SECTORS {
        hops += 1;
        if hops > max_sector + 1 {
            return damaged("its DIFAT chain loops");
        }
        let more = sector(next);
        if more.is_empty() {
            // calamine would take the last FAT sector gathered for the next link.
            return damaged("a DIFAT sector is past the end of the file");
        }
        difat.extend(words(more));
        next = difat.pop().unwrap_or(END_OF_CHAIN);
    }
    let fat_sectors: Vec<u32> = difat.into_iter().filter(|id| *id < DIFAT_SECTOR).collect();
    // A file can't hold more FAT sectors than it has sectors.
    if fat_sectors.len() > max_sector + 1 {
        return damaged("its allocation table is larger than the file");
    }
    let fat: Vec<u32> = fat_sectors.iter().flat_map(|id| words(sector(*id))).collect();
    let main = Sectors { table: &fat, data_len: body.len(), size };
    // The directory, one 128-byte entry per stream; its chain starts in the header, and calamine
    // reads it to its end.
    let dir: Vec<u8> = main.read(u32_at(48), usize::MAX)?.into_iter().flat_map(sector).copied().collect();
    let (entries, _) = dir.as_chunks::<128>();
    // Each stream's length: four bytes with 512-byte sectors, eight with 4096-byte ones, which
    // calamine expects to fit a `usize` (it panics in the browser on one past 4 GiB).
    let mut streams = Vec::new();
    for e in entries {
        let at = |i: usize| e.get(i..i + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let len = if size == 512 { u64::from(at(120)) } else { u64::from(at(120)) | (u64::from(at(124)) << 32) };
        let Ok(len) = usize::try_from(len) else {
            return damaged("a stream is longer than this system can address");
        };
        // The name as calamine reads it: decoded as UTF-16 (a byte-order mark is dropped, or
        // switches it to big-endian), up to the first NUL.
        let mut name = encoding_rs::UTF_16LE.decode(e.get(..64).unwrap_or(&[])).0.into_owned();
        if let Some(nul) = name.find('\0') {
            name.truncate(nul);
        }
        streams.push((name, at(116), len));
    }
    // Small streams sit in the mini stream (the root entry's chain), in 64-byte sectors laid out
    // by the mini FAT. calamine reads both only when the header gives the mini FAT a length.
    let mini_fat_len = u32_at(64) as usize;
    let (mini_fat, mini_len, root_sectors) = if mini_fat_len > 0 {
        let (root, root_len) = streams.first().map_or((END_OF_CHAIN, 0), |(_, s, l)| (*s, *l));
        let root_sectors = main.read(root, root_len)?;
        let mini_len = root_sectors.iter().map(|id| sector(*id).len()).sum::<usize>().min(root_len);
        let table_len = mini_fat_len.saturating_mul(size);
        let mini_fat: Vec<u8> = main.read(u32_at(60), table_len)?.into_iter().flat_map(sector).copied().take(table_len).collect();
        (words(&mini_fat), mini_len, root_sectors)
    } else {
        (Vec::new(), 0, Vec::new())
    };
    let mini = Sectors { table: &mini_fat, data_len: mini_len, size: 64 };
    // calamine reads these streams, each the first entry by its name (without regard to case),
    // through the mini FAT when it is shorter than 4096 bytes. Other entries, unused ones with
    // junk in them among them, are never read.
    let has_workbook = streams.iter().any(|(name, ..)| name.eq_ignore_ascii_case("Workbook"));
    for want in ["Workbook", "Book", "\u{5}SummaryInformation", "\u{5}DocumentSummaryInformation"] {
        if let Some((_, start, len)) = streams.iter().find(|(name, ..)| name.eq_ignore_ascii_case(want)) {
            let ids = if *len < 4096 { mini.read(*start, *len)? } else { main.read(*start, *len)? };
            if want == "Workbook" || (want == "Book" && !has_workbook) {
                if *len as u64 > crate::io::MAX_BYTES {
                    return Err(DataError::Format("spreadsheet: the XLS Workbook stream is too large".into()));
                }
                // Read only the chosen stream. In the mini stream a 64-byte sector is a slice
                // of the root's sector chain, which may itself be fragmented.
                let workbook: Vec<u8> = if *len < 4096 {
                    ids.iter()
                        .flat_map(|id| {
                            let from = (*id as usize).saturating_mul(64);
                            root_sectors.get(from / size).map_or(&[][..], |root| {
                                let data = sector(*root);
                                let offset = from % size;
                                data.get(offset..offset.saturating_add(64).min(data.len())).unwrap_or(&[])
                            })
                        })
                        .copied()
                        .take(*len)
                        .collect()
                } else {
                    ids.iter().flat_map(|id| sector(*id)).copied().take(*len).collect()
                };
                biff::check(&workbook, MAX_CELLS)
                    .map_err(|e| DataError::Format(format!("spreadsheet: the .xls file cannot be read safely ({e})")))?;
            }
        }
    }
    Ok(())
}

/// Sectors of `size` bytes over `data_len` bytes of data, chained by `table`.
struct Sectors<'a> {
    table: &'a [u32],
    data_len: usize,
    size: usize,
}

impl Sectors<'_> {
    /// The sectors calamine reads for `len` bytes from `start`, replaying its walk: it reads a
    /// sector, stops once it has `len` bytes, and only then looks up the next link, unchecked. A
    /// sector must start within the data (calamine slices its buffer from the sector's start to
    /// what it could read, which panics for one past the data unless an earlier read happened to
    /// grow the buffer that far); a link must be in the table; and a chain can't read more sectors
    /// than there are without looping.
    fn read(&self, start: u32, len: usize) -> Result<Vec<u32>> {
        let mut ids = Vec::new();
        let (mut at, mut got) = (start, 0usize);
        while at != END_OF_CHAIN {
            let from = (at as usize).saturating_mul(self.size);
            if from > self.data_len {
                return damaged(if ids.is_empty() {
                    "a chain of sectors starts past the end of its data"
                } else {
                    "a chain of sectors runs past the end of its data"
                });
            }
            if ids.len() > self.data_len / self.size + 1 {
                return damaged("a chain of sectors loops");
            }
            ids.push(at);
            got = got.saturating_add(self.size.min(self.data_len - from));
            if got >= len {
                break;
            }
            let Some(next) = self.table.get(at as usize) else {
                return damaged("a chain of sectors runs past its allocation table");
            };
            at = *next;
        }
        Ok(ids)
    }
}

/// A sector's (or a table's) little-endian 32-bit words; a partial word at the end is dropped, as
/// calamine drops it.
fn words(b: &[u8]) -> Vec<u32> {
    b.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect()
}

fn damaged<T>(what: &str) -> Result<T> {
    Err(DataError::Format(format!("spreadsheet: the .xls file is damaged ({what})")))
}

/// Refuses a sheet whose cells span more than [`MAX_CELLS`] before calamine lays it out as a dense
/// grid (one cell at `XFD1048576` would ask for 17 billion): an `.xlsx` or `.xlsb` sheet's cells
/// are read one at a time for their extent first. Every `.xls` sheet's BIFF records and cell
/// spans are checked by [`check_compound`] before the workbook is opened; an `.ods` one is
/// checked after reading.
fn check_size(wb: &mut Sheets<Cursor<&[u8]>>, sheet: &str) -> Result<()> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    let mut grow = |(r, c): (u32, u32)| -> Result<()> {
        let (r0, r1, c0, c1) = b.map_or((r, r, c, c), |(r0, r1, c0, c1)| (r0.min(r), r1.max(r), c0.min(c), c1.max(c)));
        b = Some((r0, r1, c0, c1));
        let cells = ((r1 - r0) as usize + 1).saturating_mul((c1 - c0) as usize + 1);
        if cells > MAX_CELLS {
            return Err(DataError::Format(format!(
                "sheet '{sheet}' is too large (cells from row {} to {}, column {} to {})",
                r0 + 1,
                r1 + 1,
                column_letter(c0 as usize),
                column_letter(c1 as usize)
            )));
        }
        Ok(())
    };
    let bad = |e: String| DataError::Format(format!("sheet '{sheet}': {e}"));
    match wb {
        Sheets::Xlsx(x) => {
            let mut cells = x.worksheet_cells_reader(sheet).map_err(|e| bad(e.to_string()))?;
            while let Some(c) = cells.next_cell().map_err(|e| bad(e.to_string()))? {
                grow(c.get_position())?;
            }
        }
        Sheets::Xlsb(x) => {
            let mut cells = x.worksheet_cells_reader(sheet).map_err(|e| bad(e.to_string()))?;
            while let Some(c) = cells.next_cell().map_err(|e| bad(e.to_string()))? {
                grow(c.get_position())?;
            }
        }
        Sheets::Xls(_) | Sheets::Ods(_) => {}
    }
    Ok(())
}

/// A cell as text: whole numbers without a decimal point, dates as ISO 8601, times of day as
/// `HH:MM:SS` and durations as `[h]:MM:SS`.
fn cell(d: &Data) -> String {
    match d {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(s) => s.clone(),
        Data::Bool(b) => b.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::DateTime(dt) if dt.is_duration() => duration(dt.as_f64()),
        Data::DateTime(dt) => excel_date(dt.as_f64() + if is_1904(dt) { DAYS_1900_TO_1904 } else { 0.0 }, true),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
    }
}

/// Days from the 1900 date system's day 0 to the 1904 one's (workbooks made on old Macs).
const DAYS_1900_TO_1904: f64 = 1462.0;

/// Whether a date counts its days from 1904: calamine converts it by its workbook's date system,
/// so the same serial converted from 1900 comes out differently.
fn is_1904(dt: &ExcelDateTime) -> bool {
    let v = dt.as_f64();
    v.is_finite()
        && (0.0..3_000_000.0).contains(&v)
        && dt.to_ymd_hms_milli() != ExcelDateTime::new(v, ExcelDateTimeType::DateTime, false).to_ymd_hms_milli()
}

/// A duration in days as `[h]:MM:SS` (hours past 24 kept, as Excel shows `[h]:mm:ss`).
fn duration(days: f64) -> String {
    if !days.is_finite() || days.abs() > 1e9 {
        return days.to_string();
    }
    let secs = (days.abs() * 86_400.0).round() as i64;
    let sign = if days < 0.0 && secs > 0 { "-" } else { "" };
    format!("{sign}{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// An Excel serial date (days since 1899-12-30) as `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM:SS` when it
/// has a time of day; a time alone (a serial below 1) as `HH:MM:SS`.
fn excel_date(serial: f64, with_time: bool) -> String {
    if !serial.is_finite() || !(0.0..3_000_000.0).contains(&serial) {
        return serial.to_string();
    }
    if serial < 1.0 && with_time {
        let secs = ((serial * 86_400.0).round() as i64).min(86_399);
        return format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    }
    let mut days = serial.floor() as i64;
    let mut secs = ((serial - serial.floor()) * 86_400.0).round() as i64;
    // A time that rounds up to midnight is the next day.
    if secs >= 86_400 {
        days += 1;
        secs -= 86_400;
    }
    // Excel counts a 29 February 1900 that never was: serials before it are a day early.
    if days < 60 {
        days += 1;
    }
    // Days from 1970-01-01, then the civil date (Howard Hinnant's algorithm).
    let z = days - 25_569 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    if with_time && secs > 0 {
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{y:04}-{m:02}-{d:02}")
    }
}

/// A column's letter name: A, B, …, Z, AA, AB…
fn column_letter(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// A minimal `.xlsx`: sheets of rows of cells (numbers, or inline strings), for tests.
#[doc(hidden)]
pub fn xlsx(sheets: &[(&str, &[&[&str]])]) -> Vec<u8> {
    // A cell given as `<c …>` XML goes in as it is (to place it with `r="…"`).
    let cell = |v: &str| {
        if v.starts_with("<c") {
            v.to_string()
        } else if v.parse::<f64>().is_ok() {
            format!("<c><v>{v}</v></c>")
        } else {
            format!("<c t=\"inlineStr\"><is><t>{v}</t></is></c>")
        }
    };
    let mut entries = vec![
        (
            "[Content_Types].xml".to_string(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{}</Types>"#,
                (1..=sheets.len())
                    .map(|i| format!(r#"<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#))
                    .collect::<String>()
            )
            .into_bytes(),
        ),
        (
            "_rels/.rels".to_string(),
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_vec(),
        ),
        (
            "xl/workbook.xml".to_string(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{}</sheets></workbook>"#,
                sheets.iter().enumerate().map(|(i, (n, _))| format!(r#"<sheet name="{n}" sheetId="{}" r:id="rId{}"/>"#, i + 1, i + 1)).collect::<String>()
            )
            .into_bytes(),
        ),
        (
            "xl/_rels/workbook.xml.rels".to_string(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{}</Relationships>"#,
                (1..=sheets.len())
                    .map(|i| format!(r#"<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i}.xml"/>"#))
                    .collect::<String>()
            )
            .into_bytes(),
        ),
    ];
    for (i, (_, rows)) in sheets.iter().enumerate() {
        let data: String = rows.iter().map(|r| format!("<row>{}</row>", r.iter().map(|v| cell(v)).collect::<String>())).collect();
        entries.push((
            format!("xl/worksheets/sheet{}.xml", i + 1),
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{data}</sheetData></worksheet>"#).into_bytes(),
        ));
    }
    crate::zip::write(&entries).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_letters() {
        assert_eq!(excel_date(45_000.0, false), "2023-03-15");
        assert_eq!(excel_date(25_569.5, true), "1970-01-01T12:00:00");
        assert_eq!(excel_date(1.0, false), "1900-01-01");
        assert_eq!(excel_date(59.0, false), "1900-02-28");
        assert_eq!(excel_date(61.0, false), "1900-03-01");
        assert_eq!(excel_date(45_000.999_999_9, true), "2023-03-16", "rounds up to the next midnight");
        assert_eq!(excel_date(0.5, true), "12:00:00", "a time of day alone");
        assert_eq!(excel_date(0.0, true), "00:00:00");
        assert_eq!(duration(1.5), "36:00:00");
        assert_eq!(duration(-0.25), "-6:00:00");
        // A 1904-system date is the same day as its 1900-system serial plus 1462.
        let d1904 = ExcelDateTime::new(45_000.0 - DAYS_1900_TO_1904, ExcelDateTimeType::DateTime, true);
        assert!(is_1904(&d1904));
        assert_eq!(cell(&Data::DateTime(d1904)), "2023-03-15");
        let d1900 = ExcelDateTime::new(45_000.0, ExcelDateTimeType::DateTime, false);
        assert!(!is_1904(&d1900));
        assert_eq!(cell(&Data::DateTime(d1900)), "2023-03-15");
        assert_eq!(column_letter(0), "A");
        assert_eq!(column_letter(25), "Z");
        assert_eq!(column_letter(26), "AA");
        assert_eq!(column_letter(701), "ZZ");
        assert!(is_sheet_name("a.XLSX") && is_sheet_name("b.ods") && !is_sheet_name("c.csv"));
    }

    #[test]
    fn reads_sheets() {
        let book = xlsx(&[
            (
                "Cities",
                &[
                    &["name", "lon", "lat", "pop"],
                    &["Paris", "2.35", "48.85", "2100000"],
                    &["", "", "", ""],
                    &["Rome", "12.5", "41.9", "2800000"],
                ],
            ),
            ("Utm", &[&["", "", ""], &["id", "easting", "northing"], &["1", "500000", "4649776"]]),
            ("Notes", &[&["text", ""], &["hello", "x"]]),
        ]);
        assert_eq!(sheet_names(&book).unwrap(), ["Cities", "Utm", "Notes"]);
        let (ds, rep) = read(&book, "book", None, &CsvOptions::default()).unwrap();
        assert_eq!(ds.name, "book Cities");
        assert_eq!(ds.features.len(), 2, "the empty row is passed over");
        assert_eq!(ds.geometry_type, crate::GeometryType::Point);
        assert_eq!(ds.fields.iter().find(|f| f.name == "pop").map(|f| f.ty), Some(crate::FieldType::Integer));
        assert!(rep.messages.iter().any(|m| m.contains("Utm, Notes")));
        // The header row comes after a blank one; the coordinates are UTM 33N.
        let opts = CsvOptions { crs: crate::raster::Crs::from_epsg(32633), ..CsvOptions::default() };
        let (ds, _) = read(&book, "book", Some("utm"), &opts).unwrap();
        let e = ds.features[0].geometry.as_ref().unwrap().extent();
        assert!((e.min_x - 15.0).abs() < 0.01 && (e.min_y - 42.0).abs() < 0.05, "{e:?}");
        let (ds, _) = read(&book, "book", Some("Notes"), &CsvOptions::default()).unwrap();
        assert_eq!(ds.geometry_type, crate::GeometryType::None);
        assert_eq!(ds.fields[1].name, "B", "a column without a heading is named by its letter");
        assert!(read(&book, "book", Some("Missing"), &CsvOptions::default()).is_err());
        // The columns Add Delimited Text offers, found as the sheet is read.
        let c = columns(&book, None).unwrap();
        assert_eq!(c.names, ["name", "lon", "lat", "pop"]);
        assert_eq!((c.x.as_deref(), c.y.as_deref(), c.delimiter), (Some("lon"), Some("lat"), None));
        let c = columns(&book, Some("utm")).unwrap();
        assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some("easting"), Some("northing")));
        assert_eq!(columns(&book, Some("Notes")).unwrap().names, ["text", "B"]);
        for cut in (0..book.len()).step_by(61) {
            let _ = read(&book[..cut], "t", None, &CsvOptions::default());
            let _ = columns(&book[..cut], None);
        }
    }

    #[test]
    fn junk_is_refused() {
        assert!(read(b"not a workbook", "t", None, &CsvOptions::default()).is_err());
        assert!(sheet_names(b"PK\x03\x04junk").is_err());
        // Two cells far apart are refused before calamine lays out the grid between them.
        let far = xlsx(&[("Far", &[&[r#"<c r="A1"><v>1</v></c>"#], &[r#"<c r="XFD1048576"><v>2</v></c>"#]])]);
        let e = read(&far, "t", None, &CsvOptions::default()).unwrap_err().to_string();
        assert!(e.contains("too large") && e.contains("XFD"), "{e}");
    }

    /// A compound-file (`.xls`) header with the given fields, followed by `body`.
    fn ole2(sector_shift: u16, fat_sectors: u32, first_dir: u32, difat_start: u32, difat_sectors: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
        b[24..26].copy_from_slice(&0x3Eu16.to_le_bytes());
        b[26..28].copy_from_slice(&3u16.to_le_bytes());
        b[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
        b[30..32].copy_from_slice(&sector_shift.to_le_bytes());
        b[32..34].copy_from_slice(&6u16.to_le_bytes());
        b[44..48].copy_from_slice(&fat_sectors.to_le_bytes());
        b[48..52].copy_from_slice(&first_dir.to_le_bytes());
        b[56..60].copy_from_slice(&4096u32.to_le_bytes());
        b[60..64].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        b[68..72].copy_from_slice(&difat_start.to_le_bytes());
        b[72..76].copy_from_slice(&difat_sectors.to_le_bytes());
        // The header's DIFAT points every FAT sector at sector 0.
        for i in 0..109 {
            let v: u32 = if i == 0 { 0 } else { 0xFFFF_FFFF };
            b[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(body);
        b
    }

    #[test]
    fn hostile_xls_is_refused() {
        // RUSTSEC-2026-0436: calamine before 0.38 panicked or allocated without bound on a
        // malformed compound file, and 0.38 still panics on a sector chain past the allocation
        // table. Each of these must be an error, not a crash.
        let sector = [0xFFu8; 512];
        let books = [
            ole2(9, u32::MAX, 0, 0xFFFF_FFFE, 0, &sector),
            ole2(9, 1, u32::MAX - 1, 0xFFFF_FFFE, 0, &sector),
            ole2(9, 1, 0, 0, 0x7FFF_FFFF, &sector),
            ole2(31, 1, 0, 0xFFFF_FFFE, 0, &sector),
            ole2(0, 1, 0, 0xFFFF_FFFE, 0, &sector),
            ole2(9, 1, 0, 0xFFFF_FFFE, 0, &[]),
            ole2(9, 1, 0, 0xFFFF_FFFE, 0, &[0u8; 512]),
        ];
        for (i, book) in books.iter().enumerate() {
            assert!(read(book, "t.xls", None, &CsvOptions::default()).is_err(), "book {i}");
            // Some damaged headers still list no sheets; none may panic.
            let _ = sheet_names(book);
            assert!(columns(book, None).is_err(), "book {i}");
        }
    }

    /// Little-endian words as bytes, padded with `fill` to whole 512-byte sectors.
    fn sectors(words: &[u32], fill: u8) -> Vec<u8> {
        let mut b: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        b.resize(b.len().div_ceil(512).max(1) * 512, fill);
        b
    }

    /// A small compound file: FAT in sector 0, directory in sector 1 (a root entry and a
    /// "Workbook" stream of `stream_len` bytes starting at `stream_start`), mini FAT in sector 2
    /// (`mini_fat`), mini stream in sector 3.
    fn small_book(fat: &[u32], stream_start: u32, stream_len: u32, mini_fat: &[u32]) -> Vec<u8> {
        let entry = |name: &str, kind: u8, start: u32, len: u32| {
            let mut e = [0u8; 128];
            let utf16: Vec<u8> = name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
            e[..utf16.len()].copy_from_slice(&utf16);
            e[64..66].copy_from_slice(&(utf16.len() as u16).to_le_bytes());
            e[66] = kind;
            e[116..120].copy_from_slice(&start.to_le_bytes());
            e[120..124].copy_from_slice(&len.to_le_bytes());
            e
        };
        let mut dir = Vec::new();
        dir.extend(entry("Root Entry", 5, 3, 64));
        dir.extend(entry("Workbook", 2, stream_start, stream_len));
        dir.resize(512, 0);
        let mut body = sectors(fat, 0xFF);
        body.extend(dir);
        body.extend(sectors(mini_fat, 0xFF));
        body.extend([0u8; 512]);
        let mut b = ole2(9, 1, 1, 0xFFFF_FFFE, 0, &body);
        b[60..64].copy_from_slice(&2u32.to_le_bytes());
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b
    }

    /// A BIFF8 workbook in a sound compound container, with small worksheet streams.
    fn biff_book(globals: &[(u16, Vec<u8>)], sheets: &[Vec<(u16, Vec<u8>)>]) -> Vec<u8> {
        fn record(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
            out.extend(kind.to_le_bytes());
            out.extend((data.len() as u16).to_le_bytes());
            out.extend(data);
        }
        let bof = |kind: u16| {
            let mut data = vec![0u8; 16];
            data[..2].copy_from_slice(&0x0600u16.to_le_bytes());
            data[2..4].copy_from_slice(&kind.to_le_bytes());
            data
        };
        let mut stream = Vec::new();
        record(&mut stream, 0x0809, &bof(5));
        for (kind, data) in globals {
            record(&mut stream, *kind, data);
        }
        let mut offsets = Vec::new();
        for i in 0..sheets.len() {
            offsets.push(stream.len() + 4);
            let mut data = vec![0, 0, 0, 0, 0, 0, 1, 0];
            data.push(b'A' + i as u8);
            record(&mut stream, 0x0085, &data);
        }
        record(&mut stream, 0x000A, &[]);
        for (i, records) in sheets.iter().enumerate() {
            let offset = stream.len() as u32;
            stream[offsets[i]..offsets[i] + 4].copy_from_slice(&offset.to_le_bytes());
            record(&mut stream, 0x0809, &bof(0x0010));
            for (kind, data) in records {
                record(&mut stream, *kind, data);
            }
            record(&mut stream, 0x000A, &[]);
        }
        assert!(stream.len() <= 512);
        let mut book = small_book(
            &[0xFFFF_FFFD, END_OF_CHAIN, END_OF_CHAIN, END_OF_CHAIN],
            0,
            stream.len() as u32,
            &[1, 2, 3, 4, 5, 6, 7, END_OF_CHAIN],
        );
        book[1024 + 120..1024 + 124].copy_from_slice(&512u32.to_le_bytes());
        book[2048..2048 + stream.len()].copy_from_slice(&stream);
        book
    }

    /// BIFF fields are checked before calamine runs, including browser entry points (#1495).
    #[test]
    fn xls_biff_records_are_checked_before_the_parser() {
        let refuses = |book: &[u8], kind: &str| {
            assert!(check_compound(book).is_err(), "{kind} reached the unchecked BIFF parser");
            assert!(sheet_names(book).is_err(), "{kind}: names");
            assert!(columns(book, None).is_err(), "{kind}: columns");
            assert!(read(book, "bad.xls", None, &CsvOptions::default()).is_err(), "{kind}: read");
        };
        for kind in [0x002F, 0x0042, 0x0022, 0x0809, 0x0085, 0x0018, 0x0017] {
            refuses(&biff_book(&[(kind, vec![])], &[]), &format!("short global {kind:x}"));
        }
        for len in 0..14 {
            refuses(&biff_book(&[(0x0018, vec![0; len])], &[]), "short defined name");
        }
        let mut name = vec![0; 15];
        name[4..6].copy_from_slice(&100u16.to_le_bytes());
        refuses(&biff_book(&[(0x0018, name)], &[]), "defined-name formula length");
        for token in [0x3a, 0x3b, 0x3c, 0x3d, 0x5a, 0x7b] {
            let mut name = vec![0; 16];
            name[3] = 1;
            name[4..6].copy_from_slice(&1u16.to_le_bytes());
            name[15] = b'N';
            name.push(token);
            refuses(&biff_book(&[(0x0018, name)], &[]), "short defined-name reference");
        }
        for kind in [0x0203, 0x0204, 0x0205, 0x027E, 0x00FD, 0x00BD, 0x00E5, 0x0006] {
            refuses(&biff_book(&[], &[vec![(kind, vec![])]]), &format!("short cell record {kind:x}"));
        }
        let mut formula = vec![0; 22];
        formula[20..22].copy_from_slice(&200u16.to_le_bytes());
        refuses(&biff_book(&[], &[vec![(0x0006, formula)]]), "cell formula length");
        let mut reversed = vec![0; 6];
        reversed[2..4].copy_from_slice(&10u16.to_le_bytes());
        reversed[4..6].copy_from_slice(&1u16.to_le_bytes());
        refuses(&biff_book(&[], &[vec![(0x00bd, reversed)]]), "reversed MulRk columns");
        refuses(&biff_book(&[], &[vec![(0x00e5, vec![0xff, 0xff])]]), "merge count past its record");
        for token in [0x3a, 0x3b, 0x3c, 0x3d, 0x18, 0x19, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x2a, 0x2b, 0x39] {
            let mut formula = vec![0; 22];
            formula[20..22].copy_from_slice(&1u16.to_le_bytes());
            formula.push(token);
            refuses(&biff_book(&[], &[vec![(0x0006, formula)]]), &format!("short formula token {token:x}"));
        }
        for flags in [0x08, 0x04, 0x0c] {
            let mut sst = vec![0; 8];
            sst.extend([1, 0, flags]);
            refuses(&biff_book(&[(0x00fc, sst)], &[]), "short rich string header");
        }
        let mut sst = vec![0; 8];
        sst.extend([1, 0, 0]);
        refuses(&biff_book(&[(0x00fc, sst), (0x003c, vec![])], &[]), "empty string continuation");
        let mut offset = vec![0; 9];
        offset[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        offset[6] = 1;
        offset[8] = b'X';
        refuses(&biff_book(&[(0x0085, offset)], &[]), "sheet past Workbook");
    }

    /// Declared and actual cell spans, across every eagerly read sheet, have one bound (#1495).
    #[test]
    fn xls_biff_allocations_are_bounded_before_the_parser() {
        let dimensions = |first: u32, end: u32, cols: u16| {
            let mut data = Vec::new();
            data.extend(first.to_le_bytes());
            data.extend(end.to_le_bytes());
            data.extend(0u16.to_le_bytes());
            data.extend(cols.to_le_bytes());
            data.extend(0u16.to_le_bytes());
            (0x0200, data)
        };
        for dims in [dimensions(50, 1, 1), dimensions(0, u32::MAX, 256)] {
            let book = biff_book(&[], &[vec![dims]]);
            assert!(check_compound(&book).is_err(), "Dimensions reached the unchecked allocator");
        }
        let mut number = vec![0; 14];
        number[..2].copy_from_slice(&u16::MAX.to_le_bytes());
        number[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        let book = biff_book(&[], &[vec![(0x0203, vec![0; 14]), (0x0203, number)]]);
        assert!(check_compound(&book).is_err(), "far-apart cells reached the dense-grid allocator");
        let book = biff_book(&[], &[vec![dimensions(0, 65_536, 256)], vec![dimensions(0, 65_536, 256)]]);
        assert!(check_compound(&book).is_err(), "the budget applies to all sheets, including unselected ones");
        // Normal cached formula values remain readable, and names-only reads still work.
        let mut header = vec![0; 6];
        header.extend(7.0f64.to_le_bytes());
        let mut formula = vec![0; 22];
        formula[..2].copy_from_slice(&1u16.to_le_bytes());
        formula[6..14].copy_from_slice(&8.0f64.to_le_bytes());
        formula[20..22].copy_from_slice(&3u16.to_le_bytes());
        formula.extend([0x1e, 8, 0]);
        let book = biff_book(&[], &[vec![(0x0203, header), (0x0006, formula)]]);
        assert!(check_compound(&book).is_ok());
        assert_eq!(sheet_names(&book).unwrap(), ["A"]);
        let (ds, report) = read(&book, "clean.xls", None, &CsvOptions::default()).unwrap();
        assert_eq!(ds.features.len(), 1);
        assert!(report.messages.is_empty());
    }

    #[test]
    fn xls_biff_mutations_accepted_by_the_guard_do_not_panic() {
        let mut header = vec![0; 6];
        header.extend(7.0f64.to_le_bytes());
        let mut formula = vec![0; 22];
        formula[..2].copy_from_slice(&1u16.to_le_bytes());
        formula[6..14].copy_from_slice(&8.0f64.to_le_bytes());
        formula[20..22].copy_from_slice(&3u16.to_le_bytes());
        formula.extend([0x1e, 8, 0]);
        let mut sst = vec![0; 8];
        sst[..4].copy_from_slice(&2u32.to_le_bytes());
        sst[4..8].copy_from_slice(&2u32.to_le_bytes());
        sst.extend([4, 0, 0]);
        sst.extend(b"name");
        sst.extend([4, 0, 0]);
        sst.extend(b"city");
        let mut first = vec![0; 10];
        let mut second = first.clone();
        second[..2].copy_from_slice(&1u16.to_le_bytes());
        second[6..10].copy_from_slice(&1u32.to_le_bytes());
        first[6..10].copy_from_slice(&0u32.to_le_bytes());
        for seed in [
            biff_book(&[], &[vec![(0x0203, header), (0x0006, formula)]]),
            biff_book(&[(0x00fc, sst)], &[vec![(0x00fd, first), (0x00fd, second)]]),
        ] {
            assert!(check_compound(&seed).is_ok());
            assert!(open_workbook_auto_from_rs(Cursor::new(seed.as_slice())).is_ok());
            let len = u32::from_le_bytes(seed[1272..1276].try_into().unwrap()) as usize;
            for i in 0..len {
                for value in [0, 0x80, 0xff] {
                    let mut mutated = seed.clone();
                    mutated[2048 + i] = value;
                    if check_compound(&mutated).is_ok() {
                        let result = std::panic::catch_unwind(|| open_workbook_auto_from_rs(Cursor::new(mutated.as_slice())));
                        assert!(result.is_ok(), "BIFF byte {i}, value {value:x}, passed the guard but panicked");
                    }
                }
            }
        }
    }

    #[test]
    fn xls_sector_tables_are_checked() {
        const FAT: u32 = 0xFFFF_FFFD;
        const END: u32 = 0xFFFF_FFFE;
        let damaged = |book: &[u8], what: &str| {
            let e = read(book, "t.xls", None, &CsvOptions::default()).unwrap_err().to_string();
            assert!(e.contains("damaged") && e.contains(what), "{what}: {e}");
            assert!(columns(book, None).is_err() && sheet_names(book).is_err(), "{what}");
        };
        // The tables are sound: the check passes it on, and calamine reads it (it has no sheets).
        let sound = small_book(&[FAT, END, END, END], 0, 64, &[END]);
        assert!(check_compound(&sound).is_ok());
        // A directory chain that links past the end of the file (it holds sectors 0 to 3), or to
        // a sector after a file cut short mid-sector (calamine reads that one only if an earlier
        // read happened to reach that far), or that loops.
        damaged(&small_book(&[FAT, 0x100, END, END], 0, 64, &[END]), "runs past the end");
        damaged(&small_book(&[FAT, 5, END, END], 0, 64, &[END]), "runs past the end");
        let mut cut = small_book(&[FAT, 4, END, END], 0, 64, &[END]);
        cut.truncate(cut.len() - 100);
        damaged(&cut, "runs past the end");
        damaged(&small_book(&[FAT, 1, END, END], 0, 64, &[END]), "loops");
        // The directory's chain links past the FAT (128 entries) from a sector the file holds.
        let mut far = ole2(9, 1, 128, 0xFFFF_FFFE, 0, &sectors(&[FAT, END], 0xFF));
        far.resize(512 * 131, 0);
        damaged(&far, "allocation table");
        // calamine stops at a stream's length without looking up the last sector's link: a junk
        // link there (the root entry's, 64 bytes in one sector) passes.
        assert!(check_compound(&small_book(&[FAT, END, END, 0x100], 0, 64, &[END])).is_ok());
        // Unused FAT entries left as zeros, as some writers do, aren't followed and pass.
        let mut zeros = vec![FAT, END, END, END];
        zeros.resize(128, 0);
        assert!(check_compound(&small_book(&zeros, 0, 64, &[END])).is_ok());
        // Small streams: one that starts past the mini stream (one 64-byte mini sector), one whose
        // chain runs past it, one that loops on an empty mini sector, and an empty one, whose
        // first sector calamine reads all the same.
        damaged(&small_book(&[FAT, END, END, END], 200, 100, &[END]), "starts past");
        damaged(&small_book(&[FAT, END, END, END], 0, 100, &[2, END]), "runs past");
        damaged(&small_book(&[FAT, END, END, END], 1, 100, &[0, 1]), "loops");
        damaged(&small_book(&[FAT, END, END, END], 200, 0, &[END]), "starts past");
        // A mini sector starting right at the end of the mini stream reads as empty, and passes.
        assert!(check_compound(&small_book(&[FAT, END, END, END], 0, 100, &[1, END])).is_ok());
        // Without a mini FAT there is no mini stream: an empty stream at sector 0 reads nothing
        // and passes, one with bytes in it has no table to follow.
        let no_mini = |start: u32, len: u32| {
            let mut b = small_book(&[FAT, END, END, END], start, len, &[END]);
            b[64..68].copy_from_slice(&0u32.to_le_bytes());
            b
        };
        assert!(check_compound(&no_mini(0, 0)).is_ok());
        damaged(&no_mini(0, 100), "allocation table");
        // calamine drops a byte-order mark from a name: "\u{FEFF}Workbook" is the Workbook stream.
        let mut bom = small_book(&[FAT, END, END, END], 200, 100, &[END]);
        let name: Vec<u8> = "\u{FEFF}Workbook".encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let entry = 512 + 512 + 128;
        bom[entry..entry + name.len()].copy_from_slice(&name);
        bom[entry + 64..entry + 66].copy_from_slice(&(name.len() as u16).to_le_bytes());
        damaged(&bom, "starts past");
        // A DIFAT chain whose next sector is past the end of the file.
        damaged(&ole2(9, 1, 0, 50, 1, &sectors(&[FAT, END], 0xFF)), "past the end");
    }

    #[test]
    fn a_panic_in_calamine_is_an_error() {
        let e = guarded(|| -> Result<()> { panic!("calamine indexed past a table") }).unwrap_err();
        assert!(e.to_string().contains("damaged"), "{e}");
    }

    #[test]
    fn letters_count_from_column_a() {
        // The data starts in column C: a column without a heading is still named by its letter.
        let book = xlsx(&[(
            "S",
            &[
                &[r#"<c r="C1" t="inlineStr"><is><t>name</t></is></c>"#],
                &[r#"<c r="C2" t="inlineStr"><is><t>x</t></is></c>"#, r#"<c r="D2" t="inlineStr"><is><t>y</t></is></c>"#],
            ],
        )]);
        let (ds, _) = read(&book, "t", None, &CsvOptions::default()).unwrap();
        let names: Vec<&str> = ds.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["name", "D"]);
    }
}
