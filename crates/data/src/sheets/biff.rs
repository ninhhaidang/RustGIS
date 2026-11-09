//! Bounds for BIFF records before calamine eagerly reads every XLS sheet.
//!
//! Record layouts: Microsoft [MS-XLS], sections 2.4 (records) and 2.5.198 (formula tokens):
//! <https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-xls/6fba0383-0d7a-4c7a-afe9-642ff70cbd36>.
//! This checks lengths and allocation spans, without evaluating formulas or decoding cells.

type Result<T> = std::result::Result<T, String>;

fn short() -> String {
    "a BIFF record or formula token is cut short".into()
}

fn bytes(data: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    data.get(at..at.saturating_add(len)).ok_or_else(short)
}

fn byte(data: &[u8], at: usize) -> Result<u8> {
    data.get(at).copied().ok_or_else(short)
}

fn word(data: &[u8], at: usize) -> Result<u16> {
    bytes(data, at, 2)?.try_into().map(u16::from_le_bytes).map_err(|_| short())
}

fn dword(data: &[u8], at: usize) -> Result<u32> {
    bytes(data, at, 4)?.try_into().map(u32::from_le_bytes).map_err(|_| short())
}

struct Record<'a> {
    kind: u16,
    data: &'a [u8],
    /// Encoded CONTINUE records following this record.
    cont: &'a [u8],
    next: usize,
}

fn record(stream: &[u8], at: usize) -> Result<Record<'_>> {
    let kind = word(stream, at)?;
    let len = usize::from(word(stream, at.saturating_add(2))?);
    let start = at.saturating_add(4);
    let data = bytes(stream, start, len)?;
    let end = start + len;
    let mut next = end;
    // Match the reader: a final four-byte CONTINUE header is an independent record.
    while stream.len().saturating_sub(next) > 4 && word(stream, next)? == 0x003c {
        let len = usize::from(word(stream, next + 2)?);
        bytes(stream, next + 4, len)?;
        next += 4 + len;
    }
    Ok(Record { kind, data, cont: bytes(stream, end, next - end)?, next })
}

/// Text and dense cells allocated while opening the whole workbook, not just the chosen sheet.
struct Budget {
    max_cells: usize,
    cells: usize,
    text: usize,
    /// Sheet scans together may visit at most one Workbook stream's worth of bytes.
    work: usize,
}

impl Budget {
    fn cells(&mut self, count: usize) -> Result<()> {
        self.cells = self.cells.saturating_add(count);
        if self.cells > self.max_cells {
            return Err(format!(
                "the XLS workbook is too large (more than {} allocated entries across its sheets and metadata)",
                self.max_cells
            ));
        }
        Ok(())
    }

    fn text(&mut self, count: usize) -> Result<()> {
        self.text = self.text.saturating_add(count);
        if self.text > self.max_cells.saturating_mul(8) {
            return Err("the XLS workbook's expanded strings, names and formulas are too large".into());
        }
        Ok(())
    }
}

#[derive(Default)]
struct Span {
    bounds: Option<(u16, u16, u16, u16)>,
    count: usize,
}

impl Span {
    fn add(&mut self, row: u16, col: u16) -> Result<()> {
        // BIFF8 has 65,536 rows and 256 columns. A 16-bit cell column must not become a
        // 65,536-column dense range just because it fits in the record's storage field.
        if col > 255 {
            return Err("a BIFF cell is past column IV (256 columns)".into());
        }
        self.bounds = Some(self.bounds.map_or((row, row, col, col), |(a, b, c, d)| (a.min(row), b.max(row), c.min(col), d.max(col))));
        self.count = self.count.saturating_add(1);
        Ok(())
    }

    fn cells(&self) -> usize {
        self.bounds.map_or(0, |(a, b, c, d)| (usize::from(b - a) + 1).saturating_mul(usize::from(d - c) + 1)).max(self.count)
    }
}

