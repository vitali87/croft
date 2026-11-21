//! The CodeQL query history: where runs and their results are kept, listing, removing,
//! renaming and sorting entries, an entry's query and its text, its evaluator log, and
//! its results folder.

use super::*;

impl App {
    pub(in crate::app) fn codeql_history_path() -> PathBuf {
        croft_cache_dir().join("codeql-history.json")
    }

    /// Where query runs write their results, one folder per run.
    pub(in crate::app) fn codeql_results_dir() -> PathBuf {
        croft_cache_dir().join("codeql").join("results")
    }

    /// Mirror the saved query history into the side bar.
    pub(in crate::app) fn refresh_codeql_history(&mut self) {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        self.codeql.history = history
            .entries
            .iter()
            .map(crate::widgets::codeql::HistoryRow::new)
            .collect();
        self.codeql.history_sort = history.sort_by;
        // An expanded entry that went folds away with it.
        if let Some(out) = &self.codeql.expanded
            && !self.codeql.history.iter().any(|r| &r.output == out)
        {
            self.codeql.expanded = None;
        }
    }

    /// The query history entry the palette commands act on: the selected
    /// row, else the most recent run; says so when there is none.
    pub(in crate::app) fn current_codeql_history(&mut self) -> Option<usize> {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let current = self
            .codeql
            .selected_history()
            .filter(|&i| i < history.entries.len())
            .or_else(|| history.newest());
        if current.is_none() {
            self.status = String::from("There is no CodeQL query history yet");
        }
        current
    }

    /// The results folder of a history entry that croft may delete: its
    /// run's own folder under the results cache. `None` when the output
    /// points anywhere else (a hand-edited history), which croft only
    /// forgets.
    pub(in crate::app) fn codeql_cached_results(output: &Path) -> Option<PathBuf> {
        Self::cache_entry_holding(&Self::codeql_results_dir(), output.parent()?)
    }

