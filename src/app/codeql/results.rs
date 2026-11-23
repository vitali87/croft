//! A run's results: walking a results table, decoding result sets, choosing a set, the
//! alert and raw-results views, comparing two runs' results, and exporting them.

use super::*;

impl App {
    /// VS Code's "Export Results" (#578): ask where to copy the results of
    /// the selected history entry, else of the newest successful run,
    /// suggesting `<query-stem>-results.<csv|sarif>` in the workspace.
    pub(in crate::app) fn prompt_export_codeql_results(&mut self) {
        use crate::codeql_query::{History, RunStatus};
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let history = History::load(&Self::codeql_history_path());
        let Some(entry) = self
            .codeql
            .selected_history()
            .filter(|&i| i < history.entries.len())
            .or_else(|| history.newest_success())
            .and_then(|i| history.entries.get(i))
        else {
            self.status = String::from("There is no successful CodeQL query run to export");
            return;
        };
        if entry.status != RunStatus::Succeeded {
            self.status = format!(
                "{} did not succeed, so it has no results to export",
                entry.query_name()
            );
            return;
        }
        if !entry.output.is_file() {
            self.status = format!("The results of {} are gone", entry.query_name());
            return;
        }
        let dest = self
            .workspace_root()
            .join(crate::codeql_query::export_file_name(
                &entry.query,
                &entry.output,
            ));
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlExportResults {
                    output: entry.output.clone(),
                },
                format!("Export the results of {}", entry.query_name()),
                "the file to copy the results to",
            )
            .with_value(dest.display().to_string()),
        );
    }

    /// Copy the results at `output` to the typed path; an existing file is
    /// never overwritten.
    pub(in crate::app) fn submit_export_codeql_results(&mut self, output: &Path, value: &str) {
        if value.trim().is_empty() {
            self.status = String::from("Name a file to export the results to");
            return;
        }
        let dest = self.typed_path(value);
        // Open the results first: a missing or unreadable run then fails
        // before anything is created at the destination.
        let mut src = match std::fs::File::open(output) {
            Ok(src) => src,
            Err(e) => {
                self.status = format!("Could not read the results at {}: {e}", output.display());
                return;
            }
        };
        // `create_new` refuses an existing file (or a dangling symlink) in
        // the same step that creates it, so nothing can slip in between a
        // check and the copy.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest);
        let mut file = match created {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                self.status = format!(
                    "{} already exists; export the results to a new file",
                    dest.display()
                );
                return;
            }
            Err(e) => {
                self.status = format!("Could not export the results to {}: {e}", dest.display());
                return;
            }
        };
        // A copy failing midway leaves the partial file in place: removing
        // it by name could delete a file another process put there since.
        self.status = match std::io::copy(&mut src, &mut file) {
            Ok(_) => format!("Exported results to {}", dest.display()),
            Err(e) => format!(
                "Could not finish exporting to {} ({e}); the file there is incomplete",
                dest.display()
            ),
        };
    }

    /// VS Code's "Compare Results" for history entry `index` (#578): its
    /// table against the latest earlier run of the same query, written
    /// beside its results as a CSV of the rows only one run has, and opened.
    pub(in crate::app) fn compare_codeql_results(&mut self, index: usize) {
        use crate::codeql_query::RunStatus;
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = history.entries.get(index) else {
            return;
        };
        if entry.status != RunStatus::Succeeded {
            self.status = format!(
                "{} did not finish, so it has no results",
                entry.display_name()
            );
            return;
        }
        if entry.output.extension().is_none_or(|e| e != "csv") {
            self.status =
                String::from("Only table results can be compared; alerts open in the SARIF viewer");
            return;
        }
        let Some(partner) = history.compare_partner(index).map(|i| &history.entries[i]) else {
            self.status = format!(
                "There is no earlier successful run of {} to compare with",
                entry.query_name()
            );
            return;
        };
        let read =
            |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
        let compared = read(&partner.output).and_then(|old| {
            let new = read(&entry.output)?;
            crate::codeql_query::compare_tables(
                &old,
                &new,
                &format!("earlier ({})", partner.database),
                &format!("later ({})", entry.database),
            )
        });
        let text = match compared {
            Ok(text) => text,
            Err(why) => {
                self.status = format!("Could not compare the results: {why}");
                return;
            }
        };
        let out = entry
            .output
            .with_file_name(format!("compare-{}.csv", partner.started));
        if let Err(e) = std::fs::write(&out, &text) {
            self.status = format!("{}: {e}", out.display());
            return;
        }
        let differing = text.lines().count().saturating_sub(1);
        match self.editor.open(&out) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.status = match differing {
                    0 => String::from("Both runs have the same results"),
                    1 => String::from("1 row differs between the runs"),
                    n => format!("{n} rows differ between the runs"),
                };
            }
            Err(e) => self.status = format!("{}: {e}", out.display()),
        }
    }
}