/// Refuse malformed records and excessive allocations before the XLS reader is entered.
pub(crate) fn check(stream: &[u8], max_cells: usize) -> Result<()> {
    let mut at = 0;
    let mut biff8 = true;
    let mut sheets = Vec::new();
    let mut strings = Vec::new();
    let mut budget = Budget { max_cells, cells: 0, text: 0, work: stream.len() };
    while at < stream.len() {
        let r = record(stream, at)?;
        at = r.next;
        match r.kind {
            0x000a => break,
            0x0809 => {
                let version = word(r.data, 0)?;
                biff8 = !matches!(version, 0x0002 | 0x0007 | 0x0200 | 0x0300 | 0x0400 | 0x0500)
                    && !(version == 0 && word(r.data, 2).unwrap_or(0) == 0x1000);
            }
            0x002f => {
                word(r.data, 0)?;
                return Err("the XLS workbook is encrypted or obfuscated and cannot be read".into());
            }
            0x0042 | 0x0022 => {
                word(r.data, 0)?;
            }
            0x0017 if biff8 => {
                let count = usize::from(word(r.data, 0)?);
                bytes(r.data, 2, count * 6)?;
                budget.cells(count)?;
            }
            0x00e0 => {
                // The reader retains each XF, then allocates its resolved format table.
                bytes(r.data, 0, 4)?;
                budget.cells(1)?;
            }
            0x0085 => {
                let offset = usize::try_from(dword(r.data, 0)?).map_err(|_| "a sheet offset cannot be addressed")?;
                if offset > stream.len() {
                    return Err("a sheet starts past the end of the Workbook stream".into());
                }
                let chars = usize::from(byte(r.data, 6)?);
                let prefix = if biff8 { 8 } else { 7 };
                let width = if biff8 && byte(r.data, 7)? & 1 != 0 { 2 } else { 1 };
                bytes(r.data, prefix, chars * width)?;
                // Two sheet-name collections and the empty sheet's state are retained too.
                budget.text(512 + chars * 8)?;
                if sheets.len() >= max_cells {
                    return Err("the XLS workbook has too many sheets".into());
                }
                sheets.push(offset);
            }
            0x0018 => {
                defined_name(r.data, biff8)?;
                // Bound both copies of the name and its rendered reference, plus object storage.
                budget.text(256 + usize::from(byte(r.data, 3)?) * 8)?;
            }
            0x00fc => strings = shared_strings(&r, &mut budget)?,
            _ => {}
        }
    }
    sheets.sort_unstable();
    if sheets.windows(2).any(|pair| pair.first() == pair.get(1)) {
        return Err("XLS sheets share a Workbook stream offset".into());
    }
    for start in sheets {
        worksheet(stream, start, biff8, &strings, &mut budget)?;
    }
    Ok(())
}

fn defined_name(data: &[u8], biff8: bool) -> Result<()> {
    bytes(data, 0, 14)?;
    let chars = usize::from(byte(data, 3)?);
    let len = usize::from(word(data, 4)?);
    // calamine's parse_lbl reads the final cce bytes, including for names with rgcb data.
    // Guard the bytes the reader actually indexes, rather than interpreting a different slice.
    let start = data.len().checked_sub(len).ok_or_else(short)?;
    let prefix = if biff8 { 15 } else { 14 };
    let width = if biff8 && byte(data, 14)? & 1 != 0 { 2 } else { 1 };
    if start < prefix + chars * width {
        return Err("a defined name or its formula is cut short".into());
    }
    let formula = bytes(data, start, len)?;
    let need = match formula.first().copied() {
        Some(0x3a | 0x5a | 0x7a) => {
            if biff8 {
                7
            } else {
                18
            }
        }
        Some(0x3b | 0x5b | 0x7b) => {
            if biff8 {
                11
            } else {
                21
            }
        }
        Some(0x3c | 0x5c | 0x7c | 0x3d | 0x5d | 0x7d) => {
            // parse_defined_names reads only the external-sheet index for an error token.
            if biff8 { 3 } else { 13 }
        }
        _ => 0,
    };
    bytes(formula, 0, need)?;
    Ok(())
}

/// Remaining pieces of an SST, preserving the character encoding flag on each continuation.
struct Parts<'a> {
    data: &'a [u8],
    cont: &'a [u8],
}