    /// Ask before removing history entry `index` (#578), saying whether its
    /// results go too. A run still in flight is refused: its worker would
    /// write the results back.
    pub(in crate::app) fn confirm_remove_codeql_history(&mut self, index: usize) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = history.entries.get(index) else {
            return;
        };
        if entry.status == crate::codeql_query::RunStatus::Running {
            self.status = String::from("That query is still running");
            return;
        }
        let what = if Self::codeql_cached_results(&entry.output).is_some() {
            "Its results are deleted."
        } else {
            "Its results stay on disk."
        };
        self.open_input_prompt(InputPrompt::confirm(
            InputPurpose::CodeqlRemoveHistory {
                output: entry.output.clone(),
            },
            format!(
                "Remove query history entry '{}'?  {what}",
                entry.display_name()
            ),
            "Enter or Y to remove · Esc or N to keep",
        ));
    }

    /// Remove the history entry whose run wrote `output`, deleting its
    /// results folder when it is croft's own, and save.
    pub(in crate::app) fn perform_remove_codeql_history(&mut self, output: &Path) {
        let path = Self::codeql_history_path();
        let mut history = crate::codeql_query::History::load(&path);
        let Some(i) = history.position(output) else {
            self.status = String::from("That query history entry is no longer listed");
            return;
        };
        let name = history.entries[i].display_name();
        let cached = Self::codeql_cached_results(output);
        history.remove(i);
        if let Err(e) = history.save(&path) {
            self.status = format!("Could not save the CodeQL query history: {e}");
            return;
        }
        self.status = match cached.map(std::fs::remove_dir_all) {
            Some(Err(e)) => {
                format!("Removed {name} from the query history, but not its results: {e}")
            }
            _ => format!("Removed {name} from the query history"),
        };
        self.refresh_codeql_history();
        // Stay in the list: on the row that took its place, else the one
        // above, never on the welcome text.
        let n = self.codeql.history.len();
        if n > 0 {
            self.codeql.select_history(i.min(n - 1));
        }
        if self.codeql.selected_hit().is_none() {
            self.codeql.move_selection(false);
        }
    }

    pub(in crate::app) fn prompt_rename_codeql_history(&mut self, index: usize) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = history.entries.get(index) else {
            return;
        };
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlRenameHistory {
                    output: entry.output.clone(),
                },
                String::from("Rename Query History Entry"),
                "the label shown in the side bar",
            )
            .with_value(entry.display_name()),
        );
    }

    pub(in crate::app) fn submit_rename_codeql_history(&mut self, output: &Path, value: &str) {
        let path = Self::codeql_history_path();
        let mut history = crate::codeql_query::History::load(&path);
        let Some(i) = history.position(output) else {
            self.status = String::from("That query history entry is no longer listed");
            return;
        };
        if let Err(e) = history.rename(i, value) {
            self.status = e;
            return;
        }
        // Renaming can move it in a name-sorted list.
        history.sort(history.sort_by);
        self.status = match history.save(&path) {
            Ok(()) => format!("Renamed query history entry to {}", value.trim()),
            Err(e) => format!("Could not save the CodeQL query history: {e}"),
        };
        self.refresh_codeql_history();
        if let Some(i) = history.position(output) {
            self.codeql.select_history(i);
        }
    }

    /// Step the query history to the next sort order (date, name, status),
    /// keeping the selection on the same entry.
    pub(in crate::app) fn sort_codeql_history(&mut self) {
        let path = Self::codeql_history_path();
        let mut history = crate::codeql_query::History::load(&path);
        let selected = self
            .codeql
            .selected_history()
            .and_then(|i| history.entries.get(i))
            .map(|e| e.output.clone());
        let by = history.sort_by.next();
        history.sort(by);
        self.status = match history.save(&path) {
            Ok(()) => format!("CodeQL query history sorted by {}", by.label()),
            Err(e) => format!("Could not save the CodeQL query history: {e}"),
        };
        self.refresh_codeql_history();
        if let Some(i) = selected.and_then(|o| history.position(&o)) {
            self.codeql.select_history(i);
        }
    }

    /// Open the query file history entry `index` ran (VS Code's "View
    /// Query"): the file as it is now, which may have changed since.
    pub(in crate::app) fn view_codeql_history_query(&mut self, index: usize) {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(query) = history.entries.get(index).map(|e| e.query.clone()) else {
            return;
        };
        match self.editor.open(&query) {
            Ok(()) => self.sync_open_file_poll_mtime(),
            Err(e) => self.status = format!("{}: {e}", query.display()),
        }
    }

    /// "View Query Text" (#578): the query of history entry `index` as it
    /// was when it ran, which the file itself may no longer be.
    pub(in crate::app) fn view_codeql_history_query_text(&mut self, index: usize) {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(entry) = history.entries.get(index) else {
            return;
        };
        let text = crate::codeql_query::query_text_path(&entry.output, &entry.query);
        if !text.is_file() {
            self.status = format!(
                "{} ran before croft kept query text; v opens the query file",
                entry.query_name()
            );
            return;
        }
        match self.editor.open(&text) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.status = format!("{} as it was when it ran", entry.query_name());
            }
            Err(e) => self.status = format!("{}: {e}", text.display()),
        }
    }

    /// The evaluator log of history entry `index`, or why there is none: a
    /// run croft made before it kept logs, or one still going.
    pub(in crate::app) fn codeql_history_log(&mut self, index: usize) -> Option<PathBuf> {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let entry = history.entries.get(index)?;
        if entry.status == crate::codeql_query::RunStatus::Running {
            self.status = format!("{} is still running", entry.query_name());
            return None;
        }
        let log = crate::codeql_query::evaluator_log(&entry.output);
        if log.is_file() {
            return Some(log);
        }
        self.status = format!("{} has no evaluator log", entry.query_name());
        None
    }

    /// "CodeQL: Open Query Results Directory" (#578): the folder of the
    /// Query History run selected in the side bar, else of the newest run.
    pub(in crate::app) fn open_codeql_results_directory(&mut self) {
        let index = self
            .codeql
            .selected_history()
            .or_else(|| crate::codeql_query::History::load(&Self::codeql_history_path()).newest());
        match index {
            Some(i) => self.open_codeql_history_results_dir(i),
            None => self.status = String::from("No CodeQL query has run yet"),
        }
    }

    /// VS Code's "Open Results Directory" for history entry `index`. The
    /// folder lives in croft's cache, outside the workspace, so the
    /// Explorer can show it only when the user has it open; otherwise its
    /// results file opens and the status line names the folder.
    pub(in crate::app) fn open_codeql_history_results_dir(&mut self, index: usize) {
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let Some(output) = history.entries.get(index).map(|e| e.output.clone()) else {
            return;
        };
        let Some(dir) = output.parent().map(Path::to_path_buf) else {
            return;
        };
        if !dir.is_dir() {
            self.status = format!("{} does not exist (yet)", dir.display());
        } else if self.tree.nodes.iter().any(|n| n.path == dir) {
            self.reveal_in_explorer(dir);
        } else if output.is_file() {
            match self.editor.open(&output) {
                Ok(()) => {
                    self.sync_open_file_poll_mtime();
                    self.status = format!("Results directory: {}", dir.display());
                }
                Err(e) => self.status = format!("{}: {e}", output.display()),
            }
        } else {
            self.status = format!("Results directory: {}", dir.display());
        }
    }
}
