//! Allocation preflight for calamine's eager ODS reader. Scan without expanding repeats or
//! building an XML tree. Empty leading/trailing rows and trailing cells need no dense grid.

use quick_xml::{Reader, XmlVersion, events::BytesStart, events::Event};

use crate::{DataError, Result};

fn large() -> DataError {
    DataError::Format("spreadsheet: ODS repeated cells or text are too large; reduce the workbook's rows, columns or repetitions".into())
}

fn xml_error(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("spreadsheet: damaged ODS content.xml ({e})"))
}

fn add(a: usize, b: usize, limit: usize) -> Result<usize> {
    a.checked_add(b).filter(|n| *n <= limit).ok_or_else(large)
}

fn mul(a: usize, b: usize, limit: usize) -> Result<usize> {
    a.checked_mul(b).filter(|n| *n <= limit).ok_or_else(large)
}

fn repeat(e: &BytesStart<'_>, key: &str, limit: usize) -> Result<usize> {
    for a in e.attributes() {
        let a = a.map_err(xml_error)?;
        if a.key.as_ref() == key {
            let value = a.normalized_value(XmlVersion::Implicit1_0).map_err(xml_error)?;
            let n = value.parse::<usize>().map_err(xml_error)?;
            return if n > 0 && n <= limit { Ok(n) } else { Err(large()) };
        }
    }
    Ok(1)
}

#[derive(Default)]
struct Sheet {
    rows: usize,
    stored: usize,
    extent: Option<(usize, usize, usize, usize)>,
}

struct Row {
    repeats: usize,
    columns: usize,
    stored: usize,
    extent: Option<(usize, usize)>,
    bytes: usize,
}

struct Cell {
    repeats: usize,
    occupied: bool,
    string: bool,
    bytes: usize,
    depth: usize,
    annotation: Option<usize>,
}

impl Cell {
    fn new(e: &BytesStart<'_>) -> Result<Self> {
        // calamine caps these dimensions, logging rather than reporting truncation. Refuse
        // counts beyond its supported dimensions instead of silently importing less data.
        let repeats = repeat(e, "table:number-columns-repeated", 16_384)?;
        let mut cell = Self { repeats, occupied: false, string: false, bytes: 0, depth: 1, annotation: None };
        for a in e.attributes() {
            let a = a.map_err(xml_error)?;
            match a.key.as_ref() {
                "office:value" | "office:boolean-value" => cell.occupied = true,
                "office:string-value" | "office:date-value" | "office:time-value" => {
                    cell.occupied = true;
                    cell.bytes = add(cell.bytes, a.value.len(), crate::zip::MAX_TOTAL)?;
                }
                "office:value-type" if a.value.as_ref() == "string" => {
                    cell.occupied = true; // Even String("") differs from calamine's Empty.
                    cell.string = true;
                }
                // Calamine also reads element text for error cells with an empty string value.
                "calcext:value-type" if a.value.as_ref() == "error" => cell.string = true,
                "table:formula" => {
                    cell.occupied |= !a.value.is_empty();
                    cell.bytes = add(cell.bytes, a.value.len(), crate::zip::MAX_TOTAL)?;
                }
                _ => {}
            }
        }
        Ok(cell)
    }
}

impl Sheet {
    fn row(&mut self, row: Row, limit: usize) -> Result<usize> {
        let end = add(self.rows, row.repeats, 1_048_576)?;
        // read_row keeps one copy of each encoded row, including empty cells between values.
        self.stored = add(self.stored, row.stored, limit)?;
        if let Some((left, right)) = row.extent {
            self.extent = Some(self.extent.map_or((self.rows, end, left, right), |(top, _, l, r)| (top, end, l.min(left), r.max(right))));
        }
        self.rows = end;
        self.cells(limit)
    }

    fn cells(&self, limit: usize) -> Result<usize> {
        self.extent.map_or(Ok(0), |(top, bottom, left, right)| mul(bottom - top, right - left, limit))
    }
}

