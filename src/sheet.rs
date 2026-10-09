use std::path::Path;

const CSV_BYTES_CAP: u64 = 25 * 1024 * 1024;
const XLSX_BYTES_CAP: u64 = 50 * 1024 * 1024;
/// The most cells one sheet's grid is built from, counting the dense
/// rectangle between its outermost values. The compressed-size cap above
/// doesn't bound this: repeat attributes, or two values at opposite
/// corners, expand a few hundred bytes to gigabytes (#1156).
const MAX_SHEET_CELLS: u64 = 10_000_000;
const MAX_COL_DISPLAY_W: u16 = 40;
const MIN_COL_DISPLAY_W: u16 = 3;

/// Source format the spreadsheet view was loaded from. Drives the small
/// header-line label and the parser dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetKind {
    Csv,
    Tsv,
    Xlsx,
    Xls,
    Ods,
    Xlsb,
    /// Read-only SQLite browser (#182): tables page as worksheets.
    Sqlite,
}

impl SheetKind {
    pub fn label(self) -> &'static str {
        match self {
            SheetKind::Csv => "CSV",
            SheetKind::Tsv => "TSV",
            SheetKind::Xlsx => "XLSX",
            SheetKind::Xls => "XLS",
            SheetKind::Ods => "ODS",
            SheetKind::Xlsb => "XLSB",
            SheetKind::Sqlite => "SQLITE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetView {
    pub kind: SheetKind,
    /// Raw file size on disk, for the header line.
    pub source_byte_size: u64,
    /// One entry per worksheet (single entry for CSV/TSV).
    pub sheets: Vec<SheetData>,
    pub current_sheet: usize,
    /// Unsaved cell/row/column changes (#177). Mirrored into the
    /// editor's dirty flag so tab dots, close guards, and the FS-sync
    /// conflict contract all behave exactly like text tabs.
    pub dirty: bool,
    /// The in-grid cell editor when a cell is being typed into.
    pub editing: Option<CellEdit>,
    /// Cells the user edited since the last save (#178): (sheet, body
    /// row, col). The xlsx save path applies EXACTLY these through umya,
    /// so untouched cells (formulas included) are never rewritten from
    /// calamine's formatted strings. CSV saves ignore it (whole-file
    /// serialisation).
    pub cell_edits: Vec<(usize, usize, usize)>,
    /// Frame-truth layout for mouse hit-testing, written by the render.
    pub grid: SheetGridLayout,
    /// SQLite paging state (#182/#201): per-sheet (table name, page).
    /// Empty for every other kind.
    pub sqlite_pages: Vec<(String, usize)>,
}

impl SheetView {
    /// Commit the cell being typed into at the current sheet's caret: the
    /// value goes in the cell, the cell joins `cell_edits` and the view turns
    /// dirty. Every commit (Enter, Tab, a click elsewhere, a save) goes
    /// through here, so none can skip the record the xlsx save writes from
    /// (#1140, #1141). Returns the committed cell, or None with no edit open.
    pub fn commit_edit(&mut self) -> Option<(usize, usize)> {
        let current = self.current_sheet;
        let data = self.sheets.get_mut(current)?;
        let edit = self.editing.take()?;
        let (r, c) = (data.cur_row, data.cur_col);
        data.set_cell(r, c, edit.value);
        if !self.cell_edits.contains(&(current, r, c)) {
            self.cell_edits.push((current, r, c));
        }
        self.dirty = true;
        Some((r, c))
    }

    /// Whether the grid takes edits: CSV, TSV and xlsx.
    pub fn editable(&self) -> bool {
        matches!(self.kind, SheetKind::Csv | SheetKind::Tsv | SheetKind::Xlsx)
    }
}

/// In-grid cell input state (#177): plain value + char cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellEdit {
    pub value: String,
    pub cursor: usize,
}

/// Geometry of the last painted grid frame (mouse hit-testing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SheetGridLayout {
    pub data_top: u16,
    pub data_rows: u16,
    pub body_x: u16,
    pub body_w: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetData {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Pre-computed display widths in cells (clamped to
    /// `[MIN_COL_DISPLAY_W..=MAX_COL_DISPLAY_W]`). Stored so render can lay
    /// out columns without re-walking every row each frame.
    pub col_widths: Vec<u16>,
    pub scroll_row: usize,
    pub scroll_col: usize,
    /// Selected cell (#177): body-row and column indices. Arrow keys
    /// move it; the viewport follows.
    pub cur_row: usize,
    pub cur_col: usize,
    /// Absolute 0-based (row, col) of the used range's first cell in the
    /// SOURCE sheet (#178): calamine's grid starts at the used range, not
    /// A1, so writing an edit back needs this offset. (0, 0) for CSV.
    pub origin: (u32, u32),
    /// Rows of the source above this batch (#1222): a SQLite page holds
    /// rows `row_base + 1 ..` of its table, and is numbered so. 0 elsewhere.
    pub row_base: usize,
}

impl SheetData {
    pub fn col_count(&self) -> usize {
        self.col_widths.len()
    }
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Assemble a SheetData from parts (the SQLite browser's tables).
pub fn sheet_data_from_parts(
    name: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
) -> SheetData {
    let col_widths = compute_col_widths((!headers.is_empty()).then_some(&headers), &rows);
    SheetData {
        name,
        headers,
        rows,
        col_widths,
        scroll_row: 0,
        scroll_col: 0,
        cur_row: 0,
        cur_col: 0,
        origin: (0, 0),
        row_base: 0,
    }
}

/// Assemble a view over pre-built sheets (the SQLite browser).
pub fn view_from_sheets(
    kind: SheetKind,
    source_byte_size: u64,
    sheets: Vec<SheetData>,
) -> SheetView {
    SheetView {
        kind,
        source_byte_size,
        sheets,
        current_sheet: 0,
        dirty: false,
        editing: None,
        cell_edits: Vec::new(),
        grid: SheetGridLayout::default(),
        sqlite_pages: Vec::new(),
    }
}

pub fn extension_is_sheet(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "csv" | "tsv" | "xlsx" | "xls" | "ods" | "xlsb"
    )
}

pub fn sheet_kind_from_ext(ext: &str) -> Option<SheetKind> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "csv" => SheetKind::Csv,
        "tsv" => SheetKind::Tsv,
        "xlsx" => SheetKind::Xlsx,
        "xls" => SheetKind::Xls,
        "ods" => SheetKind::Ods,
        "xlsb" => SheetKind::Xlsb,
        _ => return None,
    })
}

pub fn open_sheet(path: &Path) -> std::io::Result<SheetView> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let kind = sheet_kind_from_ext(ext)
        .ok_or_else(|| std::io::Error::other("unsupported sheet extension"))?;
    open_sheet_with_kind(path, kind)
}

/// Open with an explicit kind, bypassing the extension: the content
/// router (#174) lands extensionless / misnamed workbooks here.
pub fn open_sheet_with_kind(path: &Path, kind: SheetKind) -> std::io::Result<SheetView> {
    let meta = std::fs::metadata(path)?;
    match kind {
        SheetKind::Csv | SheetKind::Tsv => {
            if meta.len() > CSV_BYTES_CAP {
                return Err(std::io::Error::other(format!(
                    "CSV/TSV too large ({} bytes)",
                    meta.len()
                )));
            }
            let bytes = std::fs::read(path)?;
            let delim = if matches!(kind, SheetKind::Tsv) {
                b'\t'
            } else {
                b','
            };
            let sheet = parse_delimited(&bytes, delim, "Sheet1")
                .map_err(|e| std::io::Error::other(format!("CSV parse: {e}")))?;
            Ok(SheetView {
                kind,
                source_byte_size: meta.len(),
                sheets: vec![sheet],
                current_sheet: 0,
                dirty: false,
                editing: None,
                cell_edits: Vec::new(),
                grid: SheetGridLayout::default(),
                sqlite_pages: Vec::new(),
            })
        }
        SheetKind::Sqlite => Err(std::io::Error::other(
            "SQLite opens through the database browser, not the sheet parser",
        )),
        SheetKind::Xlsx | SheetKind::Xls | SheetKind::Ods | SheetKind::Xlsb => {
            if meta.len() > XLSX_BYTES_CAP {
                return Err(std::io::Error::other(format!(
                    "Spreadsheet too large ({} bytes)",
                    meta.len()
                )));
            }
            // calamine expands an ODS's repeats while opening the workbook,
            // before any sheet is asked for, so its extent is measured from
            // the XML first.
            if kind == SheetKind::Ods {
                for (name, rows, cols) in ods_extents(path)? {
                    check_sheet_extent(&name, rows, cols)?;
                }
            }
            let sheets = read_calamine_workbook(path, kind).map_err(|e| match e {
                WorkbookError::TooLarge(e) => e,
                WorkbookError::Calamine(e) => std::io::Error::other(format!("workbook open: {e}")),
            })?;
            if sheets.is_empty() {
                return Err(std::io::Error::other("workbook has no sheets"));
            }
            Ok(SheetView {
                kind,
                source_byte_size: meta.len(),
                sheets,
                current_sheet: 0,
                dirty: false,
                editing: None,
                cell_edits: Vec::new(),
                grid: SheetGridLayout::default(),
                sqlite_pages: Vec::new(),
            })
        }
    }
}

