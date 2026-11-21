//! The CodeQL side bar: databases, query history and logs, running queries, suites and
//! packs, result viewers, AST and CFG, the model editor, the run queue, variant analysis,
//! and downloading and updating the CLI.

use super::*;

mod cli_install;
mod database_sources;
mod databases;
mod history;
mod logs;
mod queries;
mod results;
mod test_runs;

impl App {
    /// Why CodeQL's side bar and commands are unavailable, or `None` when
    /// it is installed and enabled. It is not built in (#1842): it waits
    /// under AVAILABLE in the Extensions view until the user adds it.
    pub(super) fn codeql_unavailable(&self) -> Option<&'static str> {
        if !self.installed_extensions.contains("codeql") {
            Some("CodeQL is not installed - add it from the Extensions view")
        } else if self.disabled_extensions.contains("codeql") {
            Some("CodeQL is disabled - enable it in the Extensions view")
        } else {
            None
        }
    }

    /// Reveal the CodeQL view (#578): the QL icon and the palette. Refused,
    /// with the reason, while the CodeQL extension is not added or disabled.
    /// The saved lists it shows are small files read here; the workspace's
    /// queries take a walk of the tree, which runs off the UI thread so the
    /// view opens at once (#840).
    pub(super) fn open_codeql_view(&mut self) {
        if let Some(why) = self.codeql_unavailable() {
            self.status = format!("{why} to show its side bar");
            return;
        }
        self.show_tree = true;
        self.refresh_codeql_databases();
        self.refresh_codeql_history();
        self.discover_codeql_queries();
        self.refresh_codeql_variant();
        self.set_sidebar_view(SidebarView::CodeQL);
    }

    /// CodeQL view keys: arrows move between headers and actions, Enter or
    /// Space folds a section or runs an action, Esc returns to the Explorer.
    /// On the database card, Enter picks the current database, and Delete
    /// removes it, F2 renames it, `e` shows its folder in the Explorer, `u`
    /// upgrades it and `w` adds its source to the workspace; in the
    /// Databases section `r` runs the open query, `q` opens Quick Query and
    /// `a` views the AST, as the card's chips do. On a query history row,
    /// Space or Right expands its details and Left folds them, Delete
    /// removes it, F2 renames it, `v` opens its query, `t` the query's text
    /// as it ran, `o` its results directory, `c` compares its results, `x`
    /// exports them and `l` shows its query log. `s`
    /// steps the query history's sort order within that section and the
    /// databases' anywhere else. `n` creates a query, in the selected pack
    /// when there is one. `r` on a pack or one of its queries runs every
    /// query in the pack; Esc then first cancels the ones still queued, and
    /// once none are, the running query.
    /// In the Variant Analysis Repositories section `a` adds a repository
    /// (into the selected list), `l` a list, `o` an owner, `s` adds the
    /// repositories a GitHub Code Search finds to the selected list and `g`
    /// opens the selected repository or owner on GitHub; on a submitted run,
    /// Enter opens its report, `v` its results, `x` exports them, `c` copies
    /// its repository list and `l` opens its logs on GitHub Actions; Enter selects what a run
    /// targets, Space folds a list, F2 renames a list and Delete removes an
    /// entry, asking first for a list.
    pub(super) fn handle_codeql_key(&mut self, key: KeyEvent) {
        use crate::codeql_variant::Item;
        use crate::widgets::codeql::{Action, Hit, Section};
        let db = self.codeql.selected_database();
        let run = self.codeql.selected_history();
        let va_item = self.codeql.selected_variant_item();
        let in_va = self.codeql.selected_section() == Some(Section::VariantAnalysis);
        let in_db = self.codeql.selected_section() == Some(Section::Databases);
        match key.code {
            KeyCode::Esc if !self.codeql_run_queue.is_empty() => self.cancel_codeql_queue(),
            KeyCode::Esc if self.codeql_run.is_some() => self.cancel_codeql_run(),
            KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
            KeyCode::Up => self.codeql.move_selection(false),
            KeyCode::Down => self.codeql.move_selection(true),
            KeyCode::Char(' ') | KeyCode::Right if run.is_some() => {
                if let Some(i) = run {
                    self.codeql.toggle_history(i);
                }
            }
            KeyCode::Left if run.is_some_and(|i| self.codeql.is_expanded(i)) => {
                if let Some(i) = run {
                    self.codeql.toggle_history(i);
                }
            }
            KeyCode::Char(c @ ('r' | 'q' | 'a')) if in_db => {
                let chip = crate::widgets::codeql::DB_CHIPS
                    .iter()
                    .find(|chip| chip.key == Some(c));
                if let Some(chip) = chip {
                    self.activate_codeql(Hit::Action(chip.action));
                }
            }
            KeyCode::Char('c') if run.is_some() => {
                if let Some(i) = run {
                    self.compare_codeql_results(i);
                }
            }
            KeyCode::Char('x') if run.is_some() => self.prompt_export_codeql_results(),
            KeyCode::Char('l') if run.is_some() => {
                if let Some(i) = run {
                    self.show_codeql_query_log(i);
                }
            }
            KeyCode::Char(' ') if matches!(va_item, Some(Item::List(_))) => {
                if let Some(Item::List(i)) = va_item {
                    self.codeql.toggle_variant_list(i);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(hit) = self.codeql.selected_hit() {
                    self.activate_codeql(hit);
                }
            }
            KeyCode::Delete => {
                if let Some(i) = db {
                    self.confirm_remove_codeql_database(i);
                } else if let Some(i) = run {
                    self.confirm_remove_codeql_history(i);
                } else if let Some(item) = va_item {
                    self.remove_codeql_variant_item(item);
                }
            }
            KeyCode::F(2) => {
                if let Some(i) = db {
                    self.prompt_rename_codeql_database(i);
                } else if let Some(i) = run {
                    self.prompt_rename_codeql_history(i);
                } else if let Some(Item::List(i)) = va_item {
                    self.prompt_rename_codeql_variant_list(i);
                }
            }
            KeyCode::Char('m') if self.codeql.selected_model_endpoint().is_some() => {
                self.model_codeql_endpoint(self.codeql.selected_model_endpoint());
            }
            KeyCode::Char('c') if self.codeql.selected_variant_run().is_some() => {
                self.copy_codeql_variant_repo_list(self.codeql.selected_variant_run());
            }
            KeyCode::Char('l') if self.codeql.selected_variant_run().is_some() => {
                self.view_codeql_variant_logs(self.codeql.selected_variant_run());
            }
            KeyCode::Char('a') if in_va => self.prompt_add_codeql_variant_repo(),
            KeyCode::Char('l') if in_va => self.prompt_add_codeql_variant_list(),
            KeyCode::Char('o') if in_va => self.prompt_add_codeql_variant_owner(),
            KeyCode::Char('s') if in_va => self.prompt_codeql_variant_code_search(),
            KeyCode::Char('g') if in_va => self.open_codeql_variant_on_github(),
            KeyCode::Char('e') => {
                if let Some(i) = db {
                    self.reveal_codeql_database(i);
                }
            }
            KeyCode::Char('u') => {
                if let Some(i) = db {
                    self.upgrade_codeql_database(i);
                }
            }
            KeyCode::Char('w') => {
                if let Some(i) = db {
                    self.add_codeql_database_source(i);
                }
            }
            KeyCode::Char('x') if self.codeql.selected_variant_run().is_some() => {
                self.prompt_export_codeql_variant_results(self.codeql.selected_variant_run());
            }
            KeyCode::Char('v') if self.codeql.selected_variant_run().is_some() => {
                self.open_codeql_variant_results(self.codeql.selected_variant_run());
            }
            KeyCode::Char('v') => {
                if let Some(i) = run {
                    self.view_codeql_history_query(i);
                }
            }
            KeyCode::Char('o') => {
                if let Some(i) = run {
                    self.open_codeql_history_results_dir(i);
                }
            }
            KeyCode::Char('t') => {
                if let Some(i) = run {
                    self.view_codeql_history_query_text(i);
                }
            }
            KeyCode::Char('s')
                if run.is_some()
                    || self.codeql.selected_hit() == Some(Hit::Action(Action::SortHistory)) =>
            {
                self.sort_codeql_history();
            }
            KeyCode::Char('s') => self.sort_codeql_databases(),
            KeyCode::Char('n') => self.prompt_create_codeql_query(),
            KeyCode::Char('r') => {
                if let Some(p) = self.codeql.selected_pack() {
                    self.run_codeql_pack(p);
                }
            }
            _ => {}
        }
    }

    /// Run what a CodeQL side-bar row offers. Sections and query packs fold;
    /// the language and database lists select; a query row runs; "Create
    /// one" asks for a query name; a variant analysis entry becomes what a
    /// run targets (a list already selected folds instead); the actions
    /// still to come in #578 say so rather than do nothing.
    pub(super) fn activate_codeql(&mut self, hit: crate::widgets::codeql::Hit) {
        use crate::widgets::codeql::{Action, Hit, LANGUAGES};
        match hit {
            Hit::Header(section) => self.codeql.toggle(section),
            Hit::Action(Action::SelectLanguage(i)) => {
                self.codeql.language = if self.codeql.language == Some(i) {
                    None
                } else {
                    Some(i)
                };
                self.status = match self.codeql.language {
                    Some(i) => format!("CodeQL language: {}", LANGUAGES[i]),
                    None => String::from("CodeQL language: all"),
                };
            }
            Hit::Action(Action::SelectDatabase(i)) => {
                let path = Self::codeql_db_store_path();
                let mut store = crate::codeql_db::DatabaseStore::load(&path);
                if i < store.databases.len() {
                    store.current = Some(i);
                    let _ = store.save(&path);
                    self.status = format!("Current CodeQL database: {}", store.databases[i].name);
                }
                self.refresh_codeql_databases();
            }
            Hit::Action(Action::SortDatabases) => self.sort_codeql_databases(),
            Hit::Action(Action::SortHistory) => self.sort_codeql_history(),
            Hit::Action(
                action @ (Action::AddDatabaseFromFolder
                | Action::AddDatabaseFromArchive
                | Action::AddDatabaseFromUrl
                | Action::AddDatabaseFromGithub),
            ) => {
                use crate::widgets::input_prompt::{CodeqlDbSource, InputPrompt, InputPurpose};
                let (source, title, hint) = match action {
                    Action::AddDatabaseFromFolder => (
                        CodeqlDbSource::Folder,
                        "Add CodeQL Database from Folder",
                        "path to the database folder",
                    ),
                    Action::AddDatabaseFromArchive => (
                        CodeqlDbSource::Archive,
                        "Add CodeQL Database from Archive",
                        "path to a .zip",
                    ),
                    Action::AddDatabaseFromUrl => (
                        CodeqlDbSource::Url,
                        "Add CodeQL Database from URL",
                        "https://… .zip",
                    ),
                    _ => (
                        CodeqlDbSource::Github,
                        "Add CodeQL Database from GitHub",
                        "owner/repo [language]",
                    ),
                };
                self.open_input_prompt(InputPrompt::new(
                    InputPurpose::CodeqlDatabase { source },
                    String::from(title),
                    String::from(hint),
                ));
            }
            Hit::Action(Action::OpenHistory(i)) => {
                let history = crate::codeql_query::History::load(&Self::codeql_history_path());
                let Some(entry) = history.entries.get(i) else {
                    return;
                };
                match &entry.status {
                    crate::codeql_query::RunStatus::Succeeded => {
                        let output = entry.output.clone();
                        match self.editor.open(&output) {
                            Ok(()) => self.sync_open_file_poll_mtime(),
                            Err(e) => self.status = format!("{}: {e}", output.display()),
                        }
                    }
                    crate::codeql_query::RunStatus::Failed(why) => {
                        self.status = format!("That run failed: {why}");
                    }
                    crate::codeql_query::RunStatus::Running => {
                        self.status = String::from("That query is still running");
                    }
                    crate::codeql_query::RunStatus::Cancelled => {
                        self.status = String::from("That run was cancelled");
                    }
                }
            }
            Hit::Action(Action::TogglePack(i)) => self.codeql.toggle_pack(i),
            Hit::Action(Action::RunQuery(pack, query)) => {
                let Some(path) = self
                    .codeql
                    .queries
                    .get(pack)
                    .and_then(|p| p.queries.get(query))
                    .cloned()
                else {
                    return;
                };
                // A side-bar row runs the file as saved, as VS Code's
                // Queries view does.
                match std::fs::read_to_string(&path) {
                    Ok(source) => self.run_codeql_file(path, &source),
                    Err(e) => self.status = format!("{}: {e}", path.display()),
                }
            }
            Hit::Action(Action::AddDatabase) => self.open_codeql_database_sources(),
            Hit::Action(Action::PickDatabase) => self.open_codeql_database_picker(),
            Hit::Action(Action::RunOpenQuery) => {
                self.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery)
            }
            Hit::Action(Action::CompareHistory(i)) => self.compare_codeql_results(i),
            Hit::Action(Action::ExportHistory(i)) => {
                // The export prompt takes the selected entry.
                self.codeql.select_history(i);
                self.prompt_export_codeql_results();
            }
            Hit::Action(Action::HistoryLog(i)) => self.show_codeql_query_log(i),
            Hit::Action(Action::QuickQuery) => {
                self.run_command(crate::widgets::command_palette::Command::CodeqlQuickQuery)
            }
            Hit::Action(Action::ShowEvaluatorLog) => self.run_command(
                crate::widgets::command_palette::Command::CodeqlShowEvaluatorLogViewer,
            ),
            Hit::Action(Action::CreateQuery) => self.prompt_create_codeql_query(),
            Hit::Action(Action::SetUpControllerRepository) => self.prompt_codeql_controller(),
            Hit::Action(Action::VariantList(i))
                if self
                    .codeql
                    .variant
                    .is_selected(crate::codeql_variant::Item::List(i)) =>
            {
                self.codeql.toggle_variant_list(i);
            }
            Hit::Action(Action::VariantList(i)) => {
                self.select_codeql_variant_item(crate::codeql_variant::Item::List(i));
            }
            Hit::Action(Action::VariantRepo(list, j)) => {
                self.select_codeql_variant_item(crate::codeql_variant::Item::Repo(list, j));
            }
            Hit::Action(Action::VariantOwner(i)) => {
                self.select_codeql_variant_item(crate::codeql_variant::Item::Owner(i));
            }
            Hit::Action(Action::AddVariantRepo) => self.prompt_add_codeql_variant_repo(),
            Hit::Action(Action::AddVariantList) => self.prompt_add_codeql_variant_list(),
            Hit::Action(Action::AddVariantOwner) => self.prompt_add_codeql_variant_owner(),
            Hit::Action(Action::OpenVariantConfig) => self.open_codeql_variant_config(),
            Hit::Action(Action::VariantRun(i)) => self.open_codeql_variant_run(i),
            Hit::Action(Action::ViewAst) => self.view_codeql_ast(),
            Hit::Action(Action::OpenModelEditor) => self.open_codeql_model_editor(),
            Hit::Action(Action::ModelGroup(i)) => {
                if let Some(view) = self.codeql.model.as_mut()
                    && let Some(group) = view.endpoints.get(i).map(|e| e.group())
                {
                    view.toggle(&group);
                }
            }
            Hit::Action(Action::ModelEndpoint(i)) => {
                let Some(e) = self
                    .codeql
                    .model
                    .as_ref()
                    .and_then(|v| v.endpoints.get(i))
                    .cloned()
                else {
                    return;
                };
                let modeled = if e.supported {
                    "modeled by CodeQL"
                } else {
                    "not modeled"
                };
                let Some(loc) = e.location.clone() else {
                    self.status = format!("{}: {modeled}; no location", e.label());
                    return;
                };
                let Some(path) = self.codeql_source_file(&loc.path) else {
                    self.status = format!("{} is not on this machine", loc.path.display());
                    return;
                };
                let (line, column) = (loc.line.max(1) - 1, loc.column.max(1) - 1);
                self.status = match self.open_at(&path, line as usize, column as usize) {
                    Ok(()) => format!("{} ({}): {modeled}", e.label(), e.kind),
                    Err(err) => format!("{}: {err}", path.display()),
                };
            }
            Hit::Action(Action::ClearEvalLog) => {
                self.codeql.evallog = None;
                self.status = String::from("Cleared the evaluator log");
            }
            Hit::Action(Action::EvalPredicate(i)) => {
                if let Some(view) = self.codeql.evallog.as_mut() {
                    view.toggle(i);
                }
            }
            Hit::Action(Action::EvalDependency(target)) => match target {
                Some(i) => {
                    if let Some(view) = self.codeql.evallog.as_mut() {
                        view.open.insert(i);
                    }
                    self.codeql.select_evallog_predicate(i);
                }
                None => {
                    self.status =
                        String::from("That predicate is not in this log (it was cached or inlined)")
                }
            },
            Hit::Action(Action::ClearAst) => {
                self.codeql.ast = None;
                self.status = String::from("Cleared the AST");
            }
            Hit::Action(Action::AstNode(i)) => {
                let Some(view) = self.codeql.ast.as_mut() else {
                    return;
                };
                view.toggle(i);
                match view.target(i) {
                    Some((path, line, column)) if path.is_file() => {
                        let (line, column) = (line.max(1) - 1, column.max(1) - 1);
                        if let Err(e) = self.open_at(&path, line as usize, column as usize) {
                            self.status = format!("{}: {e}", path.display());
                        }
                    }
                    Some((path, ..)) => {
                        self.status = format!("{} is not on this machine", path.display());
                    }
                    None => {}
                }
            }
        }
    }

    pub(super) fn codeql_variant_path() -> PathBuf {
        croft_cache_dir().join("codeql-variant-analysis.json")
    }

    /// Reload the variant analysis config into the side bar. One that
    /// cannot be read is reported, and the section says so rather than
    /// showing it empty.
    pub(super) fn refresh_codeql_variant(&mut self) {
        self.refresh_codeql_variant_runs();
        match crate::codeql_variant::VariantConfig::load(&Self::codeql_variant_path()) {
            Ok(config) => {
                self.codeql.variant = config;
                self.codeql.variant_error = false;
            }
            Err(e) => {
                self.codeql.variant = crate::codeql_variant::VariantConfig::default();
                self.codeql.variant_error = true;
                self.status = format!("Could not read the variant analysis config: {e}");
            }
        }
    }

    /// Change the variant analysis config with `edit` and save it, showing
    /// `edit`'s message. A config that cannot be read is left as it is,
    /// never saved over with an empty one. Returns whether it saved.
    pub(super) fn edit_codeql_variant(
        &mut self,
        edit: impl FnOnce(&mut crate::codeql_variant::VariantConfig) -> Result<String, String>,
    ) -> bool {
        let path = Self::codeql_variant_path();
        let mut config = match crate::codeql_variant::VariantConfig::load(&path) {
            Ok(config) => config,
            Err(e) => {
                self.codeql.variant_error = true;
                self.status =
                    format!("Could not read the variant analysis config, so it is unchanged: {e}");
                return false;
            }
        };
        let message = match edit(&mut config) {
            Ok(m) => m,
            Err(e) => {
                self.status = e;
                return false;
            }
        };
        if let Err(e) = config.save(&path) {
            self.status = format!("Could not save the variant analysis config: {e}");
            return false;
        }
        self.status = message;
        self.codeql.variant = config;
        self.codeql.variant_error = false;
        true
    }

    /// Ask for the controller repository variant analysis runs from (#578,
    /// VS Code's "Set up controller repository"), starting from the current
    /// one.
    pub(super) fn prompt_codeql_controller(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        // From the file: the palette can ask before the side bar loaded it.
        let current = crate::codeql_variant::VariantConfig::load(&Self::codeql_variant_path())
            .ok()
            .and_then(|c| c.controller_repo);
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlControllerRepository,
                String::from("Set Up Controller Repository"),
                "owner/repo or its GitHub URL",
            )
            .with_value(current.unwrap_or_default()),
        );
    }

    pub(super) fn submit_codeql_controller(&mut self, value: &str) {
        let saved = self.edit_codeql_variant(|c| {
            c.set_controller(value)
                .map(|nwo| format!("Variant analysis controller repository: {nwo}"))
        });
        if saved {
            self.codeql
                .select_action(crate::widgets::codeql::Action::SetUpControllerRepository);
        }
    }

    /// Ask for a repository to add for variant analysis, into the list whose
    /// line (or one of whose repositories) is selected in the side bar.
    pub(super) fn prompt_add_codeql_variant_repo(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let list = self
            .codeql
            .selected_variant_list()
            .and_then(|i| self.codeql.variant.lists.get(i))
            .map(|l| l.name.clone());
        let title = match &list {
            Some(name) => format!("Add Repository to {name}"),
            None => String::from("Add Variant Analysis Repository"),
        };
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlAddVariantRepo { list },
            title,
            "owner/repo or its GitHub URL",
        ));
    }

    pub(super) fn submit_add_codeql_variant_repo(&mut self, list: Option<&str>, value: &str) {
        use crate::codeql_variant::Item;
        let mut added = None;
        let saved = self.edit_codeql_variant(|c| {
            let index = match list {
                Some(name) => Some(
                    c.list_index(name)
                        .ok_or_else(|| format!("The list {name} is no longer there"))?,
                ),
                None => None,
            };
            let nwo = c.add_repo(index, value)?;
            let len = match index {
                Some(i) => c.lists[i].repos.len(),
                None => c.repos.len(),
            };
            added = Some(Item::Repo(index, len - 1));
            Ok(match list {
                Some(name) => format!("Added {nwo} to {name}"),
                None => format!("Added {nwo} for variant analysis"),
            })
        });
        if let (true, Some(item)) = (saved, added) {
            if let Some(name) = list {
                self.codeql.folded_lists.remove(name);
            }
            self.codeql.select_variant_item(item);
        }
    }

    /// Ask for a GitHub Code Search query whose repositories go into the
    /// selected list (#578, VS Code's "Add repositories with GitHub Code
    /// Search"). The side bar's language scopes the search.
    pub(super) fn prompt_codeql_variant_code_search(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        if self.codeql_code_search.is_some() {
            self.status = String::from("A GitHub Code Search is already running");
            return;
        }
        let Some(list) = self
            .codeql
            .selected_variant_list()
            .and_then(|i| self.codeql.variant.lists.get(i))
            .map(|l| l.name.clone())
        else {
            self.status =
                String::from("Select a repository list for the Code Search results (l adds one)");
            return;
        };
        let hint = match self.codeql_search_language() {
            Some(lang) if lang != "actions" => format!("a GitHub Code Search query ({lang} only)"),
            _ => String::from("a GitHub Code Search query"),
        };
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlVariantCodeSearch { list: list.clone() },
            format!("Add Repositories to {list} with GitHub Code Search"),
            &hint,
        ));
    }

    /// The CodeQL id of the side bar's language, when one is picked.
    pub(super) fn codeql_search_language(&self) -> Option<&'static str> {
        self.codeql
            .language
            .and_then(|i| crate::widgets::codeql::LANGUAGE_IDS.get(i).copied())
    }

    /// Run the Code Search off the UI thread; [`Self::drain_codeql_code_search`]
    /// adds what it finds to `list`.
    pub(super) fn start_codeql_variant_code_search(&mut self, list: String, value: &str) {
        if value.trim().is_empty() {
            self.status = String::from("A Code Search query cannot be empty");
            return;
        }
        let query = crate::codeql_variant::code_search_query(value, self.codeql_search_language());
        let gh = self.gh_program.to_string_lossy().into_owned();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let search = query.clone();
        std::thread::spawn(move || {
            use crate::codeql_variant::{CODE_SEARCH_LIMIT, CODE_SEARCH_PAGE, code_search_args};
            let mut out = String::new();
            let mut cut = None;
            // Page by page up to the API's last, stopping at a short page.
            // A later page's failure (Code Search allows ten requests a
            // minute) keeps what the earlier ones found and says so.
            for page in 1..=CODE_SEARCH_LIMIT / CODE_SEARCH_PAGE {
                let text = match crate::pr_review::run_gh(
                    &gh,
                    &code_search_args(&search, page),
                    &root,
                    crate::pr_review::GH_TIMEOUT,
                ) {
                    Ok(text) => text,
                    Err(why) if page == 1 => {
                        let _ = tx.send(Err(why));
                        return;
                    }
                    Err(why) => {
                        cut = Some(format!("page {page} failed: {why}"));
                        break;
                    }
                };
                out.push_str(&text);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                if text.lines().count() < CODE_SEARCH_PAGE {
                    break;
                }
            }
            let _ = tx.send(Ok((crate::codeql_variant::parse_code_search(&out), cut)));
        });
        self.status = format!("Searching GitHub code for {query}…");
        self.codeql_code_search = Some((list, rx));
    }

    /// Add a finished Code Search's repositories to its list. Returns true
    /// when one finished.
    pub fn drain_codeql_code_search(&mut self) -> bool {
        let Some((_, rx)) = self.codeql_code_search.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the search stopped without answering"))
            }
        };
        let Some((list, _)) = self.codeql_code_search.take() else {
            return false;
        };
        let (found, cut) = match outcome {
            Ok((found, _)) if found.is_empty() => {
                self.status = String::from("GitHub Code Search found no repositories");
                return true;
            }
            Ok(outcome) => outcome,
            Err(why) => {
                self.status = format!("GitHub Code Search failed: {why}");
                return true;
            }
        };
        let mut index = None;
        let saved = self.edit_codeql_variant(|c| {
            let i = c
                .list_index(&list)
                .ok_or_else(|| format!("The list {list} is no longer there"))?;
            let (added, had) = c.add_repos(i, &found)?;
            index = Some(i);
            let plural = |n: usize| if n == 1 { "repository" } else { "repositories" };
            let mut message = format!("Added {added} {} to {list}", plural(added));
            if had > 0 {
                message.push_str(&format!(" ({had} already there)"));
            }
            if let Some(why) = &cut {
                message.push_str(&format!("; the search stopped early, {why}"));
            }
            Ok(message)
        });
        if let (true, Some(i)) = (saved, index) {
            self.codeql.folded_lists.remove(&list);
            self.codeql
                .select_variant_item(crate::codeql_variant::Item::List(i));
        }
        true
    }

    pub(super) fn prompt_add_codeql_variant_list(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlAddVariantList,
            String::from("Add Repository List"),
            "the list's name",
        ));
    }

    pub(super) fn submit_add_codeql_variant_list(&mut self, value: &str) {
        let mut added = None;
        let saved = self.edit_codeql_variant(|c| {
            let i = c.add_list(value)?;
            added = Some(i);
            Ok(format!("Added repository list {}", c.lists[i].name))
        });
        if let (true, Some(i)) = (saved, added) {
            self.codeql
                .select_variant_item(crate::codeql_variant::Item::List(i));
        }
    }

    pub(super) fn prompt_add_codeql_variant_owner(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlAddVariantOwner,
            String::from("Add Variant Analysis Owner"),
            "a user or organisation, or its GitHub URL",
        ));
    }

    /// VS Code's "CodeQL: Install Pack Dependencies" for pack `pack` of the
    /// Queries section (#578): `codeql pack install` on its folder.
    pub(super) fn install_codeql_pack_dependencies(&mut self, pack: usize) {
        let Some(p) = self.codeql.queries.get(pack) else {
            return;
        };
        if p.name == crate::codeql_query::NO_PACK {
            self.status =
                String::from("These queries are in no pack, so they have no dependencies");
            return;
        }
        let (name, args) = (
            p.name.clone(),
            crate::codeql_query::pack_install_args(&p.dir),
        );
        self.start_codeql_pack_job(
            args,
            format!("Installing the dependencies of {name}\u{2026}"),
            format!("Installed the dependencies of {name}"),
            format!("Could not install the dependencies of {name}"),
        );
    }

    /// Ask which packs "CodeQL: Download Packs" fetches (#578).
    pub(super) fn prompt_download_codeql_packs(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlDownloadPacks,
            String::from("Download Packs"),
            "packs to download, e.g. codeql/java-queries codeql/python-all@1.0.0",
        ));
    }

    pub(super) fn submit_download_codeql_packs(&mut self, value: &str) {
        let packs = crate::codeql_query::parse_pack_list(value);
        if packs.is_empty() {
            self.status = String::from("Name at least one pack to download");
            return;
        }
        let what = packs.join(", ");
        self.start_codeql_pack_job(
            crate::codeql_query::pack_download_args(&packs),
            format!("Downloading {what}\u{2026}"),
            format!("Downloaded {what}"),
            format!("Could not download {what}"),
        );
    }

    /// Run a `codeql pack` command on a worker thread (#578); one at a
    /// time, since installs and downloads write the same package cache.
    pub(super) fn start_codeql_pack_job(
        &mut self,
        args: Vec<String>,
        running: String,
        done: String,
        failed: String,
    ) {
        if self.codeql_pack_job.is_some() {
            self.status = String::from("A CodeQL pack install or download is already running");
            return;
        }
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::codeql_command(&program, &args));
        });
        self.status = running;
        self.codeql_pack_job = Some((rx, done, failed));
    }

    /// Collect a finished pack install or download (#578) onto the status
    /// line.
    pub fn drain_codeql_pack_job(&mut self) -> bool {
        let Some((rx, _, _)) = self.codeql_pack_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the pack command stopped unexpectedly"))
            }
        };
        let Some((_, done, failed)) = self.codeql_pack_job.take() else {
            return false;
        };
        self.status = match outcome {
            Ok(()) => done,
            Err(why) => format!("{failed}: {why}"),
        };
        true
    }

    pub(super) fn submit_add_codeql_variant_owner(&mut self, value: &str) {
        let saved = self.edit_codeql_variant(|c| {
            c.add_owner(value)
                .map(|o| format!("Added every repository of {o} for variant analysis"))
        });
        if saved {
            let last = self.codeql.variant.owners.len().saturating_sub(1);
            self.codeql
                .select_variant_item(crate::codeql_variant::Item::Owner(last));
        }
    }

    pub(super) fn prompt_rename_codeql_variant_list(&mut self, index: usize) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some(name) = self.codeql.variant.lists.get(index).map(|l| l.name.clone()) else {
            return;
        };
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlRenameVariantList { name: name.clone() },
                String::from("Rename Repository List"),
                "the list's name",
            )
            .with_value(name),
        );
    }

    pub(super) fn submit_rename_codeql_variant_list(&mut self, name: &str, value: &str) {
        let mut renamed = None;
        let saved = self.edit_codeql_variant(|c| {
            let i = c
                .list_index(name)
                .ok_or_else(|| format!("The list {name} is no longer there"))?;
            c.rename_list(i, value)?;
            renamed = Some(i);
            Ok(format!("Renamed repository list to {}", c.lists[i].name))
        });
        if let (true, Some(i)) = (saved, renamed) {
            // A fold is kept by name; it follows the list.
            if self.codeql.folded_lists.remove(name) {
                self.codeql.folded_lists.insert(value.trim().to_string());
            }
            self.codeql
                .select_variant_item(crate::codeql_variant::Item::List(i));
        }
    }

    /// Remove a variant analysis entry: a repository or owner at once, a
    /// list (with its repositories) after asking.
    pub(super) fn remove_codeql_variant_item(&mut self, item: crate::codeql_variant::Item) {
        use crate::codeql_variant::Item;
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some(name) = self.codeql.variant.name_of(item).map(str::to_string) else {
            return;
        };
        if let Item::List(i) = item {
            let n = self.codeql.variant.lists[i].repos.len();
            let what = match n {
                0 => String::new(),
                1 => String::from(" and its 1 repository"),
                n => format!(" and its {n} repositories"),
            };
            self.open_input_prompt(InputPrompt::confirm(
                InputPurpose::CodeqlRemoveVariantList { name: name.clone() },
                format!("Remove repository list '{name}'{what}?"),
                "Enter or Y to remove · Esc or N to keep",
            ));
            return;
        }
        let saved = self.edit_codeql_variant(|c| {
            // The side bar names the entry by position; make sure the file
            // still has it there before removing anything.
            if c.name_of(item) != Some(name.as_str()) {
                return Err(String::from(
                    "The variant analysis config changed on disk; look again",
                ));
            }
            c.remove(item)?;
            Ok(format!("Removed {name}"))
        });
        if saved {
            self.keep_codeql_selection_on_a_row();
        }
    }

    pub(super) fn perform_remove_codeql_variant_list(&mut self, name: &str) {
        let saved = self.edit_codeql_variant(|c| {
            let i = c
                .list_index(name)
                .ok_or_else(|| format!("The list {name} is no longer there"))?;
            c.remove(crate::codeql_variant::Item::List(i))?;
            Ok(format!("Removed repository list {name}"))
        });
        if saved {
            self.codeql.folded_lists.remove(name);
            self.keep_codeql_selection_on_a_row();
        }
    }

    /// After a row went away, keep the selection on a row rather than on
    /// the welcome text that moved up under it.
    pub(super) fn keep_codeql_selection_on_a_row(&mut self) {
        let last = self.codeql.lines().len().saturating_sub(1);
        self.codeql.selected = self.codeql.selected.min(last);
        if self.codeql.selected_hit().is_none() {
            self.codeql.move_selection(false);
        }
    }

    /// Make `item` what a variant analysis runs against (VS Code's "Select").
    pub(super) fn select_codeql_variant_item(&mut self, item: crate::codeql_variant::Item) {
        let Some(name) = self.codeql.variant.name_of(item).map(str::to_string) else {
            return;
        };
        let saved = self.edit_codeql_variant(|c| {
            if c.name_of(item) != Some(name.as_str()) {
                return Err(String::from(
                    "The variant analysis config changed on disk; look again",
                ));
            }
            c.select(item)?;
            Ok(format!("Variant analysis runs against {name}"))
        });
        if saved {
            self.codeql.select_variant_item(item);
        }
    }

    /// Open the variant analysis config in an editor tab, writing an empty
    /// one first when there is none. One that cannot be parsed opens as it
    /// is, to be fixed.
    pub(super) fn open_codeql_variant_config(&mut self) {
        let path = Self::codeql_variant_path();
        if !path.exists()
            && let Err(e) = crate::codeql_variant::VariantConfig::default().save(&path)
        {
            self.status = format!("Could not create {}: {e}", path.display());
            return;
        }
        match self.editor.open(&path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
            }
            Err(e) => self.status = format!("{}: {e}", path.display()),
        }
    }

    /// The GitHub page of the variant analysis repository or owner selected
    /// in the side bar (the controller on its row), else of the one a run
    /// targets.
    pub(super) fn codeql_variant_github_url(&self) -> Option<String> {
        use crate::codeql_variant::{Item, Selection};
        use crate::widgets::codeql::{Action, Hit};
        let v = &self.codeql.variant;
        let name = match self.codeql.selected_variant_item() {
            Some(item @ (Item::Repo(..) | Item::Owner(_))) => v.name_of(item)?.to_string(),
            Some(Item::List(_)) => return None,
            None if self.codeql.selected_hit()
                == Some(Hit::Action(Action::SetUpControllerRepository)) =>
            {
                v.controller_repo.clone()?
            }
            None => match v.selected.as_ref()? {
                Selection::Repo { nwo, .. } => nwo.clone(),
                Selection::Owner { owner } => owner.clone(),
                Selection::List { .. } => return None,
            },
        };
        Some(format!("https://github.com/{name}"))
    }

    /// Open the selected variant analysis repository or owner on GitHub
    /// (VS Code's "Open on GitHub"). Where no browser can be reached the
    /// status line shows the address instead.
    pub(super) fn open_codeql_variant_on_github(&mut self) {
        let Some(url) = self.codeql_variant_github_url() else {
            self.status = String::from(
                "Select a variant analysis repository or owner in the CodeQL side bar first",
            );
            return;
        };
        self.open_url_for_user(&url);
    }
}