/// Run before `open_workbook_auto_from_rs`: `Ods::new`, not `worksheet_range`, expands all
/// sheets. Both dense extents and cloned text share their existing workbook budgets. The
/// dimension caps and empty-cell handling follow calamine 0.38 (MIT), `ods::read_row/get_range`.
/// Cancel is checked between XML events; a single event is bounded by the ZIP byte budget.
pub(super) fn check(bytes: &[u8], cell_limit: usize, byte_limit: usize) -> Result<()> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut sheet: Option<Sheet> = None;
    let mut row: Option<Row> = None;
    let mut cell: Option<Cell> = None;
    let (mut total_cells, mut total_stored, mut total_bytes) = (0, 0, 0);
    loop {
        crate::progress::check()?;
        let event = reader.read_event().map_err(xml_error)?;
        if let Some(c) = cell.as_mut() {
            match &event {
                Event::Start(e) => {
                    let name = e.name();
                    if matches!(name.as_ref(), "table:table-cell" | "table:covered-table-cell") {
                        return Err(xml_error("nested cell"));
                    }
                    c.depth = add(c.depth, 1, bytes.len())?;
                    if name.as_ref() == "office:annotation" {
                        if c.annotation.is_some() {
                            return Err(xml_error("nested annotation"));
                        }
                        c.annotation = Some(c.depth);
                    }
                    if c.string && c.annotation.is_none() {
                        let n = if name.as_ref() == "text:s" { repeat(e, "text:c", byte_limit)? } else { 1 };
                        c.bytes = add(c.bytes, n, byte_limit)?;
                    }
                }
                Event::Text(t) if c.string && c.annotation.is_none() => c.bytes = add(c.bytes, t.len(), byte_limit)?,
                Event::GeneralRef(t) if c.string && c.annotation.is_none() => c.bytes = add(c.bytes, t.len(), byte_limit)?,
                Event::End(_) => {
                    if c.annotation == Some(c.depth) {
                        c.annotation = None;
                    }
                    c.depth -= 1;
                    if c.depth == 0 {
                        let Some(r) = row.as_mut() else { return Err(xml_error("cell outside a row")) };
                        let end = add(r.columns, c.repeats, 16_384)?;
                        if c.occupied {
                            r.extent = Some(r.extent.map_or((r.columns, end), |(left, _)| (left, end)));
                        }
                        // The final run of empty columns is pending, not pushed by read_row.
                        // A following cell flushes that run even when it is itself empty.
                        r.stored = if c.occupied { end } else { r.columns };
                        r.columns = end;
                        r.bytes = add(r.bytes, mul(c.bytes, c.repeats, byte_limit)?, byte_limit)?;
                        cell = None;
                    }
                }
                Event::Eof => return Err(xml_error("unfinished cell")),
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(e) => match e.name().as_ref() {
                "table:table" => {
                    if sheet.is_some() {
                        return Err(xml_error("nested table"));
                    }
                    sheet = Some(Sheet::default());
                }
                "table:table-row" if sheet.is_some() => {
                    if row.is_some() {
                        return Err(xml_error("nested row"));
                    }
                    row = Some(Row {
                        repeats: repeat(&e, "table:number-rows-repeated", 1_048_576)?,
                        columns: 0,
                        stored: 0,
                        extent: None,
                        bytes: 0,
                    });
                }
                "table:table-cell" | "table:covered-table-cell" if row.is_some() => cell = Some(Cell::new(&e)?),
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                "table:table-row" => {
                    if let (Some(s), Some(r)) = (sheet.as_mut(), row.take()) {
                        total_bytes = add(total_bytes, mul(r.bytes, r.repeats, byte_limit)?, byte_limit)?;
                        add(total_cells, s.row(r, cell_limit)?, cell_limit)?;
                        add(total_stored, s.stored, cell_limit)?;
                    }
                }
                "table:table" => {
                    if let Some(s) = sheet.take() {
                        total_cells = add(total_cells, s.cells(cell_limit)?, cell_limit)?;
                        total_stored = add(total_stored, s.stored, cell_limit)?;
                    }
                }
                _ => {}
            },
            Event::Eof => {
                return if sheet.is_none() && row.is_none() { Ok(()) } else { Err(xml_error("unfinished table")) };
            }
            _ => {}
        }
    }
}