pub fn parse_delimited(bytes: &[u8], delim: u8, sheet_name: &str) -> Result<SheetData, csv::Error> {
    // TSV has no quoting (#1136): `psql`, `mysql -B` and `cut` write a `"`
    // as a plain character, and reading one as a quote swallowed every row
    // after a cell that started with it.
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .quoting(delim != b'\t')
        .from_reader(bytes);
    let mut rows: Vec<Vec<String>> = Vec::new();
    // The reader skips blank lines, so a single-column file lost every
    // empty value on the first save. They come back as empty rows. A
    // record's position is where the reader started looking for it, before
    // the blank lines it skipped, so those are the line breaks it opens with.
    for record in reader.records() {
        let r = record?;
        let start = r.position().map_or(0, |p| p.byte() as usize);
        let mut rest = bytes.get(start..).unwrap_or_default();
        // After a CRLF record the reader stops past the `\r`, so the `\n`
        // it starts on still belongs to that record's terminator.
        if start > 0 && bytes[start - 1] == b'\r' {
            rest = rest.strip_prefix(b"\n").unwrap_or(rest);
        }
        let mut blanks = 0usize;
        while let Some(tail) = rest
            .strip_prefix(b"\r\n")
            .or_else(|| rest.strip_prefix(b"\n"))
        {
            rest = tail;
            blanks += 1;
        }
        // Not before the first record, which becomes the header row.
        if !rows.is_empty() {
            rows.extend(std::iter::repeat_n(Vec::new(), blanks));
        }
        rows.push(r.iter().map(|s| s.to_string()).collect());
    }
    let (headers, body) = split_header(rows);
    let col_widths = compute_col_widths(headers.as_ref(), &body);
    Ok(SheetData {
        name: sheet_name.to_string(),
        headers: headers.unwrap_or_default(),
        rows: body,
        col_widths,
        scroll_row: 0,
        scroll_col: 0,
        cur_row: 0,
        cur_col: 0,
        origin: (0, 0),
        row_base: 0,
    })
}

impl SheetData {
    /// Sort the body rows by column `col` (#578, a sortable results
    /// table): ascending, or descending when they already are ascending,
    /// so sorting the same column again reverses it. Cells that read as
    /// numbers compare as numbers and come before the rest, which compare
    /// as text without case. The sort is stable, and the cursor stays on
    /// the row it was on. Returns whether the order is now ascending.
    pub fn sort_by_column(&mut self, col: usize) -> bool {
        // Numbers sort before text. Comparing a mixed pair as text instead
        // made the order cyclic (2 < 10 < "1a" < 2), and the standard sort
        // panics on a comparator that is not a total order (#1134). Each
        // cell is parsed and lowercased once, not on every comparison.
        enum Key {
            Num(f64),
            Text(String),
        }
        fn cmp(a: &Key, b: &Key) -> std::cmp::Ordering {
            use std::cmp::Ordering::{Greater, Less};
            match (a, b) {
                (Key::Num(x), Key::Num(y)) => x.total_cmp(y),
                (Key::Num(_), Key::Text(_)) => Less,
                (Key::Text(_), Key::Num(_)) => Greater,
                (Key::Text(a), Key::Text(b)) => a.cmp(b),
            }
        }
        let keys: Vec<Key> = self
            .rows
            .iter()
            .map(|r| {
                let cell = r.get(col).map_or("", String::as_str);
                match cell.trim().parse::<f64>() {
                    Ok(x) => Key::Num(x),
                    Err(_) => Key::Text(cell.to_lowercase()),
                }
            })
            .collect();
        let ascending = !keys.windows(2).all(|w| cmp(&w[0], &w[1]).is_le());
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by(|&i, &j| {
            let o = cmp(&keys[i], &keys[j]);
            if ascending { o } else { o.reverse() }
        });
        let cursor = order.iter().position(|&i| i == self.cur_row);
        let mut old: Vec<Option<Vec<String>>> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(Some)
            .collect();
        self.rows = order.iter().filter_map(|&i| old[i].take()).collect();
        if let Some(c) = cursor {
            self.cur_row = c;
        }
        ascending
    }

    /// Overwrite one body cell (#177), growing a short row (the csv
    /// reader is `flexible`, so ragged rows are real) and refreshing the
    /// column widths so the grid re-lays-out immediately.
    pub fn set_cell(&mut self, row: usize, col: usize, value: String) {
        let Some(r) = self.rows.get_mut(row) else {
            return;
        };
        if r.len() <= col {
            r.resize(col + 1, String::new());
        }
        r[col] = value;
        self.recompute_widths();
    }

