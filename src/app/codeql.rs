//! The CodeQL side bar: databases, query history and logs, running queries, suites and
//! packs, result viewers, AST and CFG, the model editor, the run queue, variant analysis,
//! and downloading and updating the CLI.

use super::*;

mod cli_install;
mod contextual;
mod database_sources;
mod databases;
mod history;
mod logs;
mod packs;
mod queries;
mod queue;
mod results;
mod run;
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