impl Parts<'_> {
    fn advance(&mut self) -> Result<bool> {
        if self.cont.is_empty() {
            return Ok(false);
        }
        let len = usize::from(word(self.cont, 2)?);
        self.data = bytes(self.cont, 4, len)?;
        self.cont = self.cont.get(4 + len..).ok_or_else(short)?;
        Ok(true)
    }

    fn skip(&mut self, mut count: usize) -> Result<()> {
        while count > 0 {
            if self.data.is_empty() && !self.advance()? {
                return Err(short());
            }
            let n = count.min(self.data.len());
            self.data = self.data.get(n..).ok_or_else(short)?;
            count -= n;
        }
        Ok(())
    }
}

fn shared_strings(record: &Record<'_>, budget: &mut Budget) -> Result<Vec<usize>> {
    let mut parts = Parts { data: record.data.get(8..).ok_or_else(short)?, cont: record.cont };
    let mut lengths = Vec::new();
    loop {
        if parts.data.is_empty() && !parts.advance()? {
            break;
        }
        // An empty continuation at a string boundary is harmless to the reader.
        if parts.data.is_empty() {
            continue;
        }
        let chars = usize::from(word(parts.data, 0)?);
        let flags = byte(parts.data, 2)?;
        let mut prefix = 3;
        let runs = if flags & 8 != 0 {
            prefix += 2;
            usize::from(word(parts.data, 3)?)
        } else {
            0
        };
        let extension = if flags & 4 != 0 {
            let size = dword(parts.data, prefix)?;
            prefix += 4;
            if size > i32::MAX as u32 {
                return Err("a BIFF string has a negative extension length".into());
            }
            usize::try_from(size).map_err(|_| short())?
        } else {
            0
        };
        parts.data = parts.data.get(prefix..).ok_or_else(short)?;
        let mut left = chars;
        let mut wide = flags & 1 != 0;
        while left > 0 {
            let width = if wide { 2 } else { 1 };
            let n = left.min(parts.data.len() / width);
            parts.data = parts.data.get(n * width..).ok_or_else(short)?;
            left -= n;
            if left > 0 {
                if !parts.advance()? {
                    return Err(short());
                }
                wide = byte(parts.data, 0)? & 1 != 0;
                parts.data = parts.data.get(1..).ok_or_else(short)?;
            }
        }
        parts.skip(runs * 4)?;
        parts.skip(extension)?;
        if lengths.len() >= budget.max_cells {
            return Err("the XLS shared-string table is too large".into());
        }
        // Even an empty string retains a String plus this guard's length entry.
        budget.cells(1)?;
        budget.text(32usize.saturating_add(chars))?;
        lengths.push(chars);
    }
    Ok(lengths)
}

/// The inline strings decoded directly from one record (LABEL/RSTRING and formula STRING).
/// Calamine clamps a short body rather than reading CONTINUE records for these strings.
fn inline_string(data: &[u8], at: usize, biff8: bool) -> Result<usize> {
    let chars = usize::from(word(data, at)?);
    if biff8 && chars == 0 && data.len() == at.saturating_add(2) {
        return Ok(0);
    }
    let width = if biff8 && byte(data, at.saturating_add(2))? & 1 != 0 { 2 } else { 1 };
    let start = at.saturating_add(if biff8 { 3 } else { 2 });
    bytes(data, start, chars * width)
        .map_err(|_| "an XLS inline string is cut short or continues beyond this reader's supported record".to_string())?;
    Ok(chars)
}

fn dimensions(data: &[u8]) -> Result<usize> {
    let (first, end, col, last) = match data.len() {
        10 => (u32::from(word(data, 0)?), u32::from(word(data, 2)?), word(data, 4)?, word(data, 6)?),
        14 | 16 => (dword(data, 0)?, dword(data, 4)?, word(data, 8)?, word(data, 10)?),
        _ => return Err("a BIFF Dimensions record has the wrong length".into()),
    };
    if first > 65_535
        || end > 65_536
        || col > 255
        || last > 256
        || (end > 0 && end <= first)
        || last < col
        || (end > 0 && last > 0 && last == col)
    {
        return Err("a BIFF Dimensions record has reversed or out-of-range bounds".into());
    }
    let rows = if end == 0 { 1 } else { (end - first) as usize };
    let cols = if last == 0 { 1 } else { usize::from(last - col) };
    Ok(rows.saturating_mul(cols.max(1)))
}