    pub fn cell(&self, row: usize, col: usize) -> &str {
        self.rows
            .get(row)
            .and_then(|r| r.get(col))
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn insert_row(&mut self, at: usize) {
        let cols = self.col_count().max(1);
        let at = at.min(self.rows.len());
        self.rows.insert(at, vec![String::new(); cols]);
        self.recompute_widths();
    }

    pub fn delete_row(&mut self, at: usize) -> bool {
        if at >= self.rows.len() {
            return false;
        }
        self.rows.remove(at);
        self.recompute_widths();
        true
    }

    pub fn insert_col(&mut self, at: usize) {
        // A fully EMPTY sheet gains its header cell too (#193 review):
        // without one, the first saved body row would be re-read as the
        // header on reopen and vanish from the editable grid.
        if self.headers.is_empty() && self.rows.is_empty() {
            self.headers.push(String::new());
            self.recompute_widths();
            return;
        }
        if !self.headers.is_empty() {
            let hat = at.min(self.headers.len());
            self.headers.insert(hat, String::new());
        }
        for r in &mut self.rows {
            let rat = at.min(r.len());
            r.insert(rat, String::new());
        }
        self.recompute_widths();
    }

    pub fn delete_col(&mut self, at: usize) -> bool {
        if at >= self.col_count() {
            return false;
        }
        if at < self.headers.len() {
            self.headers.remove(at);
        }
        for r in &mut self.rows {
            if at < r.len() {
                r.remove(at);
            }
        }
        self.recompute_widths();
        true
    }

    fn recompute_widths(&mut self) {
        let headers = (!self.headers.is_empty()).then_some(&self.headers);
        self.col_widths = compute_col_widths(headers, &self.rows);
    }
}

/// Serialize the sheet back to delimited bytes: the header row first
/// (it is the file's own row 0, split off at parse), then the body, with
/// the delimiter the file was READ with. `crlf` keeps a CRLF file CRLF:
/// rewritten with LF, editing one cell made the whole file a diff.
///
/// `original` is the file as it is on disk (empty for a new file). A row
/// whose values match one of its records goes back as that record's own
/// bytes, so an edit changes its own line only, whatever quoting the file
/// was written with (#1375). A changed or new row is quoted the way the
/// file is: every field (R `write.csv`, Python `QUOTE_ALL`), every field
/// but numbers (`QUOTE_NONNUMERIC`), or only where csv needs it.
pub fn serialize_delimited(data: &SheetData, delim: u8, crlf: bool, original: &[u8]) -> Vec<u8> {
    let records = raw_records(original, delim);
    // The file's record terminator, read from its first record rather than
    // its first `\n`, which may sit inside a quoted field.
    let terminator: &[u8] = match records.first() {
        Some(r) if !r.term.is_empty() => &original[r.term.clone()],
        _ if crlf => b"\r\n",
        _ => b"\n",
    };
    // Never quoted for TSV, as it was read (#1136).
    let quote_style = if delim == b'\t' {
        csv::QuoteStyle::Never
    } else {
        file_quote_style(&records, original, delim)
    };
    // Record indexes by values, in file order. A row takes the record at
    // its own position when that one has its values, so of two equal rows
    // written differently each keeps its own bytes; a moved row (a sort)
    // takes the first one left.
    let mut unchanged: std::collections::HashMap<&[String], Vec<usize>> =
        std::collections::HashMap::new();
    for (i, r) in records.iter().enumerate() {
        unchanged.entry(r.fields.as_slice()).or_default().push(i);
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut write = |record: &[String]| {
        // A row with no fields is a blank line the file had (see
        // `parse_delimited`); the writer would emit `""` for it instead.
        if record.is_empty() {
            out.extend_from_slice(terminator);
            return;
        }
        let pos = at;
        at += 1;
        let reused = unchanged.get_mut(record).and_then(|q| {
            let k = q.iter().position(|&i| i == pos).unwrap_or(0);
            (!q.is_empty()).then(|| q.remove(k))
        });
        if let Some(i) = reused {
            let r = &records[i];
            out.extend_from_slice(&original[r.span.clone()]);
            // Its own line ending; none only for the file's last line.
            out.extend_from_slice(if r.term.is_empty() {
                terminator
            } else {
                &original[r.term.clone()]
            });
            return;
        }
        let mut w = csv::WriterBuilder::new()
            .delimiter(delim)
            .flexible(true)
            .terminator(if terminator == b"\r\n" {
                csv::Terminator::CRLF
            } else {
                csv::Terminator::Any(terminator[0])
            })
            .quote_style(quote_style)
            .from_writer(&mut out);
        let _ = w.write_record(record);
        let _ = w.flush();
    };
    if !data.headers.is_empty() {
        write(&data.headers);
    }
    for r in &data.rows {
        write(r);
    }
    // A file that ended without a line ending still does, unless its last
    // row is a blank line, which is nothing but its line ending.
    let unterminated = original.last().is_some_and(|b| !matches!(b, b'\r' | b'\n'));
    if unterminated && data.rows.last().is_none_or(|r| !r.is_empty()) {
        let cut = if out.ends_with(b"\r\n") {
            2
        } else {
            usize::from(matches!(out.last(), Some(b'\r' | b'\n')))
        };
        out.truncate(out.len() - cut);
    }
    out
}

/// One record of a delimited file as it is on disk (#1375).
struct RawRecord {
    /// Its values, as the grid shows them.
    fields: Vec<String>,
    /// Its bytes, line ending and blank lines around it left out.
    span: std::ops::Range<usize>,
    /// Its line ending: `\r\n`, `\n`, `\r`, or empty at the end of a file
    /// with none.
    term: std::ops::Range<usize>,
}

/// Every record of a delimited file with the bytes it was read from (#1375).
/// Read as [`parse_delimited`] reads, so a record's values are the ones the
/// grid shows. A file that does not parse yields none.
fn raw_records(bytes: &[u8], delim: u8) -> Vec<RawRecord> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .quoting(delim != b'\t')
        .from_reader(bytes);
    let mut out: Vec<RawRecord> = Vec::new();
    for record in reader.records() {
        let Ok(r) = record else {
            return Vec::new();
        };
        // Where the reader began looking for this record: the end of the
        // previous one, line ending included.
        let at = r.position().map_or(0, |p| p.byte() as usize);
        if let Some(prev) = out.last_mut() {
            prev.span.end = at;
        }
        // Past the blank lines (and a CRLF's `\n`) the reader skipped.
        let mut start = at;
        while matches!(bytes.get(start), Some(b'\r' | b'\n')) {
            start += 1;
        }
        out.push(RawRecord {
            fields: r.iter().map(str::to_string).collect(),
            span: start..bytes.len(),
            term: 0..0,
        });
    }
    for r in &mut out {
        let span = &mut r.span;
        while span.end > span.start && matches!(bytes[span.end - 1], b'\r' | b'\n') {
            span.end -= 1;
        }
        let rest = &bytes[span.end..];
        let len = if rest.starts_with(b"\r\n") {
            2
        } else {
            usize::from(matches!(rest.first(), Some(b'\r' | b'\n')))
        };
        r.term = span.end..span.end + len;
    }
    out
}

/// The quoting a delimited file was written with, read from its records:
/// every field quoted, or exactly the fields that are not numbers, or
/// neither (quote where needed). A number is what the csv writer's
/// `NonNumeric` style takes as one, so that style writes such a file back.
fn file_quote_style(records: &[RawRecord], bytes: &[u8], delim: u8) -> csv::QuoteStyle {
    let is_number = |s: &str| s.parse::<f64>().is_ok() || s.parse::<i128>().is_ok();
    let (mut quoted, mut bare_numbers, mut bare_other) = (0usize, 0usize, 0usize);
    for RawRecord { fields, span, .. } in records {
        let raw = &bytes[span.clone()];
        // The reader drops a UTF-8 BOM from the first value; a quote after
        // it still opens that field.
        let raw = if span.start == 0 {
            raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw)
        } else {
            raw
        };
        let flags = quoted_fields(raw, delim);
        if flags.len() != fields.len() {
            return csv::QuoteStyle::Necessary;
        }
        for (field, was_quoted) in fields.iter().zip(flags) {
            match (was_quoted, is_number(field)) {
                (true, _) => quoted += 1,
                (false, true) => bare_numbers += 1,
                (false, false) => bare_other += 1,
            }
        }
    }
    match (quoted, bare_numbers, bare_other) {
        (1.., 0, 0) => csv::QuoteStyle::Always,
        (1.., 1.., 0) => csv::QuoteStyle::NonNumeric,
        _ => csv::QuoteStyle::Necessary,
    }
}

/// For each field of one raw record, whether it was written in quotes.
fn quoted_fields(raw: &[u8], delim: u8) -> Vec<bool> {
    let mut flags = Vec::new();
    let mut i = 0;
    loop {
        let quoted = raw.get(i) == Some(&b'"');
        flags.push(quoted);
        if quoted {
            // Past the closing quote; `""` inside is an escaped quote.
            i += 1;
            while i < raw.len() {
                if raw[i] == b'"' {
                    if raw.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
        }
        match raw[i.min(raw.len())..].iter().position(|&b| b == delim) {
            Some(off) => i += off + 1,
            None => return flags,
        }
    }
}

/// Outcome of an xlsx edit save (#178).
pub struct XlsxSaveReport {
    pub written: usize,
    /// A1-style coordinates of formula cells left untouched because the
    /// caller did not consent to overwriting formulas.
    pub formula_skipped: Vec<String>,
}

/// Write grid cell edits back into the workbook through umya (#178):
/// the file is re-read (styles, widths, and untouched cells preserved),
/// each edited cell's grid value is applied (numbers as numbers, all
/// else as strings), and the workbook is written in place. A cell that
/// holds a FORMULA is skipped unless `overwrite_formulas`, since the
/// grid shows calamine's cached value and writing it back would destroy
/// the formula silently.
pub fn save_xlsx_edits(
    path: &Path,
    sheets: &[SheetData],
    edits: &[(usize, usize, usize)],
    overwrite_formulas: bool,
) -> Result<XlsxSaveReport, String> {
    let mut book = umya_spreadsheet::reader::xlsx::read(path).map_err(|e| e.to_string())?;
    let mut written = 0usize;
    let mut formula_skipped: Vec<String> = Vec::new();
    for &(si, r, c) in edits {
        let Some(data) = sheets.get(si) else {
            continue;
        };
        let value = data.cell(r, c).to_string();
        let Ok(ws) = book.sheet_by_name_mut(&data.name) else {
            continue;
        };
        // Grid body row r sits below the header (one range row) inside
        // the used range at `origin`; umya coordinates are 1-based.
        let col = data.origin.1 + c as u32 + 1;
        let row = data.origin.0 + r as u32 + 2;
        let cell = ws.cell_mut((col, row));
        if cell.is_formula() && !overwrite_formulas {
            // Sheet-QUALIFIED (#194 review): a bare A1 key would pin an
            // ordinary edit at the same coordinates on another sheet.
            formula_skipped.push(format!("{}!{}{row}", data.name, column_letters(col)));
            continue;
        }
        // Typed writes (#194 review): calamine formats booleans as
        // lowercase true/false, which set_value would store as STRINGS.
        if value.eq_ignore_ascii_case("true") {
            cell.set_value_bool(true);
        } else if value.eq_ignore_ascii_case("false") {
            cell.set_value_bool(false);
        } else if let Some(n) = xlsx_number(&value) {
            cell.set_value_number(n);
        } else {
            cell.set_value_string(value);
        }
        written += 1;
    }
    umya_spreadsheet::writer::xlsx::write(&book, path).map_err(|e| e.to_string())?;
    Ok(XlsxSaveReport {
        written,
        formula_skipped,
    })
}

/// The number an edited cell's text is stored as, or None to store it as
/// text. `f64::from_str` alone is too eager: it takes `nan` and `inf`
/// (and `1e400` becomes infinity), which Excel reports as a corrupt file,
/// and a zip code like `02134` would lose its leading zero.
fn xlsx_number(value: &str) -> Option<f64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    let plain = !digits.is_empty()
        && digits.starts_with(|c: char| c.is_ascii_digit())
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+'));
    let leading_zero = digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.");
    if !plain || leading_zero {
        return None;
    }
    value.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// 1-based column index to A1 letters (1 -> A, 27 -> AA).
pub fn column_letters_pub(col: u32) -> String {
    column_letters(col)
}

fn column_letters(mut col: u32) -> String {
    let mut s = String::new();
    while col > 0 {
        let rem = ((col - 1) % 26) as u8;
        s.insert(0, (b'A' + rem) as char);
        col = (col - 1) / 26;
    }
    s
}

fn split_header(mut rows: Vec<Vec<String>>) -> (Option<Vec<String>>, Vec<Vec<String>>) {
    if rows.is_empty() {
        return (None, Vec::new());
    }
    let header = rows.remove(0);
    (Some(header), rows)
}

fn compute_col_widths(headers: Option<&Vec<String>>, rows: &[Vec<String>]) -> Vec<u16> {
    let header_cols = headers.map(|h| h.len()).unwrap_or(0);
    let max_cols = rows
        .iter()
        .map(|r| r.len())
        .max()
        .unwrap_or(0)
        .max(header_cols);
    let mut widths = vec![MIN_COL_DISPLAY_W; max_cols];
    if let Some(h) = headers {
        for (i, cell) in h.iter().enumerate() {
            widths[i] = widths[i].max(display_width(cell));
        }
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                continue;
            }
            widths[i] = widths[i].max(display_width(cell));
        }
    }
    for w in widths.iter_mut() {
        *w = (*w).clamp(MIN_COL_DISPLAY_W, MAX_COL_DISPLAY_W);
    }
    widths
}

