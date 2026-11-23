//! A run's logs: the evaluator log as raw JSON, as summary text and in the log viewer,
//! the query log, and comparing two runs' performance.

use super::*;

impl App {
    /// VS Code's "Show Evaluator Log (Raw JSON)" for history entry `index`.
    pub(in crate::app) fn show_codeql_evaluator_log(&mut self, index: usize) {
        let Some(log) = self.codeql_history_log(index) else {
            return;
        };
        match self.editor.open(&log) {
            Ok(()) => self.sync_open_file_poll_mtime(),
            Err(e) => self.status = format!("{}: {e}", log.display()),
        }
    }

    /// VS Code's "Show Query Log" for history entry `index`: the newest
    /// log `codeql` wrote into the run's `logs` folder. Runs croft made
    /// before it passed `--logdir` have none.
    pub(in crate::app) fn show_codeql_query_log(&mut self, index: usize) {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = history.entries.get(index) else {
            return;
        };
        if entry.status == crate::codeql_query::RunStatus::Running {
            self.status = format!("{} is still running", entry.query_name());
            return;
        }
        let logs = std::fs::read_dir(crate::codeql_query::query_log_dir(&entry.output))
            .map(|dir| {
                dir.flatten()
                    .filter_map(|e| {
                        let meta = e.metadata().ok().filter(|m| m.is_file())?;
                        Some((e.path(), meta.modified().ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some(log) = crate::codeql_query::newest_query_log(logs) else {
            self.status = format!("{} has no query log", entry.query_name());
            return;
        };
        match self.editor.open(&log) {
            Ok(()) => self.sync_open_file_poll_mtime(),
            Err(e) => self.status = format!("{}: {e}", log.display()),
        }
    }

    /// VS Code's "Show Evaluator Log (Summary Text)" for history entry
    /// `index`: `codeql generate log-summary` on a worker thread the first
    /// time, then the summary it wrote beside the log.
    pub(in crate::app) fn show_codeql_evaluator_log_summary(&mut self, index: usize) {
        let Some(log) = self.codeql_history_log(index) else {
            return;
        };
        let summary = crate::codeql_query::evaluator_log_summary(&log);
        if summary.is_file() {
            match self.editor.open(&summary) {
                Ok(()) => self.sync_open_file_poll_mtime(),
                Err(e) => self.status = format!("{}: {e}", summary.display()),
            }
            return;
        }
        if self.codeql_log_summary.is_some() {
            self.status = String::from("An evaluator log summary is already being made");
            return;
        }
        let program = self.codeql_program.clone();
        let args = crate::codeql_query::log_summary_args(&log, &summary);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::codeql_command(&program, &args));
        });
        self.status = String::from("Summarising the evaluator log\u{2026}");
        self.codeql_log_summary = Some((rx, summary));
    }

    /// Open a finished evaluator log summary (#578), or say why the CLI
    /// could not make it.
    pub fn drain_codeql_log_summary(&mut self) -> bool {
        let Some((rx, _)) = self.codeql_log_summary.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the summary stopped unexpectedly"))
            }
        };
        let Some((_, summary)) = self.codeql_log_summary.take() else {
            return false;
        };
        match outcome.and_then(|()| {
            self.editor
                .open(&summary)
                .map_err(|e| format!("{}: {e}", summary.display()))
        }) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.status = String::from("Opened the evaluator log summary");
            }
            Err(why) => self.status = format!("Could not summarise the evaluator log: {why}"),
        }
        true
    }

    /// The predicates the evaluator log `log` records, for a worker thread:
    /// `codeql generate log-summary --format=predicates` the first time
    /// (kept beside the log), then that summary parsed.
    pub(in crate::app) fn codeql_log_predicates(
        program: &Path,
        log: &Path,
    ) -> Result<Vec<crate::codeql_evallog::Predicate>, String> {
        let predicates = crate::codeql_query::evaluator_log_predicates(log);
        if !predicates.is_file() {
            let args = crate::codeql_query::log_predicates_args(log, &predicates);
            Self::codeql_command(program, &args)?;
        }
        let text = std::fs::read_to_string(&predicates)
            .map_err(|e| format!("{}: {e}", predicates.display()))?;
        Ok(crate::codeql_evallog::parse(&text))
    }

    /// VS Code's "Show Evaluator Log (Viewer)" for history entry `index`:
    /// on a worker thread, `codeql generate log-summary --format=predicates`
    /// the first time (kept beside the log), then that summary parsed and
    /// rendered as a tree of predicates, slowest first.
    pub(in crate::app) fn show_codeql_evaluator_log_viewer(&mut self, index: usize) {
        let Some(log) = self.codeql_history_log(index) else {
            return;
        };
        if self.codeql_log_viewer.is_some() {
            self.status = String::from("The evaluator log viewer is already being prepared");
            return;
        }
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let query = history
            .entries
            .get(index)
            .map(|e| e.query_name())
            .unwrap_or_default();
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::codeql_log_predicates(&program, &log));
        });
        self.status = String::from("Reading the evaluator log\u{2026}");
        self.codeql_log_viewer = Some((rx, query));
    }

    /// Show a prepared evaluator log tree (#578) in a tab with every
    /// predicate folded, or say why it could not be made.
    pub fn drain_codeql_log_viewer(&mut self) -> bool {
        let Some((rx, _)) = self.codeql_log_viewer.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the viewer stopped unexpectedly"))
            }
        };
        let Some((_, query)) = self.codeql_log_viewer.take() else {
            return false;
        };
        let label = format!("Evaluator Log ({query})");
        match outcome.and_then(|predicates| {
            let text = crate::codeql_evallog::render(&query, &predicates);
            // The side bar's Evaluator Log Viewer shows the same tree.
            self.codeql.evallog = Some(crate::codeql_evallog::LogView {
                query: query.clone(),
                predicates,
                ..Default::default()
            });
            self.codeql
                .collapsed
                .remove(&crate::widgets::codeql::Section::EvaluatorLog);
            self.editor
                .open_text_buffer(Path::new(&label), &text)
                .map_err(|e| e.to_string())
        }) {
            Ok(()) => {
                self.editor.fold_all();
                self.focus_pane(Pane::Editor);
                self.status = String::from("Opened the evaluator log viewer");
            }
            Err(why) => self.status = format!("Could not read the evaluator log: {why}"),
        }
        true
    }

    /// VS Code's "Compare Performance" for history entry `index` (#578):
    /// the predicate timings of its evaluator log against those of the
    /// latest earlier successful run of the same query, biggest change
    /// first. Predicate summaries either run lacks are made on a worker
    /// thread, as the evaluator log viewer makes them.
    pub(in crate::app) fn compare_codeql_performance(&mut self, index: usize) {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = history.entries.get(index) else {
            return;
        };
        let Some(new_log) = self.codeql_history_log(index) else {
            return;
        };
        let Some(partner) = history.perf_partner(index) else {
            self.status = format!(
                "There is no earlier successful run of {} to compare with",
                entry.query_name()
            );
            return;
        };
        let Some(old_log) = self.codeql_history_log(partner) else {
            return;
        };
        if self.codeql_perf_compare.is_some() {
            self.status = String::from("A performance comparison is already being prepared");
            return;
        }
        let query = entry.query_name();
        let old_label = format!("earlier ({})", history.entries[partner].database);
        let new_label = format!("later ({})", entry.database);
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let table = Self::codeql_log_predicates(&program, &old_log).and_then(|old| {
                let new = Self::codeql_log_predicates(&program, &new_log)?;
                let rows = crate::codeql_evallog::compare(&old, &new);
                Ok(crate::codeql_evallog::render_comparison(
                    &old_label, &new_label, &rows,
                ))
            });
            let _ = tx.send(table);
        });
        self.status = String::from("Comparing the evaluator logs\u{2026}");
        self.codeql_perf_compare = Some((rx, format!("Performance ({query})")));
    }

    /// Show a prepared performance comparison (#578) in a tab, or say why
    /// it could not be made.
    pub fn drain_codeql_perf_compare(&mut self) -> bool {
        let Some((rx, _)) = self.codeql_perf_compare.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the comparison stopped unexpectedly"))
            }
        };
        let Some((_, label)) = self.codeql_perf_compare.take() else {
            return false;
        };
        match outcome.and_then(|text| {
            self.editor
                .open_text_buffer(Path::new(&label), &text)
                .map_err(|e| e.to_string())
        }) {
            Ok(()) => {
                self.focus_pane(Pane::Editor);
                self.status = String::from("Opened the performance comparison");
            }
            Err(why) => self.status = format!("Could not compare performance: {why}"),
        }
        true
    }
}