fn worksheet(stream: &[u8], start: usize, biff8: bool, strings: &[usize], budget: &mut Budget) -> Result<()> {
    let mut at = start;
    let mut formula_position = (0u16, 0u16);
    let mut seen_bof = false;
    let mut depth = 0usize;
    let mut cells = Span::default();
    let mut formulas = Span::default();
    let mut declared = 0;
    while at < stream.len() {
        let r = record(stream, at)?;
        budget.work = budget.work.checked_sub(r.next - at).ok_or("XLS sheet scans overlap or exceed the Workbook stream size")?;
        at = r.next;
        if depth > 0 {
            match r.kind {
                0x0809 => depth += 1,
                0x000a => depth -= 1,
                _ => {}
            }
            continue;
        }
        match r.kind {
            0x000a => break,
            0x0809 => {
                if seen_bof {
                    depth += 1;
                } else {
                    seen_bof = true;
                }
            }
            0x0200 => declared = declared.max(dimensions(r.data)?),
            0x00e5 => {
                let n = usize::from(word(r.data, 0)?);
                bytes(r.data, 2, n * 8)?;
                budget.cells(n)?;
            }
            0x00bd => {
                bytes(r.data, 0, 6)?;
                let row = word(r.data, 0)?;
                let first = word(r.data, 2)?;
                let last = word(r.data, r.data.len() - 2)?;
                let count = last.checked_sub(first).and_then(|n| n.checked_add(1)).ok_or_else(short)?;
                if r.data.len() != 6 + usize::from(count) * 6 {
                    return Err("a BIFF MulRk record has inconsistent columns".into());
                }
                for col in first..=last {
                    cells.add(row, col)?;
                }
            }
            0x0203 | 0x0204 | 0x00d6 | 0x0205 | 0x027e | 0x00fd | 0x0006 => {
                let min = match r.kind {
                    0x0203 => 14,
                    0x0204 | 0x00d6 => 6,
                    0x0205 => 8,
                    0x0006 => 22,
                    _ => 10,
                };
                bytes(r.data, 0, min)?;
                let (row, col) = (word(r.data, 0)?, word(r.data, 2)?);
                // A cached string is materialized by its following STRING record, not FORMULA.
                let cached_string = r.kind == 0x0006 && byte(r.data, 6)? == 0 && bytes(r.data, 12, 2)? == [0xff, 0xff];
                if !cached_string {
                    cells.add(row, col)?;
                }
                if r.kind == 0x0006 {
                    formula_position = (word(r.data, 0)?, word(r.data, 2)?);
                    formulas.add(row, col)?;
                    formula(r.data.get(20..).ok_or_else(short)?, biff8, budget)?;
                } else if r.kind == 0x00fd {
                    if let Some(len) = strings.get(dword(r.data, 6)? as usize) {
                        budget.text(*len)?;
                    }
                } else if matches!(r.kind, 0x0204 | 0x00d6) {
                    budget.text(inline_string(r.data, 6, biff8)?)?;
                }
            }
            0x0207 => {
                budget.text(inline_string(r.data, 0, biff8)?)?;
                cells.add(formula_position.0, formula_position.1)?;
            }
            _ => {}
        }
        if cells.cells().max(declared).saturating_add(formulas.cells()) > budget.max_cells {
            return Err(format!("an XLS sheet is too large (more than {} cells)", budget.max_cells));
        }
    }
    budget.cells(cells.cells().max(declared).saturating_add(formulas.cells()))
}