fn display_width(s: &str) -> u16 {
    // unicode-width is a separate dep; chars().count() is good enough for
    // ASCII and single-codepoint scripts that croft usually deals with.
    s.chars().count().min(u16::MAX as usize) as u16
}

/// Refuse a sheet whose dense extent is over [`MAX_SHEET_CELLS`]. The
/// message says "too large" so the open is not rerouted to the text editor.
fn check_sheet_extent(name: &str, rows: u64, cols: u64) -> std::io::Result<()> {
    if rows.saturating_mul(cols) > MAX_SHEET_CELLS {
        return Err(std::io::Error::other(format!(
            "Sheet too large: {name} is {rows}x{cols} cells (max {}M)",
            MAX_SHEET_CELLS / 1_000_000
        )));
    }
    Ok(())
}

/// Each table's `(name, rows, columns)` as calamine will expand it: the
/// rectangle from the first to the last row and column holding a value,
/// with `number-rows-repeated` / `number-columns-repeated` counted inside
/// it. Repeated EMPTY rows and cells past the last value (LibreOffice pads
/// every sheet with them) expand to nothing, as in calamine. A cell holds a
/// value when calamine reads one from it: any `office:*value` attribute, or
/// a string value type (its text, even none). A value type alone, a comment
/// or a formula is empty. Streams content.xml, keeping only running bounds.
fn ods_extents(path: &Path) -> std::io::Result<Vec<(String, u64, u64)>> {
    use quick_xml::events::{BytesStart, Event};
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    let content = zip
        .by_name("content.xml")
        .map_err(|e| std::io::Error::other(format!("content.xml: {e}")))?;
    let mut reader = quick_xml::Reader::from_reader(std::io::BufReader::new(content));
    let xml_err = |e: quick_xml::Error| std::io::Error::other(format!("content.xml: {e}"));
    // calamine adds nothing for a repeat of 0, so neither does the count.
    let repeat = |e: &BytesStart, key: &[u8]| -> u64 {
        e.try_get_attribute(key)
            .ok()
            .flatten()
            .and_then(|a| std::str::from_utf8(&a.value).ok()?.trim().parse().ok())
            .unwrap_or(1)
    };
    let valued = |e: &BytesStart| {
        e.attributes().flatten().any(|a| match a.key.as_ref() {
            b"office:value"
            | b"office:string-value"
            | b"office:date-value"
            | b"office:time-value"
            | b"office:boolean-value" => true,
            b"office:value-type" => &*a.value == b"string",
            _ => false,
        })
    };
    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut skip = Vec::new();
    let mut table: Option<String> = None;
    // Rows seen so far (repeats counted), the first valued row's start, the
    // last valued row's end, and the valued columns' span over all rows.
    let (mut row_at, mut first, mut last, mut cols) = (0u64, None::<u64>, 0u64, None::<(u64, u64)>);
    // The open row: its repeat count, next column, and valued columns.
    let (mut row_repeat, mut col, mut span) = (1u64, 0u64, None::<(u64, u64)>);
    loop {
        let ev = reader.read_event_into(&mut buf).map_err(xml_err)?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e)
                if table.is_none() && e.name().as_ref() == b"table:table" =>
            {
                let name = e
                    .try_get_attribute(b"table:name")
                    .ok()
                    .flatten()
                    .map(|a| String::from_utf8_lossy(&a.value).into_owned())
                    .unwrap_or_default();
                table = Some(name);
                (row_at, first, last, cols) = (0, None, 0, None);
            }
            Event::Start(e) if e.name().as_ref() == b"table:table-row" => {
                (row_repeat, col, span) = (repeat(&e, b"table:number-rows-repeated"), 0, None);
            }
            Event::Empty(e) if e.name().as_ref() == b"table:table-row" => {
                row_at = row_at.saturating_add(repeat(&e, b"table:number-rows-repeated"));
            }
            Event::End(e) if e.name().as_ref() == b"table:table-row" => {
                if let Some((a, b)) = span
                    && row_repeat > 0
                {
                    first.get_or_insert(row_at);
                    last = row_at.saturating_add(row_repeat);
                    cols = Some(cols.map_or((a, b), |(lo, hi)| (lo.min(a), hi.max(b))));
                }
                row_at = row_at.saturating_add(row_repeat);
            }
            Event::Start(e) | Event::Empty(e)
                if matches!(
                    e.name().as_ref(),
                    b"table:table-cell" | b"table:covered-table-cell"
                ) =>
            {
                let n = repeat(&e, b"table:number-columns-repeated");
                if n > 0 && valued(&e) {
                    let end = col.saturating_add(n - 1);
                    span = Some(span.map_or((col, end), |(a, _)| (a, end)));
                }
                col = col.saturating_add(n);
                // The cell's text, comments and anything nested in it are
                // not cells of this table.
                if !empty {
                    let name = e.name().as_ref().to_vec();
                    reader
                        .read_to_end_into(quick_xml::name::QName(&name), &mut skip)
                        .map_err(xml_err)?;
                    skip.clear();
                }
            }
            Event::End(e) if e.name().as_ref() == b"table:table" => {
                let name = table.take().unwrap_or_default();
                match (first, cols) {
                    (Some(first), Some((lo, hi))) => out.push((name, last - first, hi - lo + 1)),
                    _ => out.push((name, 0, 0)),
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Why a workbook did not load: refused for its extent, or calamine's own
/// error.
enum WorkbookError {
    TooLarge(std::io::Error),
    Calamine(calamine::Error),
}

impl From<calamine::Error> for WorkbookError {
    fn from(e: calamine::Error) -> Self {
        WorkbookError::Calamine(e)
    }
}

/// The dense extent `(rows, columns)` of a sheet's values, from its cells
/// streamed one at a time, so measuring it allocates nothing per cell.
/// calamine builds the range from the first to the last non-empty cell.
fn streamed_extent<E>(
    mut next: impl FnMut() -> Result<Option<(u32, u32, bool)>, E>,
) -> Result<(u64, u64), E> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    while let Some((r, c, empty)) = next()? {
        if empty {
            continue;
        }
        bounds = Some(match bounds {
            None => (r, r, c, c),
            Some((r0, r1, c0, c1)) => (r0.min(r), r1.max(r), c0.min(c), c1.max(c)),
        });
    }
    Ok(bounds.map_or((0, 0), |(r0, r1, c0, c1)| {
        ((r1 - r0) as u64 + 1, (c1 - c0) as u64 + 1)
    }))
}

/// Open a workbook with calamine. `open_workbook_auto` resolves the reader
/// from the file EXTENSION, so a content-routed file without one (#174)
/// needs the explicit reader for its sniffed kind.
fn open_calamine(
    path: &Path,
    kind: SheetKind,
) -> Result<calamine::Sheets<std::io::BufReader<std::fs::File>>, calamine::Error> {
    Ok(match calamine::open_workbook_auto(path) {
        Ok(w) => w,
        Err(auto_err) => match kind {
            SheetKind::Xlsx => calamine::Sheets::Xlsx(calamine::open_workbook(path)?),
            SheetKind::Xls => calamine::Sheets::Xls(calamine::open_workbook(path)?),
            SheetKind::Ods => calamine::Sheets::Ods(calamine::open_workbook(path)?),
            SheetKind::Xlsb => calamine::Sheets::Xlsb(calamine::open_workbook(path)?),
            SheetKind::Csv | SheetKind::Tsv | SheetKind::Sqlite => return Err(auto_err),
        },
    })
}

fn read_calamine_workbook(path: &Path, kind: SheetKind) -> Result<Vec<SheetData>, WorkbookError> {
    use calamine::{Data, Reader};
    let mut workbook = open_calamine(path, kind)?;
    let sheet_names = workbook.sheet_names();
    let mut out: Vec<SheetData> = Vec::with_capacity(sheet_names.len());
    for name in sheet_names {
        // xlsx and xlsb build a dense range from sparse cells, so two values
        // at opposite corners ask for the whole grid between them.
        // A sheet whose cells cannot be streamed (a chart sheet, a broken
        // part) is skipped rather than built unmeasured, as one whose range
        // cannot be read is below.
        let extent = match &mut workbook {
            calamine::Sheets::Xlsx(x) => {
                let Ok(mut r) = x.worksheet_cells_reader(&name) else {
                    continue;
                };
                let Ok(extent) = streamed_extent(|| {
                    r.next_cell().map(|c| {
                        c.map(|c| {
                            let (row, col) = c.get_position();
                            (row, col, matches!(c.get_value(), calamine::DataRef::Empty))
                        })
                    })
                }) else {
                    continue;
                };
                Some(extent)
            }
            calamine::Sheets::Xlsb(x) => {
                let Ok(mut r) = x.worksheet_cells_reader(&name) else {
                    continue;
                };
                let Ok(extent) = streamed_extent(|| {
                    r.next_cell().map(|c| {
                        c.map(|c| {
                            let (row, col) = c.get_position();
                            (row, col, matches!(c.get_value(), calamine::DataRef::Empty))
                        })
                    })
                }) else {
                    continue;
                };
                Some(extent)
            }
            _ => None,
        };
        if let Some((rows, cols)) = extent {
            check_sheet_extent(&name, rows, cols).map_err(WorkbookError::TooLarge)?;
        }
        let range = match workbook.worksheet_range(&name) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let origin = range.start().unwrap_or((0, 0));
        let mut rows: Vec<Vec<String>> = Vec::with_capacity(range.height());
        for row in range.rows() {
            rows.push(row.iter().map(format_cell).collect());
        }
        let (headers, body) = split_header(rows);
        let col_widths = compute_col_widths(headers.as_ref(), &body);
        out.push(SheetData {
            name,
            headers: headers.unwrap_or_default(),
            rows: body,
            col_widths,
            scroll_row: 0,
            scroll_col: 0,
            cur_row: 0,
            cur_col: 0,
            origin,
            row_base: 0,
        });
    }
    let _ = Data::Empty; // keeps the import explicit even if unused above
    Ok(out)
}

fn format_cell(d: &calamine::Data) -> String {
    match d {
        calamine::Data::Empty => std::string::String::new(),
        calamine::Data::String(s) => s.clone(),
        calamine::Data::Float(f) => format_float(*f),
        calamine::Data::Int(i) => i.to_string(),
        calamine::Data::Bool(b) => b.to_string(),
        calamine::Data::DateTime(dt) => dt.to_string(),
        calamine::Data::DateTimeIso(s) | calamine::Data::DurationIso(s) => s.clone(),
        calamine::Data::Error(e) => format!("#{e:?}"),
    }
}

fn format_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sorting_a_column_orders_numbers_as_numbers_and_toggles() {
        let mut d = super::parse_delimited(b"name,count\nb,10\nA,9\nc,100\n", b',', "s").unwrap();
        d.cur_row = 2; // on "c"
        assert!(d.sort_by_column(1), "ascending first");
        let col = |d: &super::SheetData, c: usize| -> Vec<String> {
            d.rows.iter().map(|r| r[c].clone()).collect()
        };
        assert_eq!(col(&d, 1), ["9", "10", "100"], "as numbers, not text");
        assert_eq!(d.rows[d.cur_row][0], "c", "the cursor keeps its row");
        assert!(!d.sort_by_column(1), "again: descending");
        assert_eq!(col(&d, 1), ["100", "10", "9"]);
        assert!(d.sort_by_column(0));
        assert_eq!(col(&d, 0), ["A", "b", "c"], "text without case");
        assert_eq!(d.headers, ["name", "count"], "the header stays put");
    }

    /// Write a zip of `(name, text)` members.
    fn zip_of(path: &std::path::Path, members: &[(&str, &str)]) {
        use std::io::Write as _;
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in members {
            z.start_file(*name, o).unwrap();
            z.write_all(text.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    /// A one-table .ods whose `table:table` body is `rows`.
    fn ods_with(path: &std::path::Path, rows: &str) {
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.2"><office:body><office:spreadsheet><table:table table:name="Sheet1">{rows}</table:table></office:spreadsheet></office:body></office:document-content>"#
        );
        zip_of(
            path,
            &[
                ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
                (
                    "META-INF/manifest.xml",
                    r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#,
                ),
                ("content.xml", &content),
            ],
        );
    }

    /// A one-sheet .xlsx whose `sheetData` is `data`.
    fn xlsx_with(path: &std::path::Path, data: &str) {
        let sheet = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{data}</sheetData></worksheet>"#
        );
        zip_of(
            path,
            &[
                (
                    "[Content_Types].xml",
                    r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
                ),
                (
                    "_rels/.rels",
                    r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
                ),
                (
                    "xl/workbook.xml",
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
                ),
                (
                    "xl/_rels/workbook.xml.rels",
                    r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
                ),
                ("xl/worksheets/sheet1.xml", &sheet),
            ],
        );
    }

    #[test]
    fn an_ods_whose_repeats_expand_past_the_cell_budget_is_refused() {
        // #1156: a few hundred bytes of repeat attributes expand to more
        // cells than a grid holds; calamine expands them when the workbook
        // opens, so the size has to be known before that.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("budget.ods");
        ods_with(
            &p,
            r#"<table:table-row table:number-rows-repeated="700"><table:table-cell office:value-type="string" table:number-columns-repeated="16384"><text:p>x</text:p></table:table-cell></table:table-row>"#,
        );
        assert!(std::fs::metadata(&p).unwrap().len() < 2048);
        let Err(err) = super::open_sheet(&p) else {
            panic!("must be refused");
        };
        let err = err.to_string();
        assert!(err.contains("too large"), "{err}");
        assert!(err.contains("700") && err.contains("16384"), "{err}");
    }

    #[test]
    fn an_ods_value_without_a_value_type_counts_toward_the_budget() {
        // #1187 review: calamine reads `office:value` as a number whatever
        // the value type says, so a repeated bare value still expands.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("bare.ods");
        ods_with(
            &p,
            r#"<table:table-row table:number-rows-repeated="700"><table:table-cell office:value="1" table:number-columns-repeated="16384"/></table:table-row>"#,
        );
        assert_eq!(
            super::ods_extents(&p).unwrap(),
            [(String::from("Sheet1"), 700, 16384)]
        );
    }

    #[test]
    fn empty_typed_and_annotation_only_ods_cells_are_not_counted() {
        // #1187 review: a float-typed cell with no value and a cell holding
        // only a comment are empty to calamine, so a 1x1 sheet padded with
        // them is not refused as 701x16384.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("padded.ods");
        ods_with(
            &p,
            concat!(
                r#"<table:table-row><table:table-cell office:value-type="string"><text:p>a</text:p></table:table-cell><table:table-cell table:number-columns-repeated="4"/><table:table-cell><office:annotation><text:p>note</text:p></office:annotation></table:table-cell></table:table-row>"#,
                r#"<table:table-row table:number-rows-repeated="700"><table:table-cell office:value-type="float" table:number-columns-repeated="16384"/></table:table-row>"#,
            ),
        );
        assert_eq!(
            super::ods_extents(&p).unwrap(),
            [(String::from("Sheet1"), 1, 1)]
        );
        let view = super::open_sheet(&p).unwrap();
        assert_eq!(view.sheets[0].headers, ["a"]);
    }

    #[test]
    fn a_zero_ods_repeat_covers_nothing() {
        // #1187 review: `number-columns-repeated="0"` made `col + n - 1`
        // underflow. calamine adds no cell for it.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("zero.ods");
        ods_with(
            &p,
            concat!(
                r#"<table:table-row><table:table-cell office:value-type="string" table:number-columns-repeated="0"><text:p>gone</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>kept</text:p></table:table-cell></table:table-row>"#,
                r#"<table:table-row table:number-rows-repeated="0"><table:table-cell office:value-type="string"><text:p>none</text:p></table:table-cell></table:table-row>"#,
            ),
        );
        assert_eq!(
            super::ods_extents(&p).unwrap(),
            [(String::from("Sheet1"), 1, 1)]
        );
    }

    #[test]
    fn an_empty_string_typed_ods_cell_still_counts() {
        // Negative: calamine reads a string-typed cell with no text as an
        // empty string, which is a value, so it widens the sheet.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blank.ods");
        ods_with(
            &p,
            r#"<table:table-row><table:table-cell office:value-type="string"><text:p>a</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"/><table:table-cell office:value-type="string"/></table:table-row>"#,
        );
        assert_eq!(
            super::ods_extents(&p).unwrap(),
            [(String::from("Sheet1"), 1, 4)]
        );
    }

    #[test]
    fn an_xlsx_sheet_that_cannot_be_streamed_fails_to_open() {
        // #1187 review: a cells-reader error skipped the extent check and
        // went on to build the range; such a sheet is now skipped unbuilt.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("broken.xlsx");
        xlsx_with(
            &p,
            r#"<row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row><row r="2"><c r="!!" t="n"><v>1</v></c></row>"#,
        );
        assert!(super::open_sheet(&p).is_err());
    }

    #[test]
    fn an_xlsx_with_two_cells_at_opposite_corners_is_refused() {
        // #1156 (comment): A1 and XFD1048576 make calamine allocate the
        // dense 17-billion-cell rectangle between them (512 GB).
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("corner.xlsx");
        xlsx_with(
            &p,
            r#"<row r="1"><c r="A1" t="inlineStr"><is><t>corner one</t></is></c></row><row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>corner two</t></is></c></row>"#,
        );
        let Err(err) = super::open_sheet(&p) else {
            panic!("must be refused");
        };
        let err = err.to_string();
        assert!(err.contains("too large"), "{err}");
        assert!(err.contains("1048576") && err.contains("16384"), "{err}");
    }

    #[test]
    fn libreoffice_style_trailing_empty_repeats_still_open() {
        // Negative: LibreOffice pads a sheet with a huge repeated EMPTY
        // row and empty repeated cells; those expand to nothing.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("real.ods");
        ods_with(
            &p,
            concat!(
                r#"<table:table-row><table:table-cell office:value-type="string"><text:p>name</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>qty</text:p></table:table-cell><table:table-cell table:number-columns-repeated="1022"/></table:table-row>"#,
                r#"<table:table-row table:number-rows-repeated="3"><table:table-cell office:value-type="string"><text:p>bolt</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="4"><text:p>4</text:p></table:table-cell><table:table-cell table:number-columns-repeated="1022"/></table:table-row>"#,
                r#"<table:table-row table:number-rows-repeated="1048572"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>"#,
            ),
        );
        let view = super::open_sheet(&p).unwrap();
        assert_eq!(view.sheets[0].headers, ["name", "qty"]);
        assert_eq!(view.sheets[0].rows.len(), 3);
        assert_eq!(view.sheets[0].cell(2, 1), "4");
    }

    #[test]
    fn an_xlsx_within_the_budget_still_opens() {
        // Negative: cells far apart but a modest rectangle between them.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("ok.xlsx");
        xlsx_with(
            &p,
            r#"<row r="1"><c r="A1" t="inlineStr"><is><t>h</t></is></c></row><row r="5000"><c r="T5000" t="inlineStr"><is><t>far</t></is></c></row>"#,
        );
        let view = super::open_sheet(&p).unwrap();
        assert_eq!(view.sheets[0].rows.len(), 4999);
        assert_eq!(view.sheets[0].cell(4998, 19), "far");
    }

    /// The 30 house numbers from #1134: plain numbers mixed with `65a`-style
    /// values, which made the old comparator cyclic and the sort panic.
    const HOUSES: [&str; 30] = [
        "109", "114", "34", "63", "101", "62", "115", "65a", "97b", "117", "91", "116a", "94b",
        "61", "46", "79", "27", "62", "67", "104", "118b", "91", "86", "79", "112", "94", "112a",
        "31", "19", "58b",
    ];

    fn sheet_of(values: &[&str]) -> super::SheetData {
        let mut csv = String::from("name,house\n");
        for (i, v) in values.iter().enumerate() {
            csv.push_str(&format!("p{i},{v}\n"));
        }
        super::parse_delimited(csv.as_bytes(), b',', "s").unwrap()
    }

    fn column(d: &super::SheetData, c: usize) -> Vec<String> {
        d.rows.iter().map(|r| r[c].clone()).collect()
    }

    #[test]
    fn sorting_numbers_mixed_with_65a_values_neither_panics_nor_cycles() {
        let mut d = sheet_of(&HOUSES);
        assert!(d.sort_by_column(1));
        let got = column(&d, 1);
        let nums: Vec<f64> = got.iter().map_while(|v| v.parse().ok()).collect();
        assert!(
            nums.windows(2).all(|w| w[0] <= w[1]),
            "numbers ascend: {got:?}"
        );
        let rest = &got[nums.len()..];
        assert!(
            rest.iter().all(|v| v.parse::<f64>().is_err()),
            "numbers first: {got:?}"
        );
        assert!(
            rest.windows(2)
                .all(|w| w[0].to_lowercase() <= w[1].to_lowercase())
        );
        assert_eq!(got.len(), HOUSES.len(), "no row lost");
        // And back the other way: the exact reverse.
        assert!(!d.sort_by_column(1));
        let mut rev = column(&d, 1);
        rev.reverse();
        assert_eq!(rev, got);
    }

    #[test]
    fn a_column_with_no_mixed_kinds_sorts_as_before() {
        // Negative: all-text and all-number columns keep their old order.
        let mut d = sheet_of(&["b", "A", "c"]);
        d.sort_by_column(1);
        assert_eq!(column(&d, 1), ["A", "b", "c"]);
        let mut d = sheet_of(&["10", "-2.5", "9", "1e3"]);
        d.sort_by_column(1);
        assert_eq!(column(&d, 1), ["-2.5", "9", "10", "1e3"]);
    }

    #[test]
    fn xlsx_edits_write_back_preserving_untouched_formulas() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("book.xlsx");
        // Build the fixture through umya itself: headers on row 1, data
        // below, one formula cell.
        let mut book = umya_spreadsheet::new_file();
        let ws = book.sheet_mut(0).unwrap();
        ws.cell_mut((1, 1)).set_value("name");
        ws.cell_mut((2, 1)).set_value("qty");
        ws.cell_mut((1, 2)).set_value("apples");
        ws.cell_mut((2, 2)).set_value_number(3);
        ws.cell_mut((1, 3)).set_value("pears");
        ws.cell_mut((2, 3)).set_formula("SUM(B2)");
        umya_spreadsheet::writer::xlsx::write(&book, &p).unwrap();

        let mut view = super::open_sheet_with_kind(&p, super::SheetKind::Xlsx).unwrap();
        let data = &mut view.sheets[0];
        assert_eq!(data.cell(0, 0), "apples");
        // Edit a plain cell and TRY to edit the formula cell.
        data.set_cell(0, 1, String::from("99"));
        data.set_cell(1, 1, String::from("7"));
        let edits = vec![(0usize, 0usize, 1usize), (0, 1, 1)];
        let report = super::save_xlsx_edits(&p, &view.sheets, &edits, false).unwrap();
        assert_eq!(report.written, 1, "the formula cell is skipped");
        assert_eq!(
            report.formula_skipped,
            vec![String::from("Sheet1!B3")],
            "skip keys are sheet-qualified (#194 review)"
        );

        // calamine re-read sees the new number; umya re-read still holds
        // the formula.
        let again = super::open_sheet_with_kind(&p, super::SheetKind::Xlsx).unwrap();
        assert_eq!(again.sheets[0].cell(0, 1), "99");
        let book2 = umya_spreadsheet::reader::xlsx::read(&p).unwrap();
        let ws2 = book2.sheet_by_name("Sheet1").unwrap();
        assert!(ws2.cell((2u32, 3u32)).unwrap().is_formula());

        // Explicit consent overwrites the formula with the literal.
        let report = super::save_xlsx_edits(&p, &view.sheets, &edits, true).unwrap();
        assert_eq!(report.written, 2);
        // Booleans round-trip TYPED, not as strings (#194 review).
        let data = &mut view.sheets[0];
        data.set_cell(0, 1, String::from("true"));
        data.set_cell(1, 1, String::from("FALSE"));
        let bool_edits = vec![(0usize, 0usize, 1usize), (0, 1, 1)];
        super::save_xlsx_edits(&p, &view.sheets, &bool_edits, true).unwrap();
        let again = super::open_sheet_with_kind(&p, super::SheetKind::Xlsx).unwrap();
        assert_eq!(
            again.sheets[0].cell(0, 1),
            "true",
            "typed bool re-reads as bool"
        );
        assert_eq!(again.sheets[0].cell(1, 1), "false");
        let book3 = umya_spreadsheet::reader::xlsx::read(&p).unwrap();
        let ws3 = book3.sheet_by_name("Sheet1").unwrap();
        assert!(!ws3.cell((2u32, 3u32)).unwrap().is_formula());
    }

    #[test]
    fn column_letters_cover_the_aa_rollover() {
        assert_eq!(super::column_letters(1), "A");
        assert_eq!(super::column_letters(26), "Z");
        assert_eq!(super::column_letters(27), "AA");
        assert_eq!(super::column_letters(52), "AZ");
    }

    #[test]
    fn cell_edits_grow_ragged_rows_and_serialize_with_quoting() {
        let mut d = super::parse_delimited(b"a,b,c\n1,2\n", b',', "S").unwrap();
        assert_eq!(d.headers, vec!["a", "b", "c"]);
        d.set_cell(0, 2, String::from("x,y"));
        assert_eq!(d.cell(0, 2), "x,y", "short row grew to hold the cell");
        let out = super::serialize_delimited(&d, b',', false, b"");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a,b,c\n1,2,\"x,y\"\n",
            "delimiter-bearing cells are quoted, header row survives"
        );
    }

    /// The #1136 repro: TSV as `psql`, `mysql -B` and `cut` write it, with
    /// `"` as a plain character.
    const PRODUCTS_TSV: &[u8] = b"sku\tname\tsize\tqty\nA1\tDeluxe widget\t5\"\t10\nA2\t\"Pro\" ruler\t12in\t4\nA3\t\"12 inch ruler\t12in\t7\nA4\tnut\tM6\t100\nA5\tbolt\tM6\t50\n";

    /// #1136: a TSV cell starting with `"` doesn't swallow the rows after it.
    #[test]
    fn a_tsv_quote_is_a_plain_character() {
        let d = super::parse_delimited(PRODUCTS_TSV, b'\t', "S").unwrap();
        let skus: Vec<&str> = d.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(skus, vec!["A1", "A2", "A3", "A4", "A5"]);
        assert_eq!(d.cell(1, 1), "\"Pro\" ruler");
        assert_eq!(d.cell(2, 1), "\"12 inch ruler");
    }

    /// #1136: a one-cell edit to a TSV leaves every other line as it was.
    #[test]
    fn a_tsv_cell_edit_rewrites_only_that_cell() {
        let mut d = super::parse_delimited(PRODUCTS_TSV, b'\t', "S").unwrap();
        d.set_cell(0, 3, String::from("12"));
        let out = super::serialize_delimited(&d, b'\t', false, b"");
        let want = String::from_utf8(PRODUCTS_TSV.to_vec())
            .unwrap()
            .replacen("5\"\t10", "5\"\t12", 1);
        assert_eq!(String::from_utf8(out).unwrap(), want);
    }

    /// CSV keeps its quoting: a quoted field holds a comma or a line break,
    /// and a cell that needs quotes gets them on save.
    #[test]
    fn csv_quoting_is_unchanged() {
        let d = super::parse_delimited(b"a,b\n\"x,y\",\"two\nlines\"\n", b',', "S").unwrap();
        assert_eq!(
            d.rows,
            vec![vec!["x,y".to_string(), "two\nlines".to_string()]]
        );
        let out = super::serialize_delimited(&d, b',', false, b"");
        assert_eq!(out, b"a,b\n\"x,y\",\"two\nlines\"\n".to_vec());
    }

    /// A TSV with no quotes in it still round-trips byte for byte.
    #[test]
    fn a_plain_tsv_round_trips() {
        let src = b"h1\th2\n1\t2\n\n3\t\n";
        let d = super::parse_delimited(src, b'\t', "S").unwrap();
        assert_eq!(
            super::serialize_delimited(&d, b'\t', false, b""),
            src.to_vec()
        );
    }

    #[test]
    fn blank_lines_survive_a_save() {
        let src = b"name\nann\n\nbob\n\n\n\"x\ny\"\n";
        let d = super::parse_delimited(src, b',', "S").unwrap();
        assert_eq!(d.rows.len(), 6, "{:?}", d.rows);
        let out = super::serialize_delimited(&d, b',', false, b"");
        assert_eq!(out, src.to_vec(), "{}", String::from_utf8_lossy(&out));
        let lead = super::parse_delimited(b"\n\nh\n1\n", b',', "S").unwrap();
        assert_eq!(lead.headers, vec!["h"]);
        let crlf = b"a,b\r\n1,2\r\n\r\n3,4\r\n";
        let d = super::parse_delimited(crlf, b',', "S").unwrap();
        assert_eq!(
            super::serialize_delimited(&d, b',', true, b""),
            crlf.to_vec()
        );
    }

    /// #1375: the repro, R `write.csv` / Python `QUOTE_ALL` style.
    const QUOTED_CSV: &[u8] = b"\"id\",\"name\",\"city\",\"score\"\n\"1\",\"Ada\",\"London\",\"91\"\n\"2\",\"Grace\",\"New York\",\"88\"\n\"3\",\"Linus\",\"Helsinki\",\"79\"\n";

    fn changed_lines(before: &[u8], after: &[u8]) -> Vec<(usize, String)> {
        let before = String::from_utf8_lossy(before);
        let after = String::from_utf8_lossy(after);
        let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(b.len(), a.len(), "line count changed:\n{after}");
        a.iter()
            .zip(&b)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, _))| (i, a.to_string()))
            .collect()
    }

    /// #1375: one cell edited in a fully quoted file changes that line only,
    /// and the line stays fully quoted.
    #[test]
    fn a_quote_all_csv_edit_rewrites_only_that_line_and_keeps_its_quotes() {
        let mut d = super::parse_delimited(QUOTED_CSV, b',', "S").unwrap();
        d.set_cell(0, 3, String::from("95"));
        let out = super::serialize_delimited(&d, b',', false, QUOTED_CSV);
        assert_eq!(
            changed_lines(QUOTED_CSV, &out),
            vec![(1, String::from("\"1\",\"Ada\",\"London\",\"95\""))]
        );
    }

    /// #1375: a `QUOTE_NONNUMERIC` file keeps numbers bare and text quoted
    /// on the edited line.
    #[test]
    fn a_quote_nonnumeric_csv_edit_keeps_numbers_bare_and_text_quoted() {
        let src = b"\"id\",\"name\",\"score\"\n1,\"Ada\",91\n2,\"Grace\",88.5\n";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(1, 1, String::from("Grace H"));
        let out = super::serialize_delimited(&d, b',', false, src);
        assert_eq!(
            changed_lines(src, &out),
            vec![(2, String::from("2,\"Grace H\",88.5"))]
        );
    }

    /// #1375: a row added to a fully quoted file is quoted like the rest,
    /// its empty cells included.
    #[test]
    fn a_row_inserted_into_a_quote_all_csv_is_quoted_too() {
        let mut d = super::parse_delimited(QUOTED_CSV, b',', "S").unwrap();
        d.insert_row(3);
        d.set_cell(3, 1, String::from("Ken"));
        let out =
            String::from_utf8(super::serialize_delimited(&d, b',', false, QUOTED_CSV)).unwrap();
        assert!(
            out.starts_with(std::str::from_utf8(QUOTED_CSV).unwrap()),
            "{out}"
        );
        assert!(out.ends_with("\"\",\"Ken\",\"\",\"\"\n"), "{out}");
    }

    /// #1375: sorting moves rows but each keeps its own bytes.
    #[test]
    fn sorted_rows_keep_their_original_bytes() {
        let mut d = super::parse_delimited(QUOTED_CSV, b',', "S").unwrap();
        d.sort_by_column(3);
        let out =
            String::from_utf8(super::serialize_delimited(&d, b',', false, QUOTED_CSV)).unwrap();
        assert_eq!(
            out,
            "\"id\",\"name\",\"city\",\"score\"\n\"3\",\"Linus\",\"Helsinki\",\"79\"\n\"2\",\"Grace\",\"New York\",\"88\"\n\"1\",\"Ada\",\"London\",\"91\"\n"
        );
    }

    /// #1375 negative: an unquoted file saves as it always has, quoting only
    /// a cell that needs it.
    #[test]
    fn an_unquoted_csv_edit_still_quotes_only_when_needed() {
        let src = b"id,name,city\n1,Ada,London\n2,Grace,Paris\n";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(0, 2, String::from("London, UK"));
        let out = super::serialize_delimited(&d, b',', false, src);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "id,name,city\n1,Ada,\"London, UK\"\n2,Grace,Paris\n"
        );
    }

    /// #1375 negative: a file that mixes styles (R's bare `NA`) leaves its
    /// untouched lines alone and writes the edited one with the minimal
    /// quoting, rather than guessing a style the file does not have.
    #[test]
    fn a_mixed_quoting_csv_keeps_untouched_lines_and_quotes_the_edit_minimally() {
        let src = b"\"id\",\"name\",\"score\"\n1,\"Ada\",NA\n2,\"Grace\",88\n";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(1, 2, String::from("90"));
        let out = super::serialize_delimited(&d, b',', false, src);
        assert_eq!(
            changed_lines(src, &out),
            vec![(2, String::from("2,Grace,90"))]
        );
    }

    /// #1375 negative: a saved file the grid did not touch is byte-identical,
    /// odd spacing and a CRLF ending included.
    #[test]
    fn an_untouched_csv_saves_byte_for_byte() {
        let src = b"\"a\" ,b\r\n\"1\",2\r\n";
        let d = super::parse_delimited(src, b',', "S").unwrap();
        assert_eq!(
            super::serialize_delimited(&d, b',', true, src),
            src.to_vec()
        );
    }

    /// #1375: of two equal rows written differently, the untouched one
    /// keeps its own bytes when the other is edited.
    #[test]
    fn equal_rows_written_differently_each_keep_their_bytes() {
        let src = b"h\n\"x\"\nx\n";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(0, 0, String::from("y"));
        let out = super::serialize_delimited(&d, b',', false, src);
        assert_eq!(String::from_utf8(out).unwrap(), "h\ny\nx\n");
    }

    /// #1375: each untouched record keeps its own line ending, and a file
    /// without a final one still has none.
    #[test]
    fn untouched_records_keep_their_own_line_endings() {
        let src = b"h\na\nb";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(0, 0, String::from("z"));
        let out = super::serialize_delimited(&d, b',', false, src);
        assert_eq!(String::from_utf8(out).unwrap(), "h\nz\nb");
        let src = b"h\r\na\nb\r\n";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(1, 0, String::from("c"));
        let out = super::serialize_delimited(&d, b',', true, src);
        assert_eq!(String::from_utf8(out).unwrap(), "h\r\na\nc\r\n");
    }

    /// #1375: a CRLF file whose first field holds a newline is still CRLF
    /// for an edited row, whatever the caller guessed from its first `\n`.
    #[test]
    fn a_quoted_newline_does_not_set_the_line_ending() {
        let src = b"\"first\nsecond\",b\r\n1,2\r\n";
        let mut d = super::parse_delimited(src, b',', "S").unwrap();
        d.set_cell(0, 0, String::from("3"));
        let out = super::serialize_delimited(&d, b',', false, src);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\"first\nsecond\",b\r\n3,2\r\n"
        );
    }

    /// #1375: a UTF-8 BOM before a fully quoted header does not hide that
    /// the file quotes every field.
    #[test]
    fn a_bom_does_not_hide_quote_all() {
        let mut src = b"\xEF\xBB\xBF".to_vec();
        src.extend_from_slice(QUOTED_CSV);
        let mut d = super::parse_delimited(&src, b',', "S").unwrap();
        d.set_cell(0, 3, String::from("95"));
        let out = super::serialize_delimited(&d, b',', false, &src);
        assert!(out.starts_with(&src[..3]), "the BOM stays");
        assert_eq!(
            changed_lines(&src, &out),
            vec![(1, String::from("\"1\",\"Ada\",\"London\",\"95\""))]
        );
    }

    #[test]
    fn a_crlf_file_saves_as_crlf() {
        let d = super::parse_delimited(b"a,b\r\n1,2\r\n", b',', "S").unwrap();
        let out = super::serialize_delimited(&d, b',', true, b"");
        assert_eq!(String::from_utf8(out).unwrap(), "a,b\r\n1,2\r\n");
    }

    #[test]
    fn only_plain_finite_numbers_are_stored_as_xlsx_numbers() {
        assert_eq!(super::xlsx_number("42"), Some(42.0));
        assert_eq!(super::xlsx_number("-0.5"), Some(-0.5));
        assert_eq!(super::xlsx_number("0"), Some(0.0));
        assert_eq!(super::xlsx_number("1.5e3"), Some(1500.0));
        for text in [
            "02134",
            "nan",
            "inf",
            "-infinity",
            "1e400",
            "+5",
            "",
            ".5",
            "1-2",
        ] {
            assert_eq!(super::xlsx_number(text), None, "{text}");
        }
    }

    #[test]
    fn row_and_column_ops_keep_headers_and_widths_in_step() {
        let mut d = super::parse_delimited(b"a,b\n1,2\n3,4\n", b',', "S").unwrap();
        d.insert_row(1);
        assert_eq!(d.rows.len(), 3);
        assert_eq!(d.cell(1, 0), "");
        assert!(d.delete_row(1));
        d.insert_col(1);
        assert_eq!(d.headers, vec!["a", "", "b"]);
        assert_eq!(d.cell(0, 2), "2");
        assert_eq!(d.col_count(), 3);
        assert!(d.delete_col(1));
        assert_eq!(d.headers, vec!["a", "b"]);
        assert_eq!(d.cell(1, 1), "4");
        assert!(!d.delete_col(9), "out of range refuses");
        let out = super::serialize_delimited(&d, b',', false, b"");
        assert_eq!(String::from_utf8(out).unwrap(), "a,b\n1,2\n3,4\n");
    }

    #[test]
    fn empty_sheet_grows_structure_that_survives_a_save_round_trip() {
        // #193 review: on a fully empty sheet, insert column + row, edit,
        // save, reopen - the body row must NOT be swallowed as the header.
        let mut d = super::parse_delimited(b"", b',', "S").unwrap();
        d.insert_col(0);
        assert_eq!(d.headers, vec![String::new()], "the header cell exists");
        d.insert_row(0);
        d.set_cell(0, 0, String::from("v"));
        let out = super::serialize_delimited(&d, b',', false, b"");
        let again = super::parse_delimited(&out, b',', "S").unwrap();
        assert_eq!(again.rows.len(), 1, "the body row survives the reopen");
        assert_eq!(again.cell(0, 0), "v");
    }

    #[test]
    fn tsv_round_trips_with_its_own_delimiter() {
        let mut d = super::parse_delimited(b"x\ty\n1\t2\n", b'\t', "S").unwrap();
        d.set_cell(0, 0, String::from("9"));
        let out = super::serialize_delimited(&d, b'\t', false, b"");
        assert_eq!(String::from_utf8(out).unwrap(), "x\ty\n9\t2\n");
    }

    use super::*;

    #[test]
    fn parse_csv_picks_up_header_and_rows() {
        let csv = b"name,age,city\nAlice,30,NYC\nBob,25,SFO\n";
        let s = parse_delimited(csv, b',', "Sheet1").unwrap();
        assert_eq!(s.headers, vec!["name", "age", "city"]);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0], vec!["Alice", "30", "NYC"]);
        assert_eq!(s.col_widths.len(), 3);
        assert!(s.col_widths[0] >= "Alice".len() as u16);
    }

    #[test]
    fn parse_csv_handles_quoted_fields_with_embedded_commas() {
        let csv = b"a,b\n\"hello, world\",2\n";
        let s = parse_delimited(csv, b',', "Sheet1").unwrap();
        assert_eq!(s.rows[0], vec!["hello, world", "2"]);
    }

    #[test]
    fn parse_tsv_uses_tab_delimiter() {
        let tsv = b"col1\tcol2\nval1\tval2\n";
        let s = parse_delimited(tsv, b'\t', "Sheet1").unwrap();
        assert_eq!(s.headers, vec!["col1", "col2"]);
        assert_eq!(s.rows[0], vec!["val1", "val2"]);
    }

    #[test]
    fn parse_csv_tolerates_ragged_rows() {
        // Flexible mode: a short row doesn't kill the parse.
        let csv = b"a,b,c\nshort\nx,y,z\n";
        let s = parse_delimited(csv, b',', "Sheet1").unwrap();
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0], vec!["short"]);
        assert_eq!(s.rows[1], vec!["x", "y", "z"]);
    }

    #[test]
    fn col_widths_clamp_to_max() {
        let very_long: String = "x".repeat(200);
        let csv = format!("a,b\n{very_long},2\n");
        let s = parse_delimited(csv.as_bytes(), b',', "Sheet1").unwrap();
        assert_eq!(s.col_widths[0], MAX_COL_DISPLAY_W);
    }

    #[test]
    fn empty_csv_produces_empty_sheet() {
        let s = parse_delimited(b"", b',', "Sheet1").unwrap();
        assert!(s.headers.is_empty());
        assert!(s.rows.is_empty());
        assert!(s.col_widths.is_empty());
    }

    #[test]
    fn extension_classifier() {
        for ext in ["csv", "CSV", "tsv", "xlsx", "XLSX", "xls", "ods", "xlsb"] {
            assert!(extension_is_sheet(ext), "should accept: {ext}");
        }
        for ext in ["txt", "md", "rs", ""] {
            assert!(!extension_is_sheet(ext));
        }
    }
}