impl App {
    /// Mirror the remembered variant analyses into the side bar.
    pub(super) fn refresh_codeql_variant_runs(&mut self) {
        self.codeql.variant_runs =
            crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path())
                .iter()
                .map(crate::codeql_submit::run_label)
                .collect();
    }

    /// Open the report of remembered variant analysis `index`: its status
    /// and a row per repository. It is written from the last progress read;
    /// an unfinished run is read again at once.
    pub(super) fn open_codeql_variant_run(&mut self, index: usize) {
        let runs = crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path());
        let Some(run) = runs.get(index) else {
            return;
        };
        if !run.progress.as_ref().is_some_and(|p| p.is_done()) {
            self.codeql_variant_polled = None;
        }
        let path = croft_cache_dir()
            .join("codeql")
            .join("variant-reports")
            .join(format!(
                "{}-{}.md",
                run.controller.replace('/', "-"),
                run.id
            ));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, crate::codeql_submit::run_report(run)));
        if let Err(e) = written {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        match self.editor.open(&path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
            }
            Err(e) => self.status = format!("{}: {e}", path.display()),
        }
    }

    /// Follow submitted variant analyses (#578): every
    /// [`CODEQL_VARIANT_POLL`], read the progress of those GitHub has not
    /// finished, off the UI thread, and record what comes back. A run
    /// finishing says so on the status line. Runs older than
    /// [`CODEQL_VARIANT_FOLLOW`] are left alone: GitHub keeps their results
    /// only so long, and one that never finishes would be read forever.
    /// Nothing is read until the side bar has loaded the runs (opening the
    /// CodeQL view, or a submission), so a session that never touches
    /// CodeQL never calls GitHub for it. Returns true when progress landed.
    pub fn poll_codeql_variant_runs(&mut self) -> bool {
        use crate::codeql_submit as cs;
        if let Some(rx) = self.codeql_variant_poll.as_ref() {
            let polled = match rx.try_recv() {
                Ok(p) => p,
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Vec::new(),
            };
            self.codeql_variant_poll = None;
            let path = Self::codeql_variant_runs_path();
            for (mut run, outcome) in polled {
                let Ok(progress) = outcome else { continue };
                let was_done = run.progress.as_ref().is_some_and(|p| p.is_done());
                if progress.is_done() && !was_done {
                    let (done, total) = progress.counts();
                    self.status = format!(
                        "Variant analysis {} of {} {}: {done}/{total} repositories, {} results",
                        run.id,
                        run.query.file_name().unwrap_or_default().to_string_lossy(),
                        progress.status,
                        progress.results()
                    );
                }
                run.progress = Some(progress);
                let _ = cs::update_submitted(&path, &run);
            }
            self.refresh_codeql_variant_runs();
            return true;
        }
        if self.codeql.variant_runs.is_empty()
            || !self.is_extension_enabled("codeql")
            || self
                .codeql_variant_polled
                .is_some_and(|t| t.elapsed() < CODEQL_VARIANT_POLL)
        {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let unfinished: Vec<cs::Submitted> = cs::load_submitted(&Self::codeql_variant_runs_path())
            .into_iter()
            .filter(|r| !r.progress.as_ref().is_some_and(|p| p.is_done()))
            .filter(|r| now.saturating_sub(r.submitted_at) < CODEQL_VARIANT_FOLLOW.as_secs())
            .collect();
        self.codeql_variant_polled = Some(std::time::Instant::now());
        if unfinished.is_empty() {
            return false;
        }
        let gh = self.gh_program.to_string_lossy().into_owned();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let polled = unfinished
                .into_iter()
                .map(|run| {
                    let outcome = crate::pr_review::run_gh(
                        &gh,
                        &cs::progress_args(&run.controller, run.id),
                        &root,
                        crate::pr_review::GH_TIMEOUT,
                    )
                    .and_then(|answer| cs::parse_progress(&answer));
                    (run, outcome)
                })
                .collect();
            let _ = tx.send(polled);
        });
        self.codeql_variant_poll = Some(rx);
        false
    }

    /// The submitted variant analysis run `index`, else the newest; says
    /// so when none has been submitted.
    pub(super) fn codeql_variant_run(
        &mut self,
        index: Option<usize>,
    ) -> Option<crate::codeql_submit::Submitted> {
        let runs = crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path());
        let run = index.map_or(runs.last(), |i| runs.get(i)).cloned();
        if run.is_none() {
            self.status = String::from("No variant analysis has been submitted yet");
        }
        run
    }

    /// Copy the repositories variant analysis run `index` (else the newest)
    /// covers, one per line (#578).
    pub(super) fn copy_codeql_variant_repo_list(&mut self, index: Option<usize>) {
        let Some(run) = self.codeql_variant_run(index) else {
            return;
        };
        self.status = match crate::codeql_submit::repository_list(&run) {
            Some(list) => {
                let landing = self.copy_where_pasted(&list);
                format!(
                    "Copied the {} repositories of variant analysis {}{}",
                    list.lines().count(),
                    run.id,
                    landing.note()
                )
            }
            None => format!(
                "Variant analysis {} has not reported its repositories yet",
                run.id
            ),
        };
    }

    /// Open the GitHub Actions run of variant analysis run `index` (else
    /// the newest), where its logs are (#578).
    pub(super) fn view_codeql_variant_logs(&mut self, index: Option<usize>) {
        let Some(run) = self.codeql_variant_run(index) else {
            return;
        };
        match crate::codeql_submit::workflow_run_url(&run) {
            Some(url) => self.open_url_for_user(&url),
            None => {
                self.status = format!(
                    "Variant analysis {} has no Actions run yet; GitHub starts one shortly",
                    run.id
                )
            }
        }
    }

    /// Fetch the results of remembered variant analysis `index` (the newest
    /// when `None`) and open them (#578): each repository that succeeded
    /// with results has its artifact downloaded (once; later opens use the
    /// copy), its alerts gathered into one SARIF log, one run per
    /// repository, and its tables into one CSV with a repository column.
    /// The fetching happens on a worker thread;
    /// [`Self::drain_codeql_variant_results`] opens what it made.
    pub(super) fn open_codeql_variant_results(&mut self, index: Option<usize>) {
        use crate::codeql_submit as cs;
        if self.codeql_variant_results.is_some() {
            self.status = String::from("Variant analysis results are already being fetched");
            return;
        }
        let runs = cs::load_submitted(&Self::codeql_variant_runs_path());
        let Some(run) = index.map_or(runs.last(), |i| runs.get(i)).cloned() else {
            self.status = String::from("No variant analysis has been submitted yet");
            return;
        };
        let repos = cs::repos_with_results(&run);
        if repos.is_empty() {
            self.status = match &run.progress {
                None => format!(
                    "Variant analysis {} has not reported any progress yet",
                    run.id
                ),
                Some(_) => format!("Variant analysis {} has no results yet", run.id),
            };
            return;
        }
        let dir = croft_cache_dir()
            .join("codeql")
            .join("variant-results")
            .join(format!("{}-{}", run.controller.replace('/', "-"), run.id));
        let gh = self.gh_program.to_string_lossy().into_owned();
        let program = self.codeql_program.clone();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let count = repos.len();
        std::thread::spawn(move || {
            let _ = tx.send(Self::fetch_codeql_variant_results(
                &gh, &program, &root, &run, &repos, &dir,
            ));
        });
        self.codeql_variant_results = Some(rx);
        self.status = format!(
            "Fetching the results of {count} repositor{}\u{2026}",
            if count == 1 { "y" } else { "ies" }
        );
    }

    /// The worker behind [`Self::open_codeql_variant_results`].
    pub(super) fn fetch_codeql_variant_results(
        gh: &str,
        program: &Path,
        root: &Path,
        run: &crate::codeql_submit::Submitted,
        repos: &[String],
        dir: &Path,
    ) -> VariantResults {
        use crate::codeql_submit::{self as cs, RepoResults};
        let mut sarif = Vec::new();
        let mut tables = Vec::new();
        let mut failed = Vec::new();
        for nwo in repos {
            let repo_dir = dir.join(nwo.replace('/', "-"));
            let fetched = (|| -> Result<(Option<String>, RepoResults), String> {
                let sha_file = repo_dir.join("commit");
                if !repo_dir.join("done").is_file() {
                    let answer = crate::pr_review::run_gh(
                        gh,
                        &cs::repo_task_args(&run.controller, run.id, nwo),
                        root,
                        crate::pr_review::GH_TIMEOUT,
                    )?;
                    let (url, sha) = cs::parse_artifact(&answer)?;
                    if !cs::artifact_url_allowed(&url) {
                        return Err(String::from(
                            "GitHub gave a results link croft will not fetch",
                        ));
                    }
                    let _ = std::fs::remove_dir_all(&repo_dir);
                    std::fs::create_dir_all(&repo_dir)
                        .map_err(|e| format!("{}: {e}", repo_dir.display()))?;
                    let resp = crate::net::builder(&url)
                        .timeout(std::time::Duration::from_secs(300))
                        .build()
                        .get(&url)
                        .call()
                        .map_err(|e| format!("download failed: {e}"))?;
                    let zip = repo_dir.join("results.zip");
                    let mut f = std::fs::File::create(&zip).map_err(|e| e.to_string())?;
                    std::io::copy(&mut resp.into_reader(), &mut f)
                        .map_err(|e| format!("download failed: {e}"))?;
                    crate::codeql_db::extract_zip(&zip, &repo_dir, Some(cs::ARTIFACT_LIMIT))?;
                    let _ = std::fs::remove_file(&zip);
                    std::fs::write(&sha_file, sha.unwrap_or_default())
                        .map_err(|e| e.to_string())?;
                    std::fs::write(repo_dir.join("done"), "").map_err(|e| e.to_string())?;
                }
                let sha = std::fs::read_to_string(&sha_file)
                    .ok()
                    .filter(|s| !s.is_empty());
                let found = |name: &str| cs::find_named(&repo_dir, name);
                if let Some(path) = found("results.sarif") {
                    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    return Ok((sha, RepoResults::Sarif(text)));
                }
                let bqrs = found("results.bqrs")
                    .ok_or_else(|| String::from("its results hold neither SARIF nor BQRS"))?;
                let csv = repo_dir.join("results.csv");
                if !csv.is_file() {
                    Self::codeql_decode_sets(program, &bqrs, &csv, &|args| {
                        Self::codeql_command(program, &args)
                    })?;
                }
                let text = std::fs::read_to_string(&csv).map_err(|e| e.to_string())?;
                Ok((sha, RepoResults::Table(text)))
            })();
            match fetched {
                Ok((sha, RepoResults::Sarif(text))) => sarif.push((nwo.clone(), sha, text)),
                Ok((_, RepoResults::Table(text))) => tables.push((nwo.clone(), text)),
                Err(why) => failed.push((nwo.clone(), why)),
            }
        }
        let write = |name: &str, text: String| -> Result<PathBuf, String> {
            let path = dir.join(name);
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(path)
        };
        let sarif_path = match sarif.is_empty() {
            true => None,
            false => Some(write("results.sarif", cs::combine_sarif(&sarif)?)?),
        };
        let csv_path = match tables.is_empty() {
            true => None,
            false => Some(write("results.csv", cs::combine_csv(&tables))?),
        };
        Ok(VariantResultFiles {
            sarif: sarif_path,
            csv: csv_path,
            repos: sarif.len() + tables.len(),
            failed,
        })
    }

    /// Open the results a finished fetch made: the SARIF log in the viewer
    /// (else the CSV), saying what else there is and what failed.
    pub fn drain_codeql_variant_results(&mut self) -> bool {
        let Some(rx) = self.codeql_variant_results.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the fetch stopped without answering"))
            }
        };
        self.codeql_variant_results = None;
        let files = match outcome {
            Ok(files) => files,
            Err(why) => {
                self.status = format!("Could not open the variant analysis results: {why}");
                return true;
            }
        };
        let failed = match files.failed.as_slice() {
            [] => String::new(),
            [(nwo, why)] => format!("; {nwo} failed: {why}"),
            [(nwo, why), rest @ ..] => {
                format!("; {nwo} failed: {why} (and {} more)", rest.len())
            }
        };
        let Some(open) = files.sarif.clone().or_else(|| files.csv.clone()) else {
            self.status = format!("No repository's results could be fetched{failed}");
            return true;
        };
        if let Err(e) = self.editor.open(&open) {
            self.status = format!("{}: {e}", open.display());
            return true;
        }
        self.sync_open_file_poll_mtime();
        self.focus_pane(Pane::Editor);
        let tables = match (&files.sarif, &files.csv) {
            (Some(_), Some(csv)) => format!("; tables in {}", csv.display()),
            _ => String::new(),
        };
        self.status = format!(
            "Results of {} repositor{}{tables}{failed}",
            files.repos,
            if files.repos == 1 { "y" } else { "ies" }
        );
        true
    }

    /// Ask where to export the results of remembered variant analysis
    /// `index` (the newest when `None`): a folder, offered as one in the
    /// workspace, or `gist` (#578, VS Code's "Export results").
    pub(super) fn prompt_export_codeql_variant_results(&mut self, index: Option<usize>) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let runs = crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path());
        let Some(i) = index.or(runs.len().checked_sub(1)) else {
            self.status = String::from("No variant analysis has been submitted yet");
            return;
        };
        let Some(run) = runs.get(i) else {
            return;
        };
        if crate::codeql_submit::repos_with_results(run).is_empty() {
            self.status = format!("Variant analysis {} has no results to export", run.id);
            return;
        }
        let folder = self
            .workspace_root()
            .join(format!("codeql-variant-analysis-{}", run.id));
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlExportVariantResults { index: i },
                format!("Export Variant Analysis {} Results", run.id),
                "a folder for the Markdown files, or gist for a secret gist",
            )
            .with_value(folder.display().to_string()),
        );
    }

    /// Export on a worker thread: fetch the results as
    /// [`Self::open_codeql_variant_results`] does (downloaded copies are
    /// reused), write them as Markdown, and for `gist` post the files with
    /// `gh gist create`.
    pub(super) fn start_export_codeql_variant_results(&mut self, index: usize, value: &str) {
        use crate::codeql_submit as cs;
        if self.codeql_variant_export.is_some() {
            self.status = String::from("Variant analysis results are already being exported");
            return;
        }
        let value = value.trim();
        if value.is_empty() {
            self.status = String::from("Give a folder, or gist");
            return;
        }
        let runs = cs::load_submitted(&Self::codeql_variant_runs_path());
        let Some(run) = runs.get(index).cloned() else {
            self.status = String::from("That variant analysis is no longer remembered");
            return;
        };
        let gist = value.eq_ignore_ascii_case("gist");
        let base = croft_cache_dir()
            .join("codeql")
            .join("variant-results")
            .join(format!("{}-{}", run.controller.replace('/', "-"), run.id));
        let target = if gist {
            base.join("export")
        } else {
            self.typed_path(value)
        };
        let repos = cs::repos_with_results(&run);
        let gh = self.gh_program.to_string_lossy().into_owned();
        let program = self.codeql_program.clone();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = (|| {
                let files =
                    Self::fetch_codeql_variant_results(&gh, &program, &root, &run, &repos, &base)?;
                if files.repos == 0 {
                    let why = files.failed.first().map_or("", |(_, w)| w.as_str());
                    return Err(format!("no repository's results could be fetched: {why}"));
                }
                let read = |p: &Option<PathBuf>| -> Result<Option<String>, String> {
                    p.as_ref()
                        .map(|p| {
                            std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
                        })
                        .transpose()
                };
                let markdown = cs::export_markdown(
                    &run,
                    read(&files.sarif)?.as_deref(),
                    read(&files.csv)?.as_deref(),
                )?;
                if gist {
                    let _ = std::fs::remove_dir_all(&target);
                }
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
                let mut written = Vec::new();
                for (name, text) in &markdown {
                    let path = target.join(name);
                    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
                    written.push(path);
                }
                let url = match gist {
                    false => None,
                    true => {
                        let name = run
                            .query
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let desc = format!(
                            "{name} (variant analysis {} of {} repositories)",
                            run.id, files.repos
                        );
                        let out = crate::pr_review::run_gh(
                            &gh,
                            &cs::gist_args(&desc, &written),
                            &root,
                            crate::pr_review::GH_TIMEOUT,
                        )?;
                        Some(
                            out.lines()
                                .rev()
                                .find(|l| l.starts_with("https://"))
                                .ok_or_else(|| String::from("gh made no gist"))?
                                .trim()
                                .to_string(),
                        )
                    }
                };
                Ok(VariantExport {
                    summary: (!gist).then(|| target.join("_summary.md")),
                    gist: url,
                    repos: files.repos,
                    failed: files.failed,
                })
            })();
            let _ = tx.send(outcome);
        });
        self.codeql_variant_export = Some(rx);
        self.status = String::from("Exporting the variant analysis results\u{2026}");
    }

    /// Report a finished export: open a folder export's summary, or name
    /// the gist.
    pub fn drain_codeql_variant_export(&mut self) -> bool {
        let Some(rx) = self.codeql_variant_export.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the export stopped without answering"))
            }
        };
        self.codeql_variant_export = None;
        let done = match outcome {
            Ok(done) => done,
            Err(why) => {
                self.status = format!("Could not export the variant analysis results: {why}");
                return true;
            }
        };
        let failed = match done.failed.as_slice() {
            [] => String::new(),
            [(nwo, why)] => format!("; {nwo} left out: {why}"),
            [(nwo, why), rest @ ..] => format!("; {nwo} left out: {why} (and {} more)", rest.len()),
        };
        let plural = if done.repos == 1 { "y" } else { "ies" };
        self.status = match (done.gist, done.summary) {
            (Some(url), _) => format!("Exported {} repositor{plural} to {url}{failed}", done.repos),
            (None, Some(summary)) => {
                if self.editor.open(&summary).is_ok() {
                    self.sync_open_file_poll_mtime();
                    self.focus_pane(Pane::Editor);
                }
                let dir = summary.parent().unwrap_or(&summary).display().to_string();
                format!("Exported {} repositor{plural} to {dir}{failed}", done.repos)
            }
            (None, None) => String::from("Exported nothing"),
        };
        true
    }

    /// Where submitted variant analyses are remembered (#578).
    pub(super) fn codeql_variant_runs_path() -> PathBuf {
        croft_cache_dir().join("codeql-variant-analyses.json")
    }

    /// Submit the open query as a variant analysis (#578, VS Code's "CodeQL:
    /// Run Variant Analysis"): bundle it into a query pack and post it to
    /// the controller repository, which runs it on GitHub Actions against
    /// the selected list, repository or owner. The bundling and the post
    /// happen on a worker thread; [`Self::drain_codeql_variant_submit`]
    /// reports the run.
    pub(super) fn run_codeql_variant_analysis(&mut self) {
        use crate::codeql_submit as cs;
        if self.codeql_variant_submit.is_some() {
            self.status = String::from("A variant analysis is already being submitted");
            return;
        }
        let query = match self.editor.path.clone() {
            Some(p) if p.extension().is_some_and(|e| e == "ql") => p,
            _ => {
                self.status = String::from("Open a .ql query to run a variant analysis");
                return;
            }
        };
        // The bundle is built from the file on disk.
        if self.editor.dirty {
            self.status = String::from("Save the query before running a variant analysis");
            return;
        }
        let config = match crate::codeql_variant::VariantConfig::load(&Self::codeql_variant_path())
        {
            Ok(c) => c,
            Err(why) => {
                self.status = format!("Could not read the variant analysis config: {why}");
                return;
            }
        };
        let Some(controller) = config.controller_repo.clone() else {
            self.status = String::from(
                "Set up a controller repository under Variant Analysis Repositories first",
            );
            return;
        };
        let targets = match cs::targets(&config) {
            Ok(t) => t,
            Err(why) => {
                self.status = why;
                return;
            }
        };
        let pack_language = cs::enclosing_pack(&query)
            .and_then(|(_, file)| std::fs::read_to_string(file).ok())
            .and_then(|text| crate::codeql_query::pack_language(&text));
        let Some(language) =
            pack_language.or_else(|| self.codeql_search_language().map(str::to_string))
        else {
            self.status = String::from(
                "The query's pack names no language: pick one in the CodeQL side bar's Language section",
            );
            return;
        };
        let submitted_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stem = query
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let work = croft_cache_dir()
            .join("codeql")
            .join("variant")
            .join(format!("{submitted_at}-{stem}"));
        let program = self.codeql_program.clone();
        let gh = self.gh_program.to_string_lossy().into_owned();
        let root = self.active_workspace_root().to_path_buf();
        let describe = targets.describe();
        let (tx, rx) = std::sync::mpsc::channel();
        let query_for_run = query.clone();
        std::thread::spawn(move || {
            let query = query_for_run;
            let outcome = (|| {
                let _ = std::fs::remove_dir_all(&work);
                let pack = cs::prepare_pack(&query, &language, &work)?;
                let run = |args: Vec<String>| Self::codeql_command(&program, &args);
                run(crate::codeql_query::pack_install_args(&pack))
                    .map_err(|e| format!("installing the pack's dependencies failed: {e}"))?;
                let bundle = work.join("pack.tgz");
                run(cs::pack_bundle_args(&pack, &bundle))
                    .map_err(|e| format!("bundling the query failed: {e}"))?;
                let bytes =
                    std::fs::read(&bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
                let body = work.join("body.json");
                std::fs::write(
                    &body,
                    cs::submission_body(&language, &bytes, &targets).to_string(),
                )
                .map_err(|e| format!("{}: {e}", body.display()))?;
                let answer = crate::pr_review::run_gh(
                    &gh,
                    &cs::submit_args(&controller, &body),
                    &root,
                    crate::pr_review::GH_TIMEOUT,
                )?;
                cs::parse_submission(&answer, &controller, &query, &language, submitted_at)
            })();
            let _ = std::fs::remove_dir_all(&work);
            let _ = tx.send(outcome);
        });
        self.codeql_variant_submit = Some(rx);
        self.status = format!(
            "Submitting {} as a variant analysis against {describe}\u{2026}",
            query.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    /// Report a finished variant analysis submission, and remember the run.
    /// Returns true when one finished.
    pub fn drain_codeql_variant_submit(&mut self) -> bool {
        let Some(rx) = self.codeql_variant_submit.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the submission stopped without answering"))
            }
        };
        self.codeql_variant_submit = None;
        self.status = match outcome {
            Ok(run) => {
                let name = run
                    .query
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let skipped = match run.skipped {
                    0 => String::new(),
                    n => format!(
                        ", {n} repositor{} skipped",
                        if n == 1 { "y" } else { "ies" }
                    ),
                };
                let url = run.url();
                let id = run.id;
                match crate::codeql_submit::record_submitted(&Self::codeql_variant_runs_path(), run)
                {
                    Ok(()) => {
                        // Follow it from now on, starting with the next tick.
                        self.codeql_variant_polled = None;
                        self.refresh_codeql_variant_runs();
                        format!("Variant analysis {id} of {name} submitted{skipped}: {url}")
                    }
                    Err(e) => format!(
                        "Variant analysis {id} of {name} submitted{skipped}: {url} (not remembered: {e})"
                    ),
                }
            }
            Err(why) => format!("Could not submit the variant analysis: {why}"),
        };
        true
    }

    /// Run the open `.ql` query or `.qls` suite on the current database
    /// (#578). The CLI reads the file from disk, so an unsaved buffer is
    /// refused rather than silently running the saved text.
    pub(super) fn run_codeql_query(&mut self) {
        let query = match self.editor.path.clone() {
            Some(p) if p.extension().is_some_and(|e| e == "ql" || e == "qls") => p,
            _ => {
                self.status = String::from("Open a .ql query or .qls suite to run it");
                return;
            }
        };
        if self.editor.dirty {
            let name = query
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.status = format!("Save {name} before running it: CodeQL reads it from disk");
            return;
        }
        let source = self.editor.lines.join("\n");
        self.run_codeql_file(query, &source);
    }

    /// VS Code's "CodeQL: Run Query Suite" (#578): the open `.qls` suite on
    /// the current database, through `database analyze` into SARIF.
    pub(super) fn run_codeql_query_suite(&mut self) {
        if !self
            .editor
            .path
            .as_deref()
            .is_some_and(crate::codeql_query::is_suite)
        {
            self.status = String::from("Open a .qls query suite to run it");
            return;
        }
        self.run_codeql_query();
    }

    /// Run `query`, whose text is `source`, on the current database (#578):
    /// alerts through `database analyze` into SARIF, anything else through
    /// `query run` into a table decoded as CSV. The run happens on a worker
    /// thread; [`Self::drain_codeql_run`] collects it.
    pub(super) fn run_codeql_file(&mut self, query: PathBuf, source: &str) {
        self.run_codeql_file_with(query, source, None);
    }

    /// [`Self::run_codeql_file`], or with `quick` a quick evaluation of a
    /// span of `query` through the CLI's query server (#578), recorded in
    /// the history like any run and decoded to a table.
    pub(super) fn run_codeql_file_with(
        &mut self,
        query: PathBuf,
        source: &str,
        quick: Option<crate::codeql_qs::QuickEval>,
    ) {
        let mode = match quick {
            Some(q) => CodeqlRunMode::Quick(q),
            None => CodeqlRunMode::Whole,
        };
        self.run_codeql_file_on(query, source, mode, None);
    }

    /// CodeQL: Run Queries in Published Pack (#578): ask which pack.
    pub(super) fn prompt_run_codeql_published_pack(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlRunPublishedPack,
            String::from("Run Queries in Published Pack"),
            "e.g. codeql/python-queries, scope/name@1.0.0, scope/name:path/in/pack",
        ));
    }

    /// Download the pack `reference` names and run it, or the part of it
    /// after `:`, on the current database into SARIF (#578).
    pub(super) fn run_codeql_published_pack(&mut self, reference: &str) {
        let reference = reference.trim();
        if crate::codeql_query::published_pack(reference).is_none() {
            self.status = format!("{reference:?} is not a pack: write scope/name");
            return;
        }
        self.run_codeql_file_on(PathBuf::from(reference), "", CodeqlRunMode::Pack, None);
    }

    /// [`Self::run_codeql_file_with`] on the database at `database`, or on
    /// the current one when that is `None`.
    pub(super) fn run_codeql_file_on(
        &mut self,
        query: PathBuf,
        source: &str,
        mode: CodeqlRunMode,
        database: Option<&Path>,
    ) {
        use crate::codeql_query::{self as cq, History, HistoryEntry, Output, RunStatus};
        if self.codeql_run.is_some() {
            self.status = String::from("A CodeQL query is already running");
            return;
        }
        if let Some((_, job, _, _)) = self.codeql_upgrade.as_ref() {
            self.status = format!("Wait for the CodeQL database {} to finish", job.noun());
            return;
        }
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let db = match database {
            Some(path) => store.databases.iter().find(|db| db.path == path).cloned(),
            None => store.current.and_then(|i| store.databases.get(i)).cloned(),
        };
        let Some(db) = db else {
            self.status = match database {
                Some(path) => format!("The CodeQL database {} is gone", path.display()),
                None => String::from("Add a CodeQL database and select it first"),
            };
            return;
        };
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stem = query
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Runs of a pack start back to back, so two queries of the same
        // name can start within the same second; each keeps its own folder.
        let base = Self::codeql_results_dir().join(format!("{started}-{stem}"));
        let mut dir = base.clone();
        let mut n = 1;
        while dir.exists() {
            n += 1;
            dir = PathBuf::from(format!("{}-{n}", base.display()));
        }
        let kind = match mode {
            CodeqlRunMode::Quick(_) => Output::Table,
            CodeqlRunMode::Pack => Output::Sarif,
            CodeqlRunMode::Whole => cq::output_of(&query, source),
        };
        let output = dir.join(match kind {
            Output::Sarif => "results.sarif",
            Output::Table => "results.csv",
        });
        // The query as it is run, for "View Query Text": the file may change
        // or go before anyone looks back at this run. Read-only, since
        // editing it would change nothing.
        let text = cq::query_text_path(&output, &query);
        if !matches!(mode, CodeqlRunMode::Pack)
            && let Some(parent) = text.parent()
            && std::fs::create_dir_all(parent).is_ok()
            && std::fs::write(&text, source).is_ok()
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&text, std::fs::Permissions::from_mode(0o444));
        }
        let mut history = History::load(&Self::codeql_history_path());
        history.push(HistoryEntry {
            query: query.clone(),
            database: db.name.clone(),
            database_path: Some(db.path.clone()),
            started,
            seconds: 0,
            status: RunStatus::Running,
            output: output.clone(),
            name: match &mode {
                CodeqlRunMode::Quick(q) => Some(q.title()),
                _ => None,
            },
            results: None,
        });
        let _ = history.save(&Self::codeql_history_path());
        self.refresh_codeql_history();
        let name = match &mode {
            CodeqlRunMode::Quick(q) => q.title(),
            CodeqlRunMode::Pack => query.display().to_string(),
            CodeqlRunMode::Whole => query
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        // Where the query server looks for the workspace's own packs.
        let packs: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
        // The workspace's model packs (#578), which customize the run.
        let models = crate::codeql_model::model_pack_args(self.workspace_root());
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        // A fresh flag per run, so a late cancel never stops the next one.
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.codeql_cancel = cancel.clone();
        std::thread::spawn(move || {
            let run = |args: Vec<String>| Self::codeql_command_until(&program, &args, &cancel);
            let outcome = std::fs::create_dir_all(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))
                .and_then(|()| match (mode, kind) {
                    (CodeqlRunMode::Quick(q), _) => {
                        let bqrs = dir.join("results.bqrs");
                        crate::codeql_qs::run_quick_eval(
                            &program, &db.path, &query, &bqrs, &packs, q.span, q.count, &cancel,
                        )
                        .and_then(|()| Self::codeql_decode_sets(&program, &bqrs, &output, &run))
                    }
                    (CodeqlRunMode::Pack, _) => {
                        let reference = query.to_string_lossy();
                        let pack = cq::published_pack(&reference).unwrap_or_default();
                        run(cq::pack_download_args(&[pack.to_string()])).and_then(|()| {
                            let mut args = cq::analyze_args(&query, &db.path, &output);
                            args.extend(models.iter().cloned());
                            run(args)
                        })
                    }
                    (CodeqlRunMode::Whole, Output::Sarif) => {
                        let mut args = cq::analyze_args(&query, &db.path, &output);
                        args.extend(models.iter().cloned());
                        if cq::is_suite(&query) {
                            Self::check_codeql_suite(&program, &query).and_then(|()| run(args))
                        } else {
                            run(args)
                        }
                    }
                    (CodeqlRunMode::Whole, Output::Table) => {
                        let bqrs = dir.join("results.bqrs");
                        let mut args = cq::run_args(&query, &db.path, &bqrs);
                        args.extend(models.iter().cloned());
                        run(args)
                            .and_then(|()| Self::codeql_decode_sets(&program, &bqrs, &output, &run))
                    }
                });
            let _ = tx.send(match outcome {
                Ok(()) => RunStatus::Succeeded,
                Err(_) if cancel.load(std::sync::atomic::Ordering::SeqCst) => RunStatus::Cancelled,
                Err(e) => RunStatus::Failed(e),
            });
        });
        self.codeql_run = Some((rx, std::time::Instant::now()));
        self.codeql_run_name = name.clone();
        self.status = format!("Running {name} on {}\u{2026}", db.name);
    }

    /// Refuse suite `suite` when it selects a query whose results are not
    /// alerts (#578): `database analyze` cannot produce those tables.
    pub(super) fn check_codeql_suite(program: &Path, suite: &Path) -> Result<(), String> {
        let out = crate::review_ops::output_retrying_busy(
            std::process::Command::new(program)
                .args(crate::codeql_query::resolve_suite_args(suite)),
        )
        .map_err(|e| format!("could not run codeql: {e}"))?;
        if !out.status.success() {
            return Err(crate::codeql_query::failure_reason(
                &String::from_utf8_lossy(&out.stderr),
            ));
        }
        let tables = crate::codeql_query::table_queries_in_suite(
            &String::from_utf8_lossy(&out.stdout),
            |p| std::fs::read_to_string(p).ok(),
        )?;
        if tables.is_empty() {
            return Ok(());
        }
        Err(format!(
            "the suite selects queries that are not alert queries ({}); run those on their own",
            tables.join(", ")
        ))
    }

    /// CodeQL: Quick Evaluation, and its count variant (#578): evaluate the
    /// predicate under the cursor, or the selected expression, of the open
    /// query on the current database, through the CLI's query server.
    pub(super) fn quick_eval_codeql(&mut self, count: bool) {
        let Some(query) = self
            .editor
            .path
            .clone()
            .filter(|p| p.extension().is_some_and(|e| e == "ql"))
        else {
            self.status = String::from("Open a .ql query to quick-evaluate part of it");
            return;
        };
        if self.editor.dirty {
            self.status = String::from("Save the query first: the query server reads it from disk");
            return;
        }
        let selection = self.editor.selection.map(|s| s.normalised());
        let Some((span, label)) = crate::codeql_qs::span_at(
            &self.editor.lines,
            self.editor.cursor_row,
            self.editor.cursor_col,
            selection,
        ) else {
            self.status = String::from("Put the cursor on a predicate, or select an expression");
            return;
        };
        let source = self.editor.lines.join("\n");
        self.run_codeql_file_with(
            query,
            &source,
            Some(crate::codeql_qs::QuickEval { span, count, label }),
        );
    }

    /// CodeQL: Debug Query (#578): evaluate the open query up to its first
    /// breakpoint, the way VS Code's CodeQL debugger stops at one: the
    /// predicate the breakpoint's line defines or calls is quick-evaluated.
    /// With no breakpoints the whole query runs.
    pub(super) fn debug_codeql_query(&mut self) {
        let Some(query) = self
            .editor
            .path
            .clone()
            .filter(|p| p.extension().is_some_and(|e| e == "ql"))
        else {
            self.status = String::from("Open a .ql query to debug it");
            return;
        };
        let lines: Vec<usize> = self
            .editor
            .breakpoints
            .get(&query)
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default();
        if lines.is_empty() {
            self.run_codeql_query();
            return;
        }
        if self.editor.dirty {
            self.status = String::from("Save the query first: the query server reads it from disk");
            return;
        }
        let target = lines.iter().find_map(|&line| {
            let text = self.editor.lines.get(line.checked_sub(1)?)?;
            crate::codeql_qs::breakpoint_target(text, line as u32)
        });
        let Some((span, name)) = target else {
            self.status = format!(
                "No predicate at the breakpoint on line {}: put it on a line that defines or calls one",
                lines[0]
            );
            return;
        };
        let source = self.editor.lines.join("\n");
        let label = format!("{name} (breakpoint on line {})", span.line);
        self.run_codeql_file_with(
            query,
            &source,
            Some(crate::codeql_qs::QuickEval {
                span,
                count: false,
                label,
            }),
        );
    }

    /// [`Self::codeql_command`], killing `codeql` as soon as `cancel` is
    /// raised (#578). Its output is drained on threads of its own, so a
    /// chatty CLI never blocks on a full pipe while this polls.
    pub(super) fn codeql_command_until(
        program: &Path,
        args: &[String],
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String> {
        use std::io::Read as _;
        use std::sync::atomic::Ordering;
        if cancel.load(Ordering::SeqCst) {
            return Err(String::from("cancelled"));
        }
        let mut child = crate::review_ops::spawn_retrying_busy(
            std::process::Command::new(program)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped()),
        )
        .map_err(|e| format!("could not run codeql: {e}"))?;
        let mut stderr = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_string(&mut text);
            }
            text
        });
        let status = loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(String::from("cancelled"));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(e) => return Err(format!("could not wait for codeql: {e}")),
            }
        };
        let err = reader.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(crate::codeql_query::failure_reason(&err))
        }
    }

    /// CodeQL: Cancel Running Query (#578): stop the query in flight, and
    /// the queries a pack or multi-database run has queued after it.
    pub(super) fn cancel_codeql_run(&mut self) {
        if self.codeql_run.is_none() {
            self.status = String::from("No CodeQL query is running");
            return;
        }
        self.codeql_cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queued = self.codeql_run_queue.len();
        self.codeql_run_queue.clear();
        self.status = match queued {
            0 => String::from("Cancelling the running CodeQL query\u{2026}"),
            n => format!("Cancelling the running CodeQL query and {n} queued\u{2026}"),
        };
    }

    /// Run `codeql` with `args` and return what it printed on stdout; a
    /// failure is read as [`Self::codeql_command`] reads it.
    pub(super) fn codeql_stdout(program: &Path, args: &[String]) -> Result<String, String> {
        let out =
            crate::review_ops::output_retrying_busy(std::process::Command::new(program).args(args))
                .map_err(|e| format!("could not run codeql: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(crate::codeql_query::failure_reason(
            &String::from_utf8_lossy(&out.stderr),
        ))
    }

    /// What a contextual query (AST, CFG) runs on (#578): the open file,
    /// the current database, its language, and the file's name in that
    /// database's source archive. Says what is missing when something is.
    pub(super) fn codeql_contextual_target(
        &mut self,
    ) -> Option<(PathBuf, crate::codeql_db::DbEntry, String, String)> {
        let Some(file) = self.editor.path.clone() else {
            self.status = String::from("Open a source file from the CodeQL database first");
            return None;
        };
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.current.and_then(|i| store.databases.get(i)).cloned() else {
            self.status = String::from("Select a CodeQL database first");
            return None;
        };
        let Some(lang) = db.language.clone() else {
            self.status = format!("CodeQL database {} does not say its language", db.name);
            return None;
        };
        // The places this database's source can be open from: its `src`
        // folder and its extracted `src.zip`s, which both mirror the
        // absolute paths it was built from.
        let prefix = crate::codeql_db::source_cache_prefix(&db.path);
        let mut roots: Vec<PathBuf> = std::fs::read_dir(Self::codeql_source_cache_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
            .collect();
        roots.push(db.path.join("src"));
        let archive = crate::codeql_ast::archive_path(&file, &roots);
        Some((file, db, lang, archive))
    }

    /// A fresh folder for one contextual query run of `kind`.
    pub(super) fn codeql_contextual_dir(kind: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        croft_cache_dir()
            .join("codeql")
            .join(kind)
            .join(stamp.to_string())
    }

    /// Resolve `lang`'s contextual query tagged `tag`, and make sure its
    /// pack's dependencies are there (#578).
    pub(super) fn codeql_contextual_query(
        program: &Path,
        dir: &Path,
        lang: &str,
        tag: &str,
    ) -> Result<PathBuf, String> {
        use crate::codeql_ast as ast;
        let suite = dir.join(format!("{tag}.qls"));
        std::fs::write(&suite, ast::contextual_suite(lang, tag))
            .map_err(|e| format!("{}: {e}", suite.display()))?;
        let listed = Self::codeql_stdout(program, &ast::resolve_args(&suite))?;
        let query = ast::first_query(&listed).ok_or_else(|| {
            format!(
                "no {tag} query for {lang}; download the codeql/{lang}-all pack \
                 with CodeQL: Download Packs"
            )
        })?;
        // A library pack downloaded on its own may lack what it depends
        // on; installing is quick when nothing is missing.
        if let Some(pack) = ast::pack_dir(&query) {
            Self::codeql_command(program, &crate::codeql_query::pack_install_args(&pack))?;
        }
        Ok(query)
    }

    /// CodeQL: View CFG (#578): the control flow graph of the function at
    /// the cursor, from the current database, opened as a document.
    pub(super) fn view_codeql_cfg(&mut self) {
        use crate::codeql_ast as ast;
        if self.codeql_doc_job.is_some() {
            self.status = String::from("A CodeQL view is already being made");
            return;
        }
        let Some((file, db, lang, archive)) = self.codeql_contextual_target() else {
            return;
        };
        let (line, column) = (self.editor.cursor_row + 1, self.editor.cursor_col + 1);
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = format!("{name}:{line}:{column} in {}", db.name);
        let dir = Self::codeql_contextual_dir("cfg");
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read = || -> Result<PathBuf, String> {
                let io = |e: std::io::Error| format!("{}: {e}", dir.display());
                std::fs::create_dir_all(&dir).map_err(io)?;
                let query = Self::codeql_contextual_query(&program, &dir, &lang, "print-cfg")?;
                let (f, l, c) = (
                    dir.join("file.csv"),
                    dir.join("line.csv"),
                    dir.join("column.csv"),
                );
                std::fs::write(&f, ast::selected_file_csv(&archive)).map_err(io)?;
                std::fs::write(&l, format!("{line}\n")).map_err(io)?;
                std::fs::write(&c, format!("{column}\n")).map_err(io)?;
                let bqrs = dir.join("cfg.bqrs");
                Self::codeql_command(
                    &program,
                    &crate::codeql_cfg::run_args(&query, &db.path, &f, &l, &c, &bqrs),
                )?;
                let json = dir.join("cfg.json");
                Self::codeql_command(&program, &ast::decode_args(&bqrs, &json))?;
                let cfg =
                    crate::codeql_cfg::parse_cfg(&std::fs::read_to_string(&json).map_err(io)?)?;
                let doc = dir.join("cfg.md");
                std::fs::write(&doc, crate::codeql_cfg::render_document(&cfg, &title))
                    .map_err(io)?;
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&doc, std::fs::Permissions::from_mode(0o444));
                Ok(doc)
            };
            let _ = tx.send(read());
        });
        self.status = format!("Reading the control flow graph at {name}:{line}\u{2026}");
        self.codeql_doc_job = Some((
            rx,
            String::from("Control flow graph"),
            String::from("Could not read the control flow graph"),
        ));
    }

    /// Open a finished CodeQL document (#578), or say why there is none.
    pub fn drain_codeql_doc(&mut self) -> bool {
        let Some((rx, ..)) = self.codeql_doc_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("it stopped unexpectedly"))
            }
        };
        let Some((_, done, failed)) = self.codeql_doc_job.take() else {
            return false;
        };
        self.status = match outcome {
            Ok(doc) => match self.editor.open(&doc) {
                Ok(()) => done,
                Err(e) => format!("{}: {e}", doc.display()),
            },
            Err(why) => format!("{failed}: {why}"),
        };
        true
    }

    /// CodeQL: Open Model Editor (#578): read the current database's
    /// library endpoints with its language's model-editor query, off the
    /// UI thread; [`Self::drain_codeql_model`] lists them under Method
    /// Modeling.
    pub(super) fn open_codeql_model_editor(&mut self) {
        use crate::codeql_query as cq;
        if self.codeql_model_job.is_some() {
            self.status = String::from("The model editor is already reading endpoints");
            return;
        }
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.current.and_then(|i| store.databases.get(i)).cloned() else {
            self.status = String::from("Select a CodeQL database first");
            return;
        };
        let Some(lang) = db.language.clone() else {
            self.status = format!("CodeQL database {} does not say its language", db.name);
            return;
        };
        let dir = Self::codeql_contextual_dir("model");
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let lang_for = lang.clone();
        std::thread::spawn(move || {
            let read = || -> Result<Vec<crate::codeql_model::Endpoint>, String> {
                let io = |e: std::io::Error| format!("{}: {e}", dir.display());
                std::fs::create_dir_all(&dir).map_err(io)?;
                let suite = dir.join("endpoints.qls");
                std::fs::write(&suite, crate::codeql_model::endpoints_suite(&lang_for))
                    .map_err(io)?;
                let listed =
                    Self::codeql_stdout(&program, &crate::codeql_ast::resolve_args(&suite))?;
                let query = crate::codeql_ast::first_query(&listed).ok_or_else(|| {
                    format!(
                        "no model-editor query for {lang_for}; download the \
                         codeql/{lang_for}-queries pack with CodeQL: Download Packs"
                    )
                })?;
                let bqrs = dir.join("endpoints.bqrs");
                Self::codeql_command(&program, &cq::run_args(&query, &db.path, &bqrs))?;
                let json = dir.join("endpoints.json");
                Self::codeql_command(
                    &program,
                    &cq::decode_locations_args(&bqrs, &json, "#select"),
                )?;
                let endpoints = crate::codeql_model::parse_endpoints(
                    &std::fs::read_to_string(&json).map_err(io)?,
                );
                if endpoints.is_empty() {
                    return Err(String::from(
                        "the database has no library endpoints to model (is it a library?)",
                    ));
                }
                Ok(endpoints)
            };
            let _ = tx.send(read());
        });
        self.status = format!("Reading the endpoints of {}\u{2026}", db.name);
        self.codeql_model_job = Some((rx, db.name, lang));
    }

    /// Model endpoint `endpoint` of the Model Editor (#578): pick what it
    /// is, as a source at its return value, a sink at an argument, or a
    /// summary, or remove the models made for it.
    pub(super) fn model_codeql_endpoint(&mut self, endpoint: Option<usize>) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let Some(view) = self.codeql.model.as_ref() else {
            self.status = String::from("Open the model editor first");
            return;
        };
        let Some(e) = endpoint.and_then(|i| view.endpoints.get(i)) else {
            self.status = String::from("Select an endpoint in Method Modeling first");
            return;
        };
        let choices = e.choices();
        if choices.is_empty() {
            self.status = format!("{} is a class; model its methods", e.label());
            return;
        }
        let i = endpoint.unwrap_or(0);
        let mut rows: Vec<ListRow> = choices
            .iter()
            .enumerate()
            .map(|(j, m)| ListRow {
                id: format!("{i}:{j}"),
                label: m.describe(),
            })
            .collect();
        let path = e.access_path();
        if view
            .models
            .iter()
            .any(|m| m.ty == e.namespace && Some(&m.path) == path.as_ref())
        {
            rows.push(ListRow {
                id: format!("{i}:remove"),
                label: String::from("remove the models made for it"),
            });
        }
        let title = format!("Model {}", e.label());
        self.open_list_picker(ListPicker::new(ListPurpose::CodeqlModel, title, rows), "");
    }

    /// A model picked for an endpoint (`<endpoint>:<choice>` or
    /// `<endpoint>:remove`): ask for its kind, or remove its models.
    pub(super) fn choose_codeql_model(&mut self, id: &str) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some((endpoint, choice)) = id.split_once(':') else {
            return;
        };
        let Ok(endpoint) = endpoint.parse::<usize>() else {
            return;
        };
        if choice == "remove" {
            let Some(view) = self.codeql.model.as_mut() else {
                return;
            };
            let Some(e) = view.endpoints.get(endpoint).cloned() else {
                return;
            };
            let path = e.access_path();
            view.models
                .retain(|m| !(m.ty == e.namespace && Some(&m.path) == path.as_ref()));
            self.status = match self.write_codeql_models() {
                Ok(file) => format!(
                    "Removed the models of {} from {}",
                    e.label(),
                    file.display()
                ),
                Err(why) => format!("Could not save the models: {why}"),
            };
            return;
        }
        let Ok(choice) = choice.parse::<usize>() else {
            return;
        };
        let Some(model) = self
            .codeql
            .model
            .as_ref()
            .and_then(|v| v.endpoints.get(endpoint))
            .and_then(|e| e.choices().into_iter().nth(choice))
        else {
            return;
        };
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlModelKind { endpoint, choice },
                format!("Kind of {}", model.describe()),
                String::from("remote, command-injection, sql-injection, taint, value, \u{2026}"),
            )
            .with_value(model.which.default_kind()),
        );
    }

    /// Save choice `choice` of endpoint `endpoint` with kind `kind` in the
    /// database's model pack (#578).
    pub(super) fn save_codeql_model(&mut self, endpoint: usize, choice: usize, kind: &str) {
        let kind = kind.trim();
        if kind.is_empty() {
            self.status = String::from("A model needs a kind");
            return;
        }
        let Some(view) = self.codeql.model.as_mut() else {
            return;
        };
        let Some(mut model) = view
            .endpoints
            .get(endpoint)
            .and_then(|e| e.choices().into_iter().nth(choice))
        else {
            return;
        };
        model.kind = kind.to_string();
        let label = view.endpoints[endpoint].label();
        let described = model.describe();
        view.models.retain(|m| *m != model);
        view.models.push(model);
        self.status = match self.write_codeql_models() {
            Ok(file) => format!("Modeled {label} as a {described} in {}", file.display()),
            Err(why) => format!("Could not save the model: {why}"),
        };
    }

    /// Write the Model Editor's models to the database's model pack: its
    /// pack file, the data extension CodeQL reads, and croft's own list.
    /// Returns the data extension's path.
    pub(super) fn write_codeql_models(&mut self) -> Result<PathBuf, String> {
        use crate::codeql_model as cm;
        let view = self
            .codeql
            .model
            .as_ref()
            .ok_or("no model editor is open")?;
        let (dir, name) = cm::model_pack(self.workspace_root(), &view.database, &view.language);
        let io = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(dir.join("models")).map_err(io)?;
        std::fs::write(
            dir.join("codeql-pack.yml"),
            cm::pack_yml(&name, &view.language),
        )
        .map_err(io)?;
        let slug = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = dir.join("models").join(format!("{slug}.model.yml"));
        std::fs::write(&file, cm::models_yml(&view.language, &view.models)).map_err(io)?;
        let json = serde_json::to_string_pretty(&view.models).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("croft-models.json"), json).map_err(io)?;
        Ok(file
            .strip_prefix(self.workspace_root())
            .unwrap_or(&file)
            .to_path_buf())
    }

    /// List a finished endpoints read under Method Modeling (#578), or say
    /// why there is none.
    pub fn drain_codeql_model(&mut self) -> bool {
        let Some((rx, ..)) = self.codeql_model_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the endpoints read stopped unexpectedly"))
            }
        };
        let Some((_, database, language)) = self.codeql_model_job.take() else {
            return false;
        };
        match outcome {
            Ok(endpoints) => {
                let modeled = endpoints.iter().filter(|e| e.supported).count();
                self.status = format!(
                    "{} endpoints of {database}, {modeled} modeled by CodeQL",
                    endpoints.len()
                );
                // The models made before, kept beside the pack.
                let (dir, _) =
                    crate::codeql_model::model_pack(self.workspace_root(), &database, &language);
                let models = std::fs::read_to_string(dir.join("croft-models.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default();
                self.codeql.model = Some(crate::codeql_model::ModelView {
                    database,
                    language,
                    endpoints,
                    models,
                    ..Default::default()
                });
                self.codeql
                    .collapsed
                    .remove(&crate::widgets::codeql::Section::MethodModeling);
            }
            Err(why) => self.status = format!("Could not read the endpoints: {why}"),
        }
        true
    }

    /// CodeQL: View AST (#578): the AST of the open file, read from the
    /// current database with its language's print-AST query, off the UI
    /// thread; [`Self::drain_codeql_ast`] shows it in the AST Viewer.
    pub(super) fn view_codeql_ast(&mut self) {
        use crate::codeql_ast as ast;
        if self.codeql_ast_job.is_some() {
            self.status = String::from("An AST is already being read");
            return;
        }
        let Some((file, db, lang, archive)) = self.codeql_contextual_target() else {
            return;
        };
        let dir = Self::codeql_contextual_dir("ast");
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let (db_path, csv) = (db.path.clone(), ast::selected_file_csv(&archive));
        let shown = archive.clone();
        std::thread::spawn(move || {
            let read = || -> Result<ast::AstTree, String> {
                let io = |e: std::io::Error| format!("{}: {e}", dir.display());
                std::fs::create_dir_all(&dir).map_err(io)?;
                let query = Self::codeql_contextual_query(&program, &dir, &lang, "print-ast")?;
                let selected = dir.join("selected-file.csv");
                std::fs::write(&selected, csv).map_err(io)?;
                let bqrs = dir.join("ast.bqrs");
                Self::codeql_command(&program, &ast::run_args(&query, &db_path, &selected, &bqrs))?;
                let json = dir.join("ast.json");
                Self::codeql_command(&program, &ast::decode_args(&bqrs, &json))?;
                let tree = ast::parse_graph(&std::fs::read_to_string(&json).map_err(io)?)?;
                if tree.roots.is_empty() {
                    return Err(format!("the database has no AST for {shown}"));
                }
                Ok(tree)
            };
            let _ = tx.send(read());
        });
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.status = format!("Reading the AST of {name} from {}\u{2026}", db.name);
        self.codeql_ast_job = Some((
            rx,
            ast::AstView {
                file,
                archive,
                database: db.name,
                ..ast::AstView::default()
            },
        ));
    }

    /// Show a finished AST read in the AST Viewer (#578), or say why there
    /// is none.
    pub fn drain_codeql_ast(&mut self) -> bool {
        let Some((rx, _)) = self.codeql_ast_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the AST read stopped unexpectedly"))
            }
        };
        let Some((_, mut view)) = self.codeql_ast_job.take() else {
            return false;
        };
        match outcome {
            Ok(tree) => {
                let name = view
                    .file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.status = format!("AST of {name}: {} nodes", tree.nodes.len());
                view.tree = tree;
                self.codeql.ast = Some(view);
                self.codeql
                    .collapsed
                    .remove(&crate::widgets::codeql::Section::AstViewer);
            }
            Err(why) => self.status = format!("Could not read the AST: {why}"),
        }
        true
    }

    /// Run `codeql` with `args` and wait. A failure is the reason and the
    /// fix the CLI printed on stderr ([`crate::codeql_query::failure_reason`]).
    pub(super) fn codeql_command(program: &Path, args: &[String]) -> Result<(), String> {
        let out =
            crate::review_ops::output_retrying_busy(std::process::Command::new(program).args(args))
                .map_err(|e| format!("could not run codeql: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(crate::codeql_query::failure_reason(
            &String::from_utf8_lossy(&out.stderr),
        ))
    }

    /// Collect a finished query run (#578): record how it went in the
    /// history, open its results when it succeeded, and start a pack run's
    /// next query.
    pub fn drain_codeql_run(&mut self) -> bool {
        use crate::codeql_query::RunStatus;
        let Some((rx, since)) = self.codeql_run.as_ref() else {
            return false;
        };
        let status = match rx.try_recv() {
            Ok(s) => s,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                RunStatus::Failed(String::from("the query run stopped unexpectedly"))
            }
        };
        let seconds = since.elapsed().as_secs();
        self.codeql_run = None;
        let failed = matches!(status, RunStatus::Failed(_));
        self.record_codeql_run(status, seconds);
        if self.codeql_batch.0 > 0 {
            self.codeql_batch.1 += usize::from(failed);
            self.start_next_queued_codeql();
        }
        true
    }

    /// Write a finished run's outcome into its history entry and open its
    /// results when it succeeded.
    pub(super) fn record_codeql_run(
        &mut self,
        status: crate::codeql_query::RunStatus,
        seconds: u64,
    ) {
        use crate::codeql_query::{History, RunStatus};
        let mut history = History::load(&Self::codeql_history_path());
        // The newest run still marked running is this one; a sorted history
        // need not have it on top, and it may have been removed meanwhile.
        let Some(entry) = history
            .entries
            .iter_mut()
            .filter(|e| e.status == RunStatus::Running)
            .max_by_key(|e| e.started)
        else {
            return;
        };
        entry.status = status.clone();
        entry.seconds = seconds;
        entry.results = (status == RunStatus::Succeeded)
            .then(|| crate::codeql_query::count_results(&entry.output))
            .flatten();
        let output = entry.output.clone();
        // A published pack's run names the pack by its reference.
        let name = match entry.query.is_relative() {
            true => entry.query.display().to_string(),
            false => entry
                .query
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let _ = history.save(&Self::codeql_history_path());
        self.refresh_codeql_history();
        match status {
            RunStatus::Failed(why) => self.status = format!("{name} failed: {why}"),
            RunStatus::Cancelled => self.status = format!("{name} cancelled"),
            _ => match self.editor.open(&output) {
                Ok(()) => {
                    self.sync_open_file_poll_mtime();
                    self.status = format!("{name} finished in {seconds}s");
                    let sets = crate::codeql_query::result_set_files(&output).len();
                    if sets > 1 && output.extension().is_some_and(|e| e == "csv") {
                        self.status.push_str(&format!(
                            "; {sets} result sets, see CodeQL: Show Result Set"
                        ));
                    }
                }
                Err(e) => self.status = format!("{}: {e}", output.display()),
            },
        }
    }

    /// Run every query in pack `pack` on the current database, one after
    /// another (#578): the first starts now and [`Self::drain_codeql_run`]
    /// starts each next one as the one before finishes, failed or not.
    /// Each run keeps its own history entry. Refused while a query is
    /// running, so two pack runs never interleave.
    pub(super) fn run_codeql_pack(&mut self, pack: usize) {
        if self.codeql_run.is_some() {
            self.status = String::from("A CodeQL query is already running");
            return;
        }
        let Some(queries) = self.codeql.queries.get(pack).map(|p| p.queries.clone()) else {
            return;
        };
        self.run_codeql_batch(queries, "That pack has no queries");
    }

    /// Run `queries` one after another on the current database, through
    /// the queue a pack run uses; `none` is what to say when there are none.
    pub(super) fn run_codeql_batch(&mut self, queries: Vec<PathBuf>, none: &str) {
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
    pub(super) fn run_codeql_query_on_databases(&mut self) {
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
    pub(super) fn open_codeql_multi_db_picker(&mut self, selected: usize) {
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
    pub(super) fn choose_codeql_multi_db(&mut self, id: &str, selected: usize) {
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
    pub(super) fn run_all_codeql_queries(&mut self) {
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
    pub(super) fn run_selected_codeql_queries(&mut self) {
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
    pub(super) fn start_next_queued_codeql(&mut self) {
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
    pub(super) fn cancel_codeql_queue(&mut self) {
        let left = self.codeql_run_queue.len();
        self.codeql_run_queue.clear();
        self.codeql_batch = (0, 0);
        self.status = match left {
            0 => String::from("No CodeQL queries are queued"),
            1 => String::from("Cancelled 1 queued CodeQL query"),
            n => format!("Cancelled {n} queued CodeQL queries"),
        };
    }

    /// Keep the chrome in step once CodeQL is added, uninstalled, enabled or
    /// disabled: a CodeQL that is off takes its open side bar with it.
    pub(super) fn codeql_availability_changed(&mut self) {
        if !self.is_extension_enabled("codeql") && self.sidebar_view == SidebarView::CodeQL {
            self.set_sidebar_view(SidebarView::Explorer);
        }
        // The icon appears or disappears, so the baked bar redraws.
        self.overlays.activity.mark_dirty();
    }
}

/// What a CodeQL run evaluates (#578): the whole query, a quick
/// evaluation of part of it, or a published pack named by its reference.
pub(super) enum CodeqlRunMode {
    Whole,
    Quick(crate::codeql_qs::QuickEval),
    Pack,
}

/// What "CodeQL: Check for CLI Updates" learns (#578): the latest release,
/// or why it could not be read, and the version in use when it runs.
pub(super) type CodeqlCliCheck = (Result<String, String>, Option<String>);

/// Query help being rendered (#578): where the outcome arrives, the
/// Markdown file to preview, and the query's name.
pub(super) type CodeqlQueryHelp = (
    std::sync::mpsc::Receiver<Result<(), String>>,
    PathBuf,
    String,
);

/// A `codeql pack` install or download in flight (#578): where its outcome
/// arrives, and the status lines for success and for failure.
pub(super) type CodeqlPackJob = (
    std::sync::mpsc::Receiver<Result<(), String>>,
    String,
    String,
);

/// A CodeQL document being made (#578): the file it writes, or why not,
/// and the status lines for success and for failure.
pub(super) type CodeqlDocJob = (
    std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    String,
    String,
);

/// An evaluator log being read for its viewer (#578): the predicates, or
/// why not, and the query whose log it is.
pub(super) type CodeqlLogViewerJob = (
    std::sync::mpsc::Receiver<Result<Vec<crate::codeql_evallog::Predicate>, String>>,
    String,
);

/// The rows of the Add CodeQL Database picker and the side-bar action each
/// runs.
pub(super) const CODEQL_DB_SOURCES: [(crate::widgets::codeql::Action, &str); 4] = [
    (
        crate::widgets::codeql::Action::AddDatabaseFromFolder,
        "From a folder",
    ),
    (
        crate::widgets::codeql::Action::AddDatabaseFromArchive,
        "From an archive",
    ),
    (
        crate::widgets::codeql::Action::AddDatabaseFromUrl,
        "From a URL (as a zip file)",
    ),
    (
        crate::widgets::codeql::Action::AddDatabaseFromGithub,
        "From GitHub",
    ),
];

/// The Model Editor's endpoints being read (#578): the endpoints, or why
/// not, and the database and language they are for.
pub(super) type CodeqlModelJob = (
    std::sync::mpsc::Receiver<Result<Vec<crate::codeql_model::Endpoint>, String>>,
    String,
    String,
);

/// A CodeQL database upgrade or cache cleanup in flight (#578): where its
/// outcome arrives, which job it is, and the database's name and folder.
pub(super) type CodeqlUpgrade = (
    std::sync::mpsc::Receiver<Result<(), String>>,
    crate::codeql_query::DbJob,
    String,
    PathBuf,
);

pub(super) type VariantResults = Result<VariantResultFiles, String>;

/// One unfinished variant analysis and what reading its progress gave.
pub(super) type CodeqlRunPoll = (
    crate::codeql_submit::Submitted,
    Result<crate::codeql_submit::RunProgress, String>,
);

/// How often unfinished variant analyses are read again (#578).
pub(super) const CODEQL_VARIANT_POLL: std::time::Duration = std::time::Duration::from_secs(30);

/// How long after submitting a variant analysis croft follows it (#578).
pub(super) const CODEQL_VARIANT_FOLLOW: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 3600);

/// A GitHub Code Search in flight: the variant analysis list its
/// repositories go into, and its outcome: the repositories found, and why
/// a page after the first failed, cutting the results short.
pub(super) type CodeqlCodeSearch = (
    String,
    std::sync::mpsc::Receiver<Result<(Vec<String>, Option<String>), String>>,
);
