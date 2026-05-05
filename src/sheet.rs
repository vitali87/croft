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
            let delim = if matches!(kind, SheetKind::Tsv) { b'\t' } else { b',' };
            let sheet = parse_delimited(&bytes, delim, "Sheet1")
                .map_err(|e| std::io::Error::other(format!("CSV parse: {e}")))?;
            Ok(SheetView {
                kind,
                source_byte_size: meta.len(),
                sheets: vec![sheet],
                current_sheet: 0,
            })
        }
        SheetKind::Xlsx | SheetKind::Xls | SheetKind::Ods | SheetKind::Xlsb => {
            if meta.len() > XLSX_BYTES_CAP {
                return Err(std::io::Error::other(format!(
                    "Spreadsheet too large ({} bytes)",
                    meta.len()
                )));
            }
            let sheets = read_calamine_workbook(path)
                .map_err(|e| std::io::Error::other(format!("workbook open: {e}")))?;
            if sheets.is_empty() {
                return Err(std::io::Error::other("workbook has no sheets"));
            }
            Ok(SheetView {
                kind,
                source_byte_size: meta.len(),
                sheets,
                current_sheet: 0,
            })
        }
    }
}

pub fn parse_delimited(
    bytes: &[u8],
    delim: u8,
    sheet_name: &str,
) -> Result<SheetData, csv::Error> {
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
    })
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

fn read_calamine_workbook(path: &Path) -> Result<Vec<SheetData>, calamine::Error> {
    use calamine::{Data, Reader};
    let mut workbook: calamine::Sheets<_> = calamine::open_workbook_auto(path)?;
    let sheet_names = workbook.sheet_names();
    let mut out: Vec<SheetData> = Vec::with_capacity(sheet_names.len());
    for name in sheet_names {
        let range = match workbook.worksheet_range(&name) {
            Ok(r) => r,
            Err(_) => continue,
        };
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
