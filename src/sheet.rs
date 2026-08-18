use std::path::Path;

const CSV_BYTES_CAP: u64 = 25 * 1024 * 1024;
const XLSX_BYTES_CAP: u64 = 50 * 1024 * 1024;
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
}

impl SheetData {
    pub fn col_count(&self) -> usize {
        self.col_widths.len()
    }
    pub fn row_count(&self) -> usize {
        self.rows.len()
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
            })
        }
        SheetKind::Xlsx | SheetKind::Xls | SheetKind::Ods | SheetKind::Xlsb => {
            if meta.len() > XLSX_BYTES_CAP {
                return Err(std::io::Error::other(format!(
                    "Spreadsheet too large ({} bytes)",
                    meta.len()
                )));
            }
            let sheets = read_calamine_workbook(path, kind)
                .map_err(|e| std::io::Error::other(format!("workbook open: {e}")))?;
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
            })
        }
    }
}

pub fn parse_delimited(bytes: &[u8], delim: u8, sheet_name: &str) -> Result<SheetData, csv::Error> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .from_reader(bytes);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let r = record?;
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
    })
}

impl SheetData {
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
/// (it is the file's own row 0, split off at parse), then the body,
/// quoted per csv rules with the delimiter the file was READ with.
pub fn serialize_delimited(data: &SheetData, delim: u8) -> Vec<u8> {
    let mut w = csv::WriterBuilder::new()
        .delimiter(delim)
        .flexible(true)
        .from_writer(Vec::new());
    if !data.headers.is_empty() {
        let _ = w.write_record(&data.headers);
    }
    for r in &data.rows {
        let _ = w.write_record(r);
    }
    w.into_inner().unwrap_or_default()
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
            formula_skipped.push(format!("{}{row}", column_letters(col)));
            continue;
        }
        if !value.is_empty() && value.parse::<f64>().is_ok() {
            cell.set_value_number(value.parse::<f64>().expect("checked"));
        } else {
            cell.set_value(value);
        }
        written += 1;
    }
    umya_spreadsheet::writer::xlsx::write(&book, path).map_err(|e| e.to_string())?;
    Ok(XlsxSaveReport {
        written,
        formula_skipped,
    })
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

fn read_calamine_workbook(path: &Path, kind: SheetKind) -> Result<Vec<SheetData>, calamine::Error> {
    use calamine::{Data, Reader};
    // `open_workbook_auto` resolves the reader from the file EXTENSION,
    // so a content-routed file without one (#174) needs the explicit
    // reader for its sniffed kind.
    let mut workbook: calamine::Sheets<_> = match calamine::open_workbook_auto(path) {
        Ok(w) => w,
        Err(auto_err) => match kind {
            SheetKind::Xlsx => calamine::Sheets::Xlsx(calamine::open_workbook(path)?),
            SheetKind::Xls => calamine::Sheets::Xls(calamine::open_workbook(path)?),
            SheetKind::Ods => calamine::Sheets::Ods(calamine::open_workbook(path)?),
            SheetKind::Xlsb => calamine::Sheets::Xlsb(calamine::open_workbook(path)?),
            SheetKind::Csv | SheetKind::Tsv => return Err(auto_err),
        },
    };
    let sheet_names = workbook.sheet_names();
    let mut out: Vec<SheetData> = Vec::with_capacity(sheet_names.len());
    for name in sheet_names {
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
        assert_eq!(report.formula_skipped, vec![String::from("B3")]);

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
        let out = super::serialize_delimited(&d, b',');
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a,b,c\n1,2,\"x,y\"\n",
            "delimiter-bearing cells are quoted, header row survives"
        );
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
        let out = super::serialize_delimited(&d, b',');
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
        let out = super::serialize_delimited(&d, b',');
        let again = super::parse_delimited(&out, b',', "S").unwrap();
        assert_eq!(again.rows.len(), 1, "the body row survives the reopen");
        assert_eq!(again.cell(0, 0), "v");
    }

    #[test]
    fn tsv_round_trips_with_its_own_delimiter() {
        let mut d = super::parse_delimited(b"x\ty\n1\t2\n", b'\t', "S").unwrap();
        d.set_cell(0, 0, String::from("9"));
        let out = super::serialize_delimited(&d, b'\t');
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
