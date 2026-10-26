//! Read-only SQLite browser (#182): every table and view becomes a
//! "worksheet" in the existing sheet grid, so navigation, the cell cursor,
//! and Tab-switching between tables all reuse. Databases open with
//! SQLITE_OPEN_READONLY: read-only, never mutated or created (readers
//! still take sqlite's shared read locks while a query runs), rows are capped per table, and a locked or corrupt
//! file surfaces its error instead of crashing.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

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

/// One fetched page of a table or view.
#[derive(Debug)]
pub struct TablePage {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// `(row, col)` of the cells holding SQL NULL (#1002), which `rows`
    /// carries as empty strings: the grid draws them as a dimmed `NULL`, so
    /// they read apart from `''`.
    pub nulls: HashSet<(usize, usize)>,
    /// Whether rows follow this page.
    pub more: bool,
    /// Whether the name is a view rather than a table (#1002).
    pub view: bool,
}

impl TablePage {
    /// The page as the sheet the grid shows: labelled by its place in the
    /// table, numbered from there (#1222), its NULLs marked.
    pub fn into_sheet(self, table: &str, page: usize) -> crate::sheet::SheetData {
        let name = if self.view {
            format!("{table} (view)")
        } else {
            table.to_string()
        };
        let label = page_label(&name, page, self.rows.len(), self.more);
        let mut data = crate::sheet::sheet_data_from_parts(label, self.headers, self.rows);
        data.row_base = page * ROW_CAP;
        for &(_, col) in &self.nulls {
            if let Some(w) = data.col_widths.get_mut(col) {
                *w = (*w).max(NULL_LABEL.len() as u16);
            }
        }
        data.null_cells = self.nulls;
        data
    }
}

/// What the grid shows for a SQL NULL.
pub const NULL_LABEL: &str = "NULL";

/// How long one query over a view may run before it is stopped (#1002). A
/// view is a query the database file defines, and `LIMIT` bounds the rows
/// it returns, not the work behind them: an aggregate over a huge join, or
/// a recursive CTE that never ends, would otherwise freeze croft on open,
/// since every view's first page is fetched then, on the UI thread.
pub const VIEW_BUDGET: Duration = Duration::from_secs(1);

/// A read-only connection for querying `table`, and whether it is a view.
/// A view's queries are stopped once they run past [`VIEW_BUDGET`].
fn open_for(path: &Path, table: &str) -> Result<(rusqlite::Connection, bool), String> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    let view = is_view(&conn, table);
    if view {
        let start = Instant::now();
        conn.progress_handler(10_000, Some(move || start.elapsed() > VIEW_BUDGET))
            .map_err(|e| e.to_string())?;
    }
    Ok((conn, view))
}

/// A query error as the sheet or status bar says it: a view stopped by
/// [`VIEW_BUDGET`] says so rather than SQLite's bare "interrupted".
fn query_error(e: rusqlite::Error) -> String {
    if e.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
        format!(
            "stopped after {} s: the view takes too long to compute",
            VIEW_BUDGET.as_secs()
        )
    } else {
        e.to_string()
    }
}