/// Check token operands the reader indexes, without computing a formula or following references.
fn formula(data: &[u8], biff8: bool, budget: &mut Budget) -> Result<()> {
    let len = usize::from(word(data, 0)?);
    let mut tokens = bytes(data, 2, len)?;
    while let Some((&raw, rest)) = tokens.split_first() {
        // Operand-class variants have identical record widths.
        let kind = if raw >= 0x40 { (raw & 0x1f) | 0x20 } else { raw };
        let need = match kind {
            0x01 => 4,
            0x03..=0x16 => 0,
            0x17 => {
                let chars = usize::from(byte(rest, 0)?);
                let prefix = if biff8 { 2 } else { 1 };
                let width = if biff8 && byte(rest, 1)? & 1 != 0 { 2 } else { 1 };
                budget.text(chars * 4 + 2)?;
                prefix + chars * width
            }
            0x18 => 5,
            0x19 => match byte(rest, 0)? {
                0x04 => 3 + 2 * (usize::from(word(rest, 1)?) + 1),
                0x01 | 0x02 | 0x08 | 0x10 | 0x20 | 0x21 | 0x40 | 0x41 => {
                    budget.text(usize::from(byte(rest, 2)?))?;
                    3
                }
                _ => return Ok(()), // An unknown token is a checked error in calamine.
            },
            0x1c | 0x1d => 1,
            0x1e | 0x21 => 2,
            0x1f => 8,
            0x20 => 7,
            0x22 => 3,
            0x23 => {
                if dword(rest, 0)? == 0 {
                    return Err("a BIFF name reference has index zero".into());
                }
                budget.text(1024)?;
                4
            }
            0x24 | 0x2a => {
                if biff8 {
                    4
                } else {
                    3
                }
            }
            0x25 | 0x2b => {
                if biff8 {
                    8
                } else {
                    6
                }
            }
            0x39 | 0x3a | 0x3c => 6,
            0x3b | 0x3d => 10,
            _ => return Ok(()),
        };
        bytes(rest, 0, need)?;
        // calamine currently adds one in u16 arithmetic for these row references. A valid
        // last-row reference must be refused explicitly until that reader can handle it safely.
        if (kind == 0x3a && word(rest, 2)? == u16::MAX) || (kind == 0x24 && biff8 && word(rest, 0)? == u16::MAX) {
            return Err("the XLS formula reader cannot safely handle a reference to row 65536".into());
        }
        budget.text(if matches!(kind, 0x39..=0x3d) { 160 } else { 32 })?;
        tokens = rest.get(need..).ok_or_else(short)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_string_formulas_use_one_value_and_one_formula_slot() {
        let mut stream = Vec::new();
        for row in 0..3u16 {
            let mut data = vec![0; 22];
            data[..2].copy_from_slice(&row.to_le_bytes());
            data[12..14].fill(0xff);
            // The reader retains cached values even if the formula body is empty.
            stream.extend([6, 0, 22, 0]);
            stream.extend(data);
            stream.extend([7, 2, 3, 0, 0, 0, 0]);
        }
        stream.extend([10, 0, 0, 0]);
        let mut budget = Budget { max_cells: 6, cells: 0, text: 0, work: stream.len() };
        assert_eq!(worksheet(&stream, 0, true, &[], &mut budget), Ok(()));
    }

    #[test]
    fn external_sheet_counts_require_their_declared_entries() {
        assert!(check(&[0x17, 0, 8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 0], 64).is_err());
        assert!(check(&[0x17, 0, 8, 0, 1, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 0], 64).is_ok());
    }

    #[test]
    fn repeated_empty_formula_string_values_use_the_cell_budget() {
        let mut stream = [0x07, 0x02, 3, 0, 0, 0, 0].repeat(20);
        stream.extend([0x0a, 0, 0, 0]);
        let mut budget = Budget { max_cells: 8, cells: 0, text: 0, work: stream.len() };
        assert!(worksheet(&stream, 0, true, &[], &mut budget).is_err());
    }

    #[test]
    fn inline_labels_and_formula_strings_require_their_declared_body() {
        for biff8 in [false, true] {
            for (kind, offset) in [(0x0204u16, 6usize), (0x00d6, 6), (0x0207, 0)] {
                for chars in [100u16, 1] {
                    let mut data = vec![0; offset + 2 + usize::from(biff8)];
                    data[offset..offset + 2].copy_from_slice(&chars.to_le_bytes());
                    data.push(b'A');
                    let mut stream = kind.to_le_bytes().to_vec();
                    stream.extend((data.len() as u16).to_le_bytes());
                    stream.extend(data);
                    stream.extend([0x0a, 0, 0, 0]);
                    let mut budget = Budget { max_cells: 64, cells: 0, text: 0, work: stream.len() };
                    assert_eq!(
                        worksheet(&stream, 0, biff8, &[], &mut budget).is_ok(),
                        chars == 1,
                        "kind={kind:x}, biff8={biff8}, chars={chars}"
                    );
                }
            }
        }
    }

    #[test]
    fn xor_obfuscated_workbooks_are_refused() {
        assert!(check(&[0x2f, 0, 2, 0, 0, 0, 0x0a, 0, 0, 0], 64).is_err_and(|e| e.contains("encrypted")));
    }

    #[test]
    fn encrypted_workbooks_are_refused_before_unchecked_records() {
        assert!(check(&[0x2f, 0, 2, 0, 1, 0, 0xff], 64).is_err_and(|error| error.contains("encrypted")));
    }

    #[test]
    fn empty_shared_strings_use_the_shared_allocation_budget() {
        let mut data = 3u32.to_le_bytes().repeat(2);
        data.extend([0u8; 9]);
        let mut stream = vec![0xfc, 0, 17, 0];
        stream.extend(data);
        stream.extend([0x0a, 0, 0, 0]);
        assert!(check(&stream, 8).is_err(), "empty strings still allocate table entries");
        assert!(check(&stream, 64).is_ok());
    }

    #[test]
    fn xls_review_equal_nonzero_column_bounds_are_refused() {
        let mut data = vec![0; 14];
        data[4..8].copy_from_slice(&10u32.to_le_bytes());
        data[8..10].copy_from_slice(&2u16.to_le_bytes());
        data[10..12].copy_from_slice(&2u16.to_le_bytes());
        assert!(dimensions(&data).is_err(), "calamine subtracts one from the exclusive column end");
        data[8..12].fill(0);
        assert!(dimensions(&data).is_ok(), "zero column bounds remain an empty-sheet sentinel");
    }

    #[test]
    fn xls_review_metadata_shares_the_allocation_budget() {
        let mut name = vec![0; 15];
        name[3] = 1;
        name[4..6].copy_from_slice(&3u16.to_le_bytes());
        name.extend([b'N', 0x1e, 1, 0]);
        for (kind, data) in [(0x18u16, name), (0xe0, vec![0; 20]), (0x17, vec![1, 0, 0, 0, 0, 0, 0, 0])] {
            let mut record = kind.to_le_bytes().to_vec();
            record.extend((data.len() as u16).to_le_bytes());
            record.extend(data);
            let mut one = record.clone();
            one.extend([0x0a, 0, 0, 0]);
            assert!(check(&one, 64).is_ok(), "one metadata entry fits: {kind:x}");
            let mut many = record.repeat(100);
            many.extend([0x0a, 0, 0, 0]);
            assert!(
                check(&many, 64).is_err_and(|error| error.contains("too large")),
                "metadata cannot allocate outside the shared budget: {kind:x}"
            );
        }
    }

    #[test]
    fn repeated_and_overlapping_empty_sheets_have_bounded_work() {
        let mut stream = Vec::new();
        let offset = (500 * 13 + 4) as u32;
        for _ in 0..500 {
            stream.extend([0x85, 0, 9, 0]);
            stream.extend(offset.to_le_bytes());
            stream.extend([0, 0, 1, 0, b'A']);
        }
        stream.extend([0x0a, 0, 0, 0]);
        for _ in 0..1000 {
            stream.extend([0xff, 0, 0, 0]);
        }
        stream.extend([0x0a, 0, 0, 0]);
        assert!(check(&stream, 1_000_000).is_err_and(|e| e.contains("share")));
        // Different offsets into the same cell-less substream must not multiply scan work.
        for i in 0..500 {
            stream[i * 13 + 4..i * 13 + 8].copy_from_slice(&(offset + i as u32 * 4).to_le_bytes());
        }
        assert!(check(&stream, 1_000_000).is_err_and(|e| e.contains("overlap")));
    }
}
