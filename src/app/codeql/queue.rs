//! The run queue: several queries run one after another (all queries in the workspace,
//! those in selected files), one query on several databases and their checklist, and
//! cancelling what waits.

use super::*;

impl App {
    /// Run `queries` one after another on the current database, through
    /// the queue a pack run uses; `none` is what to say when there are none.
    pub(in crate::app) fn run_codeql_batch(&mut self, queries: Vec<PathBuf>, none: &str) {
        if self.codeql_run.is_some() {
            self.status = String::from("A CodeQL query is already running");
            return;
        }
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        if store.current.and_then(|i| store.databases.get(i)).is_none() {
            self.status = String::from("Add a CodeQL database and select it first");
            return;
        }
        if queries.is_empty() {
            self.status = none.to_string();
            return;
        }
        self.codeql_batch = (queries.len(), 0);
        self.codeql_run_queue = queries
            .into_iter()
            .map(|query| crate::codeql_query::QueuedRun {
                query,
                source: None,
                database: None,
            })
            .collect();
        self.start_next_queued_codeql();
    }

    /// CodeQL: Run Query on Multiple Databases (#578): check databases in
    /// a list, then run the open query on each of them in turn.
    pub(in crate::app) fn run_codeql_query_on_databases(&mut self) {
        let query = match self.editor.path.clone() {
            Some(p) if p.extension().is_some_and(|e| e == "ql") => p,
            _ => {
                self.status = String::from("Open a .ql query to run it");
                return;
            }
        };
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let checked: Vec<PathBuf> = store
            .current
            .and_then(|i| store.databases.get(i))
            .map(|db| db.path.clone())
            .into_iter()
            .collect();
        self.codeql_multi_db = Some((query, self.editor.lines.join("\n"), checked));
        self.open_codeql_multi_db_picker(0);
    }