/// Fetch one page of a table or view (#201 review: real paging, not a
/// permanent head snapshot).
pub fn table_page(path: &Path, table: &str, page: usize) -> Result<TablePage, String> {
    let (conn, view) = open_for(path, table)?;
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
        .map_err(query_error)?;
    let headers: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let ncols = headers.len();
    let mut body: Vec<Vec<String>> = Vec::new();
    let mut nulls = HashSet::new();
    let mut rows = stmt.query([]).map_err(query_error)?;
    while let Some(row) = rows.next().map_err(query_error)? {
        let mut out = Vec::with_capacity(ncols);
        for i in 0..ncols {
            use rusqlite::types::ValueRef;
            let cell = match row.get_ref(i).map_err(|e| e.to_string())? {
                ValueRef::Null => {
                    nulls.insert((body.len(), i));
                    String::new()
                }
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
    nulls.retain(|&(row, _)| row < ROW_CAP);
    Ok(TablePage {
        headers,
        rows: body,
        nulls,
        more,
        view,
    })
}

/// The index of a table's last page (#1222), for Ctrl+End. Counting costs a
/// scan, the same as the OFFSET fetch of that page does, so it runs only
/// when asked for the end, never on open or on a page turn.
pub fn last_page(path: &Path, table: &str) -> Result<usize, String> {
    let (conn, _) = open_for(path, table)?;
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    let rows: i64 = conn
        .query_row(&format!("SELECT count(*) FROM {quoted}"), [], |r| r.get(0))
        .map_err(query_error)?;
    Ok((rows.max(1) as usize - 1) / ROW_CAP)
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

/// Build a sheet view over the database: one SheetData per table and view
/// with up to [`ROW_CAP`] rows, headers from the column names, and a name
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
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name")
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
        match table_page(path, name, 0) {
            Ok(page) => sheets.push(page.into_sheet(name, 0)),
            // A view over a table that is gone fails only when queried; it
            // is the one sheet that says so, not a database that won't open.
            Err(e) if is_view(&conn, name) => sheets.push(crate::sheet::sheet_data_from_parts(
                format!("{name} (view): {e}"),
                Vec::new(),
                Vec::new(),
            )),
            Err(e) => return Err(e),
        }
    }
    let mut view =
        crate::sheet::view_from_sheets(crate::sheet::SheetKind::Sqlite, meta.len(), sheets);
    view.sqlite_pages = names.into_iter().map(|n| (n, 0usize)).collect();
    Ok(view)
}

/// Whether `name` is a view rather than a table.
fn is_view(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT type FROM sqlite_master WHERE name = ?1",
        [name],
        |r| r.get::<_, String>(0),
    )
    .is_ok_and(|t| t == "view")
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
        assert_eq!(fruit.cell(1, 2), "", "NULL is stored empty");
        assert!(fruit.null_cells.contains(&(1, 2)), "and marked as NULL");
        assert!(!fruit.null_cells.contains(&(0, 2)));
        assert!(fruit.name.contains("2 rows"));
    }

    /// #1002: NULL reads apart from `''`, text keeps its newlines and tabs
    /// (the grid shows them), and views are listed beside the tables,
    /// labelled, and paged the same way. A view over a dropped table is a
    /// sheet that says why, not a database that will not open.
    #[test]
    fn nulls_views_and_multiline_text_come_through() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("shop.db");
        let conn = rusqlite::Connection::open(&p).unwrap();
        conn.execute_batch(
            "CREATE TABLE customers (name TEXT, country TEXT, notes TEXT);
             INSERT INTO customers VALUES ('Ann', NULL, ''), ('Bo', '', 'line1' || char(10) || 'line2' || char(9) || 'tabbed');
             CREATE TABLE orders (id INTEGER, total REAL);
             INSERT INTO orders VALUES (1, 5.0), (2, 250.0);
             CREATE VIEW big_orders AS SELECT id FROM orders WHERE total > 100;
             CREATE TABLE gone (a INTEGER);
             CREATE VIEW stale AS SELECT a FROM gone;
             DROP TABLE gone;",
        )
        .unwrap();
        drop(conn);
        let view = open_database(&p).unwrap();
        let names: Vec<&str> = view.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        assert_eq!(names[0], "big_orders (view): 1 rows", "{names:?}");
        assert!(names[3].starts_with("stale (view): "), "{names:?}");
        let pages: Vec<&str> = view.sqlite_pages.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(pages, ["big_orders", "customers", "orders", "stale"]);
        assert_eq!(view.sheets[0].cell(0, 0), "2");

        let customers = &view.sheets[1];
        assert!(
            customers.null_cells.contains(&(0, 1)),
            "Ann's country is NULL"
        );
        assert!(!customers.null_cells.contains(&(1, 1)), "Bo's is ''");
        assert!(
            !customers.null_cells.contains(&(0, 2)),
            "Ann's notes are ''"
        );
        assert_eq!(customers.cell(1, 2), "line1\nline2\ttabbed");

        let page = table_page(&p, "big_orders", 0).unwrap();
        assert!(page.view && !page.more);
        assert_eq!(
            page.into_sheet("big_orders", 0).name,
            "big_orders (view): 1 rows"
        );
        assert_eq!(last_page(&p, "big_orders"), Ok(0));
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
        let page = table_page(&p, "t", 0).unwrap();
        assert_eq!((page.rows.len(), page.more), (ROW_CAP, true));
        assert_eq!(
            page_label("t", 0, page.rows.len(), page.more),
            format!("t: rows 1-{ROW_CAP}, more follow")
        );
        let page = table_page(&p, "t", 1).unwrap();
        assert_eq!((page.rows.len(), page.more), (7, false));
        let data = page.into_sheet("t", 1);
        assert_eq!(
            data.name,
            format!("t: rows {}-{}", ROW_CAP + 1, ROW_CAP + 7)
        );
        assert_eq!(data.row_base, ROW_CAP);
    }

    /// #1002 review: a view is a query the file defines, and one that never
    /// returns a row must not freeze the open. It is stopped at
    /// [`VIEW_BUDGET`] and its sheet says so; the tables still open.
    #[test]
    fn a_view_that_never_finishes_is_stopped_instead_of_freezing_the_open() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("forever.db");
        rusqlite::Connection::open(&p)
            .unwrap()
            .execute_batch(
                "CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (1);
                 CREATE VIEW forever AS
                   WITH RECURSIVE r(x) AS (VALUES(1) UNION ALL SELECT x FROM r)
                   SELECT count(*) FROM r;",
            )
            .unwrap();
        let start = Instant::now();
        let view = open_database(&p).unwrap();
        assert!(start.elapsed() < VIEW_BUDGET * 5, "{:?}", start.elapsed());
        let names: Vec<&str> = view.sheets.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names[0].starts_with("forever (view): stopped after"),
            "{names:?}"
        );
        assert_eq!(names[1], "t: 1 rows");
        let err = last_page(&p, "forever").unwrap_err();
        assert!(err.starts_with("stopped after"), "{err}");
    }

    /// #1222: the last page's index, and 0 for an empty or one-page table.
    #[test]
    fn last_page_is_where_the_tables_rows_end() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("pages.sqlite");
        let conn = rusqlite::Connection::open(&p).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE e (n INTEGER); CREATE TABLE full_page (n INTEGER);
             CREATE TABLE t (n INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < {}) INSERT INTO t SELECT x FROM c;
             WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < {ROW_CAP}) INSERT INTO full_page SELECT x FROM c;",
            2 * ROW_CAP + 7
        ))
        .unwrap();
        assert_eq!(last_page(&p, "t"), Ok(2));
        assert_eq!(last_page(&p, "full_page"), Ok(0), "exactly one page");
        assert_eq!(last_page(&p, "e"), Ok(0));
        assert!(last_page(&p, "missing").is_err());
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
