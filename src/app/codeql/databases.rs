//! The CodeQL database list: the current database, removing, renaming, sorting and
//! revealing databases, upgrading one to the CLI's schema, deleting unused copies, and
//! where databases and their sources are kept.

use super::*;

impl App {
    /// The store index of the current database, for the palette commands
    /// that act on it; says what to do when there is none.
    pub(in crate::app) fn current_codeql_database(&mut self) -> Option<usize> {
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let current = store.current.filter(|&i| i < store.databases.len());
        if current.is_none() {
            self.status = String::from("Select a CodeQL database first");
        }
        current
    }

    /// Ask before removing database `index` (#578), saying whether its
    /// files go too: only a copy croft made in its own cache is deleted.
    pub(in crate::app) fn confirm_remove_codeql_database(&mut self, index: usize) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.databases.get(index) else {
            return;
        };
        let what = if Self::codeql_cached_copy(&db.path).is_some() {
            "Its downloaded files are deleted."
        } else {
            "Its folder stays on disk."
        };
        self.open_input_prompt(InputPrompt::confirm(
            InputPurpose::CodeqlRemoveDatabase {
                path: db.path.clone(),
            },
            format!("Remove CodeQL database '{}'?  {what}", db.name),
            "Enter or Y to remove · Esc or N to keep",
        ));
    }

    /// The folder to delete with the database at `path`: the entry under
    /// croft's database cache that holds it (an extracted archive keeps the
    /// database one level down). `None` for a database the user pointed at
    /// elsewhere, which croft only forgets. Both sides are canonicalised so
    /// a `..` in a hand-edited list cannot walk out of the cache.
    pub(in crate::app) fn codeql_cached_copy(path: &Path) -> Option<PathBuf> {
        Self::cache_entry_holding(&Self::codeql_db_cache_dir(), path)
    }

    /// The entry directly under `cache` that holds `path`, both
    /// canonicalised; `None` when `path` is elsewhere, is `cache` itself, or
    /// either does not exist.
    pub(in crate::app) fn cache_entry_holding(cache: &Path, path: &Path) -> Option<PathBuf> {
        let cache = cache.canonicalize().ok()?;
        let path = path.canonicalize().ok()?;
        let first = path.strip_prefix(&cache).ok()?.components().next()?;
        Some(cache.join(first))
    }

    /// Remove the database at `path` from the list, deleting its files when
    /// croft made them, and save.
    pub(in crate::app) fn perform_remove_codeql_database(&mut self, path: &Path) {
        let store_path = Self::codeql_db_store_path();
        let mut store = crate::codeql_db::DatabaseStore::load(&store_path);
        let Some(i) = store.position(path) else {
            self.status = String::from("That CodeQL database is no longer listed");
            return;
        };
        let name = store.databases[i].name.clone();
        let cached = Self::codeql_cached_copy(path);
        store.remove(i);
        if let Err(e) = store.save(&store_path) {
            self.status = format!("Could not save the CodeQL database list: {e}");
            return;
        }
        self.status = match cached.map(std::fs::remove_dir_all) {
            Some(Err(e)) => format!("Removed CodeQL database {name}, but not its files: {e}"),
            _ => format!("Removed CodeQL database {name}"),
        };
        self.refresh_codeql_databases();
        // Stay on the database card while there is one, never on the
        // welcome text below it.
        if !self.codeql.databases.is_empty() {
            self.codeql.select_database_card();
        }
        if self.codeql.selected_hit().is_none() {
            self.codeql.move_selection(false);
        }
    }

    pub(in crate::app) fn prompt_rename_codeql_database(&mut self, index: usize) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.databases.get(index) else {
            return;
        };
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlRenameDatabase {
                    path: db.path.clone(),
                },
                String::from("Rename CodeQL Database"),
                "the name shown in the side bar",
            )
            .with_value(db.name.clone()),
        );
    }

    pub(in crate::app) fn submit_rename_codeql_database(&mut self, path: &Path, value: &str) {
        let store_path = Self::codeql_db_store_path();
        let mut store = crate::codeql_db::DatabaseStore::load(&store_path);
        let Some(i) = store.position(path) else {
            self.status = String::from("That CodeQL database is no longer listed");
            return;
        };
        let old = store.databases[i].name.clone();
        if let Err(e) = store.rename(i, value) {
            self.status = e;
            return;
        }
        // History from before runs recorded their database's path names it
        // only by its old name. The rename keeps that name among its former
        // names, so such runs still count for it; with no other database
        // ever called that, they are also pinned to its path. Otherwise the
        // runs could be either's, and both stay protected by name.
        let same_name = store
            .databases
            .iter()
            .enumerate()
            .filter(|&(j, d)| j != i && d.has_had_name(&old))
            .count();
        if same_name == 0 {
            let history_path = Self::codeql_history_path();
            let mut history = crate::codeql_query::History::load(&history_path);
            if history.adopt_legacy(&old, path)
                && let Err(e) = history.save(&history_path)
            {
                self.status = format!("Could not save the CodeQL query history: {e}");
                return;
            }
        }
        // Renaming can move it in a name-sorted list.
        if let Some(by) = store.sort_by {
            store.sort(by);
        }
        self.status = match store.save(&store_path) {
            Ok(()) => format!("Renamed CodeQL database to {}", value.trim()),
            Err(e) => format!("Could not save the CodeQL database list: {e}"),
        };
        self.refresh_codeql_databases();
        if store.position(path).is_some() {
            self.codeql.select_database_card();
        }
    }

    /// Step the databases to the next sort order (name, language, date
    /// added), keeping the selection on the database card.
    pub(in crate::app) fn sort_codeql_databases(&mut self) {
        let store_path = Self::codeql_db_store_path();
        let mut store = crate::codeql_db::DatabaseStore::load(&store_path);
        let on_card = self.codeql.selected_database().is_some();
        let by = store
            .sort_by
            .map_or(crate::codeql_db::DbSort::Name, |b| b.next());
        store.sort(by);
        self.status = match store.save(&store_path) {
            Ok(()) => format!("CodeQL databases sorted by {}", by.label()),
            Err(e) => format!("Could not save the CodeQL database list: {e}"),
        };
        self.refresh_codeql_databases();
        if on_card {
            self.codeql.select_database_card();
        }
    }

    /// Show database `index`'s folder in the Explorer (VS Code's "Show
    /// Database Directory").
    pub(in crate::app) fn reveal_codeql_database(&mut self, index: usize) {
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(path) = store.databases.get(index).map(|d| d.path.clone()) else {
            return;
        };
        self.reveal_in_explorer(path.clone());
        if !self.tree.nodes.iter().any(|n| n.path == path) {
            self.status = format!(
                "{} is outside the workspace; add its folder to the workspace to browse it",
                path.display()
            );
        }
    }

    /// Upgrade database `index` to the CLI's current schema (#578, VS
    /// Code's "CodeQL: Upgrade Database") on a worker thread;
    /// [`Self::drain_codeql_upgrade`] collects it. Refused while a query or
    /// another upgrade runs: either would read a database being rewritten.
    pub(in crate::app) fn upgrade_codeql_database(&mut self, index: usize) {
        self.start_codeql_db_job(index, crate::codeql_query::DbJob::Upgrade);
    }

    /// Run `job` on database `index` on a worker thread (#578): an upgrade,
    /// or VS Code's "CodeQL: Clear Cache", "Trim Cache" and "Trim Cache to
    /// Overlay Base". One at a time, never beside a query run, since each
    /// rewrites the database a query would read.
    pub(in crate::app) fn start_codeql_db_job(
        &mut self,
        index: usize,
        job: crate::codeql_query::DbJob,
    ) {
        if let Some((_, running, _, _)) = self.codeql_upgrade.as_ref() {
            self.status = format!("A CodeQL database {} is already running", running.noun());
            return;
        }
        if self.codeql_run.is_some() {
            self.status = format!(
                "Wait for the CodeQL query run to finish before {}",
                job.gerund()
            );
            return;
        }
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.databases.get(index).cloned() else {
            return;
        };
        let program = self.codeql_program.clone();
        let path = db.path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::codeql_command(&program, &job.args(&path)));
        });
        self.status = job.running(&db.name);
        self.codeql_upgrade = Some((rx, job, db.name, db.path));
    }

    /// Collect a finished database upgrade or cache cleanup (#578) onto the
    /// status line.
    pub fn drain_codeql_upgrade(&mut self) -> bool {
        let Some((rx, _, _, _)) = self.codeql_upgrade.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the job stopped unexpectedly"))
            }
        };
        let Some((_, job, name, _)) = self.codeql_upgrade.take() else {
            return false;
        };
        self.status = job.finished(&name, outcome);
        true
    }

    /// The databases "Delete Unused Databases" removes: copies in croft's
    /// cache that no query history entry ran on, never the current one or
    /// one being upgraded. A database the user pointed at elsewhere is
    /// theirs and never listed.
    pub(in crate::app) fn unused_codeql_databases(&self) -> Vec<PathBuf> {
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let upgrading = self.codeql_upgrade.as_ref().map(|(_, _, _, p)| p);
        store
            .databases
            .iter()
            .enumerate()
            .filter(|&(i, d)| store.current != Some(i) && Some(&d.path) != upgrading)
            .filter(|(_, d)| Self::codeql_cached_copy(&d.path).is_some())
            .filter(|(_, d)| {
                let mut names = vec![d.name.as_str()];
                names.extend(d.former_names.iter().map(String::as_str));
                !history.entries.iter().any(|e| e.refers_to(&d.path, &names))
            })
            .map(|(_, d)| d.path.clone())
            .collect()
    }

    /// Ask before deleting the unused databases (#578); says so when there
    /// are none.
    pub(in crate::app) fn confirm_delete_unused_codeql_databases(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let paths = self.unused_codeql_databases();
        let title = match paths.len() {
            0 => {
                self.status = String::from("There are no unused CodeQL databases to delete");
                return;
            }
            1 => String::from("Delete 1 unused CodeQL database?"),
            n => format!("Delete {n} unused CodeQL databases?"),
        };
        self.open_input_prompt(InputPrompt::confirm(
            InputPurpose::CodeqlDeleteUnusedDatabases { paths },
            title,
            "Enter or Y to delete · Esc or N to keep",
        ));
    }

    /// Remove the databases at `paths` from the list, delete their cached
    /// folders, and save. Each is checked again first: one that became
    /// current or was run on while the prompt was open is kept.
    pub(in crate::app) fn perform_delete_unused_codeql_databases(&mut self, paths: &[PathBuf]) {
        let unused = self.unused_codeql_databases();
        let store_path = Self::codeql_db_store_path();
        let mut store = crate::codeql_db::DatabaseStore::load(&store_path);
        let mut deleted = 0;
        let mut file_error = None;
        for path in paths.iter().filter(|p| unused.contains(p)) {
            let Some(i) = store.position(path) else {
                continue;
            };
            let cached = Self::codeql_cached_copy(path);
            store.remove(i);
            deleted += 1;
            if let Some(Err(e)) = cached.map(std::fs::remove_dir_all) {
                file_error.get_or_insert(e);
            }
        }
        if let Err(e) = store.save(&store_path) {
            self.status = format!("Could not save the CodeQL database list: {e}");
            return;
        }
        let what = match deleted {
            0 => String::from("There are no unused CodeQL databases to delete"),
            1 => String::from("Deleted 1 unused CodeQL database"),
            n => format!("Deleted {n} unused CodeQL databases"),
        };
        self.status = match file_error {
            Some(e) => format!("{what}, but not all their files: {e}"),
            None => what,
        };
        self.refresh_codeql_databases();
        if self.codeql.selected_hit().is_none() {
            self.codeql.move_selection(false);
        }
    }

    pub(in crate::app) fn codeql_db_store_path() -> PathBuf {
        croft_cache_dir().join("codeql-databases.json")
    }

    /// Where fetched or extracted databases live: croft's cache, never
    /// beside the archive the user pointed at.
    pub(in crate::app) fn codeql_db_cache_dir() -> PathBuf {
        croft_cache_dir().join("codeql").join("databases")
    }

    /// Where a database's `src.zip` is extracted to be browsed, one folder
    /// per database, beside the database cache.
    pub(in crate::app) fn codeql_source_cache_dir() -> PathBuf {
        croft_cache_dir().join("codeql").join("sources")
    }
}

impl App {
    /// Mirror the saved database list into the side bar.
    pub(in crate::app) fn refresh_codeql_databases(&mut self) {
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        // The card shows each folder's size: summed once per database on a
        // worker as the list loads or changes, never while painting or on
        // the key that opened the list. Known sizes stay until it reports.
        self.codeql
            .db_sizes
            .retain(|path, _| store.databases.iter().any(|db| &db.path == path));
        for db in &store.databases {
            if self.codeql.db_sizes.contains_key(&db.path)
                || !self.codeql_size_pending.insert(db.path.clone())
            {
                continue;
            }
            let tx = self.codeql_size_tx.clone();
            let path = db.path.clone();
            std::thread::spawn(move || {
                let size = crate::codeql_db::folder_size(&path);
                let _ = tx.send((path, size));
            });
        }
        self.codeql.databases = store.databases;
        self.codeql.current_db = store.current;
        self.codeql.db_sort = store.sort_by;
    }
}