    /// The checklist of databases for "Run Query on Multiple Databases",
    /// with row `selected` under the cursor: Enter on a database checks or
    /// unchecks it, Enter on the first row runs.
    pub(in crate::app) fn open_codeql_multi_db_picker(&mut self, selected: usize) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let Some((query, _, checked)) = self.codeql_multi_db.as_ref() else {
            return;
        };
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        if store.databases.is_empty() {
            self.codeql_multi_db = None;
            self.status = String::from("Add a CodeQL database first");
            return;
        }
        let name = query
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut rows = vec![ListRow {
            id: String::from("run"),
            label: match checked.len() {
                1 => String::from("\u{25b6} Run on the checked database"),
                n => format!("\u{25b6} Run on the {n} checked databases"),
            },
        }];
        rows.extend(store.databases.iter().enumerate().map(|(i, db)| ListRow {
            id: i.to_string(),
            label: format!(
                "{} {}{}",
                if checked.contains(&db.path) { "[x]" } else { "[ ]" },
                db.name,
                db.language.as_deref().map(|l| format!(" ({l})")).unwrap_or_default()
            ),
        }));
        let mut picker = ListPicker::new(
            ListPurpose::CodeqlMultiDb,
            format!("Run {name} on databases"),
            rows,
        );
        picker.selected = selected.min(picker.rows.len() - 1);
        self.open_list_picker(picker, "");
    }

    /// A row picked in the databases checklist (#578): toggle a database,
    /// or queue the query on every checked one.
    pub(in crate::app) fn choose_codeql_multi_db(&mut self, id: &str, selected: usize) {
        let Some((_, _, checked)) = self.codeql_multi_db.as_mut() else {
            return;
        };
        if id != "run" {
            let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
            if let Some(db) = id
                .parse::<usize>()
                .ok()
                .and_then(|i| store.databases.get(i))
            {
                match checked.iter().position(|p| *p == db.path) {
                    Some(at) => {
                        checked.remove(at);
                    }
                    None => checked.push(db.path.clone()),
                }
            }
            self.open_codeql_multi_db_picker(selected);
            return;
        }
        if checked.is_empty() {
            self.status = String::from("Check at least one database to run on");
            self.open_codeql_multi_db_picker(selected);
            return;
        }
        let Some((query, source, checked)) = self.codeql_multi_db.take() else {
            return;
        };
        if self.codeql_run.is_some() {
            self.status = String::from("A CodeQL query is already running");
            return;
        }
        // In the list's order, not the order they were checked in.
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let databases: Vec<PathBuf> = store
            .databases
            .iter()
            .map(|db| db.path.clone())
            .filter(|p| checked.contains(p))
            .collect();
        self.codeql_batch = (databases.len(), 0);
        self.codeql_run_queue = databases
            .into_iter()
            .map(|database| crate::codeql_query::QueuedRun {
                query: query.clone(),
                source: Some(source.clone()),
                database: Some(database),
            })
            .collect();
        self.start_next_queued_codeql();
    }

    /// CodeQL: Run All Queries in Workspace (#578): every query the side
    /// bar's Queries section lists, pack after pack.
    pub(in crate::app) fn run_all_codeql_queries(&mut self) {
        self.refresh_codeql_queries();
        let queries: Vec<PathBuf> = self
            .codeql
            .queries
            .iter()
            .flat_map(|p| p.queries.iter().cloned())
            .collect();
        self.run_codeql_batch(queries, "No CodeQL queries in the workspace");
    }

    /// CodeQL: Run Queries in Selected Files (#578): the `.ql` files
    /// selected in the Explorer, and every query under a selected folder.
    pub(in crate::app) fn run_selected_codeql_queries(&mut self) {
        let mut paths: Vec<PathBuf> = self.tree.marked.iter().cloned().collect();
        if paths.is_empty()
            && let Some(p) = self.tree.selected_path()
        {
            paths.push(p.to_path_buf());
        }
        let queries = crate::codeql_query::queries_in(&paths);
        self.run_codeql_batch(
            queries,
            "Select .ql files, or folders holding them, in the Explorer first",
        );
    }

    /// Start the pack run's next query, read from disk as the side-bar rows
    /// are; one that cannot be read counts as failed and the next is tried.
    /// With the queue empty, the pack run is over and the status line sums
    /// it up.
    pub(in crate::app) fn start_next_queued_codeql(&mut self) {
        let (total, _) = self.codeql_batch;
        while self.codeql_run.is_none() {
            let Some(next) = self.codeql_run_queue.pop_front() else {
                let failed = self.codeql_batch.1;
                self.codeql_batch = (0, 0);
                let queries = if total == 1 { "query" } else { "queries" };
                self.status = match failed {
                    0 => format!("Ran {total} CodeQL {queries}"),
                    n => format!("Ran {total} CodeQL {queries}, {n} failed"),
                };
                return;
            };
            let n = total - self.codeql_run_queue.len();
            let path = next.query;
            let source = match next.source {
                Some(source) => source,
                None => match std::fs::read_to_string(&path) {
                    Ok(source) => source,
                    Err(e) => {
                        self.codeql_batch.1 += 1;
                        self.status = format!("{}: {e}", path.display());
                        continue;
                    }
                },
            };
            if let Some(database) = next.database {
                self.run_codeql_file_on(path, &source, CodeqlRunMode::Whole, Some(&database));
                if self.codeql_run.is_none() {
                    // That database went away meanwhile: the others may
                    // still be there.
                    self.codeql_batch.1 += 1;
                    continue;
                }
            } else {
                self.run_codeql_file(path, &source);
            }
            if self.codeql_run.is_none() {
                // Refused (the database went away meanwhile): the rest
                // would be refused too.
                self.codeql_run_queue.clear();
                self.codeql_batch = (0, 0);
                return;
            }
            if let Some(rest) = self.status.strip_prefix("Running ") {
                self.status = format!("Running CodeQL queries {n}/{total}: {rest}");
            }
        }
    }

    /// Drop the pack run's queries still waiting (#578); the one running
    /// finishes and is recorded as usual.
    pub(in crate::app) fn cancel_codeql_queue(&mut self) {
        let left = self.codeql_run_queue.len();
        self.codeql_run_queue.clear();
        self.codeql_batch = (0, 0);
        self.status = match left {
            0 => String::from("No CodeQL queries are queued"),
            1 => String::from("Cancelled 1 queued CodeQL query"),
            n => format!("Cancelled {n} queued CodeQL queries"),
        };
    }
}
