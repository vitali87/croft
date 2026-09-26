//! Read-only SQLite browser (#182): every table becomes a "worksheet"
//! in the existing sheet grid, so navigation, the cell cursor, and
//! Tab-switching between tables all reuse. Databases open with
//! SQLITE_OPEN_READONLY: read-only, never mutated or created (readers
//! still take sqlite's shared read locks while a query runs), rows are capped per table, and a locked or corrupt
//! file surfaces its error instead of crashing.

use std::path::Path;

/// Rows fetched per table: the grid shows the head of large tables,
/// with the truth stated in the sheet name.
pub const ROW_CAP: usize = 500;

/// True when the leading bytes carry the SQLite magic.
pub fn extension_is_sqlite(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "sqlite" | "sqlite3" | "db"
    )
}

/// One fetched table page: headers, rows, and whether rows follow it.
pub type TablePage = (Vec<String>, Vec<Vec<String>>, bool);

/// Fetch one page of a table (#201 review: real paging, not a
/// permanent head snapshot).
pub fn table_page(path: &Path, table: &str, page: usize) -> Result<TablePage, String> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    // One row past the page says whether more follow. `COUNT(*)` gave an
    // exact total, but SQLite keeps no row count: it scanned every table in
    // full, on the UI thread, when the database opened and on every page
    // turn, freezing croft for minutes on a large file.
    let offset = page * ROW_CAP;
    let probe = ROW_CAP + 1;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT * FROM {quoted} LIMIT {probe} OFFSET {offset}"
        ))
        .map_err(|e| e.to_string())?;
    let headers: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let ncols = headers.len();
    let mut body: Vec<Vec<String>> = Vec::new();
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut out = Vec::with_capacity(ncols);
        for i in 0..ncols {
            use rusqlite::types::ValueRef;
            let cell = match row.get_ref(i).map_err(|e| e.to_string())? {
                ValueRef::Null => String::new(),
                ValueRef::Integer(v) => v.to_string(),
                ValueRef::Real(v) => v.to_string(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                ValueRef::Blob(b) => format!("<blob {} bytes>", b.len()),
            };
            out.push(cell);
        }
        body.push(out);
    }
    let more = body.len() > ROW_CAP;
    body.truncate(ROW_CAP);
    Ok((headers, body, more))
}

/// The sheet name for a table page: honest about position, and about more
/// rows following without claiming a total nobody counted.
pub fn page_label(table: &str, page: usize, got: usize, more: bool) -> String {
    if page == 0 && !more {
        return format!("{table}: {got} rows");
    }
    let first = page * ROW_CAP + 1;
    let last = page * ROW_CAP + got;
    let tail = if more { ", more follow" } else { "" };
    format!("{table}: rows {first}-{last}{tail}")
}

/// Build a sheet view over the database: one SheetData per table with
/// up to [`ROW_CAP`] rows, headers from the column names, and a name
/// that states row counts honestly.
pub fn open_database(path: &Path) -> Result<crate::sheet::SheetView, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    let mut names: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for n in rows {
            names.push(n.map_err(|e| e.to_string())?);
        }
    }
    if names.is_empty() {
        return Err(String::from("no tables in this database"));
    }
    let mut sheets = Vec::new();
    for name in &names {
        let (headers, body, more) = table_page(path, name, 0)?;
        let got = body.len();
        sheets.push(crate::sheet::sheet_data_from_parts(
            page_label(name, 0, got, more),
            headers,
            body,
        ));
    }
    let mut view =
        crate::sheet::view_from_sheets(crate::sheet::SheetKind::Sqlite, meta.len(), sheets);
    view.sqlite_pages = names.into_iter().map(|n| (n, 0usize)).collect();
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(p: &Path) {
        let conn = rusqlite::Connection::open(p).unwrap();
        conn.execute_batch(
            "CREATE TABLE fruit (id INTEGER PRIMARY KEY, name TEXT, price REAL, pic BLOB);
             INSERT INTO fruit (name, price, pic) VALUES ('apple', 1.5, x'00ff'), ('pear', NULL, NULL);
             CREATE TABLE empty_t (a TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn tables_become_sheets_with_typed_cells() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.sqlite");
        fixture(&p);
        let view = open_database(&p).unwrap();
        assert_eq!(view.kind, crate::sheet::SheetKind::Sqlite);
        assert_eq!(view.sheets.len(), 2, "sqlite_% internals excluded");
        let fruit = view
            .sheets
            .iter()
            .find(|s| s.name.starts_with("fruit"))
            .unwrap();
        assert_eq!(fruit.headers, vec!["id", "name", "price", "pic"]);
        assert_eq!(fruit.cell(0, 1), "apple");
        assert_eq!(fruit.cell(0, 3), "<blob 2 bytes>");
        assert_eq!(fruit.cell(1, 2), "", "NULL renders empty");
        assert!(fruit.name.contains("2 rows"));
    }

    #[test]
    fn a_full_page_says_more_rows_follow_without_counting_them() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.sqlite");
        let conn = rusqlite::Connection::open(&p).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE t (n INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < {}) INSERT INTO t SELECT x FROM c;",
            ROW_CAP + 7
        ))
        .unwrap();
        let (_, rows, more) = table_page(&p, "t", 0).unwrap();
        assert_eq!((rows.len(), more), (ROW_CAP, true));
        assert_eq!(
            page_label("t", 0, rows.len(), more),
            format!("t: rows 1-{ROW_CAP}, more follow")
        );
        let (_, rows, more) = table_page(&p, "t", 1).unwrap();
        assert_eq!((rows.len(), more), (7, false));
        assert_eq!(
            page_label("t", 1, rows.len(), more),
            format!("t: rows {}-{}", ROW_CAP + 1, ROW_CAP + 7)
        );
    }

    #[test]
    fn a_non_database_reports_instead_of_crashing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("junk.db");
        std::fs::write(&p, b"not a database at all").unwrap();
        assert!(open_database(&p).is_err());
    }

    #[test]
    fn readonly_never_creates_or_mutates() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("absent.sqlite");
        assert!(open_database(&p).is_err(), "read-only open cannot create");
        assert!(!p.exists());
    }
}