impl App {
    /// Walk a CodeQL results table (#578, VS Code's `codeQLQueryResults`
    /// up/down/left/right): move its cursor by (`rows`, `cols`) and open
    /// the code of the cell's entity, else of the first entity in its row.
    /// The table is the active results sheet, else the one walked last,
    /// so the walk goes on once the code has the focus.
    pub(in crate::app) fn walk_codeql_results(&mut self, rows: isize, cols: isize) {
        use crate::codeql_query as cq;
        let active = self
            .editor
            .path
            .clone()
            .filter(|p| self.editor.sheet.is_some() && cq::locations_path(p).is_file());
        let on_table = active.is_some();
        let Some(table) = active.or_else(|| self.codeql_results_walk.clone()) else {
            self.status = String::from("Open a CodeQL results table first");
            return;
        };
        // The active tab when it is the table, else a tab showing it.
        let tab = if on_table {
            Some(&mut *self.editor)
        } else {
            self.editor
                .editors
                .iter_mut()
                .find(|t| t.path.as_deref() == Some(table.as_path()) && t.sheet.is_some())
        };
        let Some(sheet) = tab.and_then(|t| t.sheet.as_mut()) else {
            self.codeql_results_walk = None;
            self.status = String::from("Open a CodeQL results table first");
            return;
        };
        let current = sheet.current_sheet;
        let Some(data) = sheet.sheets.get_mut(current) else {
            return;
        };
        if data.rows.is_empty() {
            self.status = String::from("The results table is empty");
            return;
        }
        let step = |at: usize, by: isize, len: usize| -> usize {
            (at as isize + by).clamp(0, len.saturating_sub(1) as isize) as usize
        };
        data.cur_row = step(data.cur_row, rows, data.rows.len());
        data.cur_col = step(data.cur_col, cols, data.col_widths.len().max(1));
        let (cells, col) = (data.rows[data.cur_row].clone(), data.cur_col);
        let row = data.cur_row + 1;
        self.codeql_results_walk = Some(table.clone());
        let saved: Vec<cq::RowLocs> = std::fs::read_to_string(cq::locations_path(&table))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let Some(locs) = cq::row_locations(&saved, &cells) else {
            self.status = format!("Result {row} has no location");
            return;
        };
        let Some(loc) = locs
            .get(col)
            .cloned()
            .flatten()
            .or_else(|| locs.iter().flatten().next().cloned())
        else {
            self.status = format!("Result {row} has no location");
            return;
        };
        let Some(path) = self.codeql_source_file(&loc.path) else {
            self.status = format!("{} is not on this machine", loc.path.display());
            return;
        };
        if self.editor.path.as_deref() == Some(table.as_path()) {
            self.editor.pin_active();
        }
        let (line, column) = (loc.line.max(1) - 1, loc.column.max(1) - 1);
        self.status = match self.open_at(&path, line as usize, column as usize) {
            Ok(()) => format!("Result {row}: {}:{}", path.display(), loc.line),
            Err(e) => format!("{}: {e}", path.display()),
        };
    }

    /// A source file a CodeQL result names, on this machine: the path
    /// itself, else its copy in an extracted source archive or in a
    /// database's `src` folder, which both mirror the original paths.
    pub(in crate::app) fn codeql_source_file(&self, path: &Path) -> Option<PathBuf> {
        if path.is_file() {
            return Some(path.to_path_buf());
        }
        let rel = path.strip_prefix("/").unwrap_or(path);
        let extracted = std::fs::read_dir(Self::codeql_source_cache_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path());
        let src = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path())
            .databases
            .into_iter()
            .map(|d| d.path.join("src"));
        extracted
            .chain(src)
            .map(|root| root.join(rel))
            .find(|p| p.is_file())
    }

    /// Decode `bqrs` to CSV, one file per result set (#578): the main set
    /// (`#select`) to `out`, each other beside it as `results-<set>.csv`.
    /// `bqrs decode` without a set writes them all into one CSV, header
    /// rows and all, so that is only the fallback for a BQRS whose sets
    /// cannot be listed. `run` runs each decode.
    pub(in crate::app) fn codeql_decode_sets(
        program: &Path,
        bqrs: &Path,
        out: &Path,
        run: &dyn Fn(Vec<String>) -> Result<(), String>,
    ) -> Result<(), String> {
        use crate::codeql_query as cq;
        let sets = Self::codeql_stdout(program, &cq::info_args(bqrs))
            .map(|json| cq::parse_result_sets(&json))
            .unwrap_or_default();
        let Some(main) = cq::main_result_set(&sets).map(str::to_string) else {
            return run(cq::decode_args(bqrs, out));
        };
        for (name, _) in &sets {
            let file = cq::result_set_file(out, name, *name == main);
            run(cq::decode_set_args(bqrs, &file, name))?;
            // Where each cell's entity is, for walking the results in the
            // code. Optional: a table without it still opens.
            let raw = file.with_extension("entities.json");
            if run(cq::decode_locations_args(bqrs, &raw, name)).is_ok()
                && let Ok(text) = std::fs::read_to_string(&raw)
                && let Ok(json) = serde_json::to_string(&cq::parse_row_locations(&text))
            {
                let _ = std::fs::write(cq::locations_path(&file), json);
            }
            let _ = std::fs::remove_file(&raw);
        }
        Ok(())
    }

    /// CodeQL: Show Result Set (#578): pick which result set of the Query
    /// History run selected (else the newest) to open.
    pub(in crate::app) fn show_codeql_result_sets(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = self
            .codeql
            .selected_history()
            .or_else(|| history.newest())
            .and_then(|i| history.entries.get(i))
            .cloned()
        else {
            self.status = String::from("No CodeQL query has run yet");
            return;
        };
        let name = entry.query_name();
        if entry.output.extension().is_none_or(|e| e != "csv") {
            self.status = format!("{name} found alerts, which open in the SARIF viewer");
            return;
        }
        let sets = crate::codeql_query::result_set_files(&entry.output);
        if sets.len() < 2 {
            self.status = format!("{name} has one result set");
            return;
        }
        let rows = sets
            .into_iter()
            .map(|(set, path)| ListRow {
                id: path.display().to_string(),
                label: set,
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(
                ListPurpose::CodeqlResultSet,
                format!("Result sets of {name}"),
                rows,
            ),
            "No result sets",
        );
    }

    /// The Query History run selected in the side bar, else the newest;
    /// says so when nothing has run.
    pub(in crate::app) fn codeql_chosen_run(
        &mut self,
    ) -> Option<crate::codeql_query::HistoryEntry> {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let entry = self
            .codeql
            .selected_history()
            .or_else(|| history.newest())
            .and_then(|i| history.entries.get(i))
            .cloned();
        if entry.is_none() {
            self.status = String::from("No CodeQL query has run yet");
        }
        entry
    }

    /// The folder of the database `entry` ran on, by its recorded path,
    /// else by its name in the store.
    pub(in crate::app) fn codeql_run_database(
        entry: &crate::codeql_query::HistoryEntry,
    ) -> Option<PathBuf> {
        entry.database_path.clone().or_else(|| {
            crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path())
                .databases
                .into_iter()
                .find(|d| d.name == entry.database)
                .map(|d| d.path)
        })
    }

    /// The alert views of a run (#578): "View Alerts (SARIF)" opens the
    /// SARIF as JSON; "View Alerts (CSV)" has `codeql database
    /// interpret-results` write the alerts as CSV from the results the
    /// database keeps, without running the query again.
    pub(in crate::app) fn view_codeql_alerts(&mut self, csv: bool) {
        let Some(entry) = self.codeql_chosen_run() else {
            return;
        };
        let name = entry.query_name();
        if entry.status != crate::codeql_query::RunStatus::Succeeded
            || entry.output.extension().is_none_or(|e| e != "sarif")
        {
            self.status = format!("{name} did not produce alerts; its results are a table");
            return;
        }
        if !csv {
            // Opened, then reopened as text: the override holds only for
            // the tab's own file, so it is set once the tab is on it.
            let opened = self.editor.open(&entry.output).and_then(|()| {
                self.editor.force_text = true;
                self.editor.open(&entry.output)
            });
            self.status = match opened {
                Ok(()) => format!("Alerts of {name} (SARIF)"),
                Err(e) => format!("{}: {e}", entry.output.display()),
            };
            return;
        }
        let Some(db) = Self::codeql_run_database(&entry) else {
            self.status = format!("The database {name} ran on is gone", name = entry.database);
            return;
        };
        if self.codeql_doc_job.is_some() {
            self.status = String::from("A CodeQL view is already being made");
            return;
        }
        let out = entry.output.with_file_name("alerts.csv");
        let program = self.codeql_program.clone();
        let query = entry.query.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let made = Self::codeql_command(
                &program,
                &crate::codeql_query::interpret_csv_args(&db, &query, &out),
            )
            .map(|()| out);
            let _ = tx.send(made);
        });
        self.status = format!("Writing the alerts of {name} as CSV\u{2026}");
        self.codeql_doc_job = Some((
            rx,
            format!("Alerts of {name} (CSV)"),
            format!("Could not write the alerts of {name} as CSV"),
        ));
    }

    /// "View Results (CSV)" (#578): a run's raw results as tables. A table
    /// run's are its results already; an alert run's are decoded from the
    /// BQRS the database keeps for the query.
    pub(in crate::app) fn view_codeql_raw_results(&mut self) {
        let Some(entry) = self.codeql_chosen_run() else {
            return;
        };
        let name = entry.query_name();
        if entry.status != crate::codeql_query::RunStatus::Succeeded {
            self.status = format!("{name} has no results");
            return;
        }
        if entry.output.extension().is_some_and(|e| e == "csv") {
            self.status = match self.editor.open(&entry.output) {
                Ok(()) => format!("Results of {name} (CSV)"),
                Err(e) => format!("{}: {e}", entry.output.display()),
            };
            return;
        }
        let Some(db) = Self::codeql_run_database(&entry) else {
            self.status = format!("The database {name} ran on is gone", name = entry.database);
            return;
        };
        let Some(bqrs) = crate::codeql_query::kept_bqrs(&db, &entry.query) else {
            self.status = format!("The database no longer keeps the results of {name}");
            return;
        };
        if self.codeql_doc_job.is_some() {
            self.status = String::from("A CodeQL view is already being made");
            return;
        }
        let out = entry.output.with_file_name("raw-results.csv");
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let made = Self::codeql_decode_sets(&program, &bqrs, &out, &|args| {
                Self::codeql_command(&program, &args)
            })
            .map(|()| out);
            let _ = tx.send(made);
        });
        self.status = format!("Decoding the results of {name}\u{2026}");
        self.codeql_doc_job = Some((
            rx,
            format!("Results of {name} (CSV)"),
            format!("Could not decode the results of {name}"),
        ));
    }
}
