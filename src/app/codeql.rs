//! The CodeQL side bar: databases, query history and logs, running queries, suites and
//! packs, result viewers, AST and CFG, the model editor, the run queue, variant analysis,
//! and downloading and updating the CLI.

use super::*;

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

    /// CodeQL: Run Tests (#578): every CodeQL test, through the Testing
    /// view's run-all, with the same `codeql` the side bar runs. Refused,
    /// with the reason, when the workspace's runner is not the CodeQL one.
    pub(super) fn run_codeql_tests(&mut self) {
        use crate::testing::worker::{Runner, runner_for};
        if runner_for(&self.extensions_dir(), &self.active_test_root) != Some(Runner::Codeql) {
            self.status = String::from(
                "No CodeQL tests here: a test pack's qlpack.yml declares tests: or extractor:",
            );
            return;
        }
        if self.refuse_test_run_while_busy() {
            return;
        }
        self.run_all_tests();
        self.status = String::from("Running CodeQL tests");
        self.await_test_run_outcome(String::from("CodeQL tests"));
    }

    /// CodeQL: Accept Test Output (#578): make a failing CodeQL test's
    /// actual results its expected ones, through `codeql test accept`. The
    /// test is the one the open file belongs to (its `.ql`, `.qlref`,
    /// `.expected` or `.actual`), else every CodeQL test the last run
    /// failed. Only tests a run left `.actual` output for are accepted.
    pub(super) fn accept_codeql_test_output(&mut self) {
        use crate::testing::codeqltest as ct;
        let tests: Vec<PathBuf> = match self.editor.path.as_deref().and_then(ct::test_for_file) {
            Some(test) => vec![test],
            None => self
                .testing
                .failed_names()
                .into_iter()
                .map(|id| self.active_test_root.join(ct::id_path(&id)))
                .filter(|t| t.is_file())
                .collect(),
        };
        if tests.is_empty() {
            self.status = String::from(
                "No CodeQL test to accept: open one of its files, or run the tests first",
            );
            return;
        }
        let ready: Vec<PathBuf> = tests
            .into_iter()
            .filter(|t| ct::actual_output(t).is_file())
            .collect();
        if ready.is_empty() {
            self.status = String::from(
                "No actual output to accept: run the test first, and accept it once it fails",
            );
            return;
        }
        let what = match ready.as_slice() {
            [one] => one
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            many => format!("{} tests", many.len()),
        };
        self.start_codeql_pack_job(
            ct::accept_args(&ready),
            format!("Accepting the output of {what}\u{2026}"),
            format!("Accepted the output of {what} as expected"),
            format!("Could not accept the output of {what}"),
        );
    }

    /// Point every CodeQL run at `program`: the side bar's queries and,
    /// through the test worker, the Testing view's CodeQL tests (#578).
    /// The one place the configured `codeql` changes, so the two never
    /// disagree.
    pub(super) fn set_codeql_program(&mut self, program: PathBuf) {
        self.test_worker.set_codeql_program(program.clone());
        self.codeql_program = program;
    }

    /// Pick the `codeql` to run from settings, `PATH` and croft's managed
    /// copies (#578), at startup and when the setting changes.
    pub(super) fn resolve_codeql_cli(&mut self) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let resolved = crate::codeql_cli::resolve(
            self.codeql_cli_setting.as_deref(),
            home.as_deref(),
            std::env::var_os("PATH").as_deref(),
            &croft_cache_dir(),
        );
        self.codeql_cli_source = resolved.source;
        self.set_codeql_program(resolved.program);
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

    /// The four places a CodeQL database comes from, in a picker whose rows
    /// run the matching side-bar action.
    pub(super) fn open_codeql_database_sources(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows = CODEQL_DB_SOURCES
            .iter()
            .enumerate()
            .map(|(i, (_, label))| ListRow {
                id: i.to_string(),
                label: String::from(*label),
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeqlDbSource, "Add CodeQL Database", rows),
            "",
        );
    }

    /// The databases, under the side bar's language, in a picker whose
    /// choice becomes the current one (#578): the database card's switch.
    pub(super) fn open_codeql_database_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows: Vec<ListRow> = self
            .codeql
            .database_choices()
            .into_iter()
            .map(|(i, label)| ListRow {
                id: i.to_string(),
                label,
            })
            .collect();
        if rows.is_empty() {
            self.status = String::from("No CodeQL databases in this language");
            return;
        }
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeqlDatabase, "Select CodeQL Database", rows),
            "",
        );
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

    /// VS Code's "CodeQL: Quick Query" (#578): open a scratch query for the
    /// selected database's language, kept in croft's cache so it outlives
    /// the session. The first time, its pack is written; until `codeql pack
    /// install` has left a lock file, its library dependency is installed.
    pub(super) fn open_codeql_quick_query(&mut self) {
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.current.and_then(|i| store.databases.get(i)) else {
            self.status = String::from("Add a CodeQL database and select it first");
            return;
        };
        let Some(module) = db
            .language
            .as_deref()
            .and_then(crate::codeql_query::language_module)
        else {
            self.status = format!("CodeQL database {} has no language croft knows", db.name);
            return;
        };
        let dir = croft_cache_dir()
            .join("codeql")
            .join("quick-query")
            .join(module);
        let query = dir.join("quick-query.ql");
        let fresh = !query.is_file();
        if fresh {
            let (pack, source) = crate::codeql_query::quick_query(module);
            let written = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(dir.join("qlpack.yml"), pack))
                .and_then(|()| std::fs::write(&query, source));
            if let Err(e) = written {
                self.status = format!("Could not create the quick query: {e}");
                return;
            }
        }
        match self.editor.open(&query) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
            }
            Err(e) => {
                self.status = format!("{}: {e}", query.display());
                return;
            }
        }
        if !dir.join("codeql-pack.lock.yml").is_file() {
            self.start_codeql_pack_job(
                crate::codeql_query::pack_install_args(&dir),
                format!("Installing codeql/{module}-all for the quick query\u{2026}"),
                format!("Quick query ready: codeql/{module}-all is installed"),
                format!("Could not install codeql/{module}-all for the quick query"),
            );
        } else {
            self.status = format!("Opened the {module} quick query");
        }
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

    /// The store index of the current database, for the palette commands
    /// that act on it; says what to do when there is none.
    pub(super) fn current_codeql_database(&mut self) -> Option<usize> {
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let current = store.current.filter(|&i| i < store.databases.len());
        if current.is_none() {
            self.status = String::from("Select a CodeQL database first");
        }
        current
    }

    /// Ask before removing database `index` (#578), saying whether its
    /// files go too: only a copy croft made in its own cache is deleted.
    pub(super) fn confirm_remove_codeql_database(&mut self, index: usize) {
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
    pub(super) fn codeql_cached_copy(path: &Path) -> Option<PathBuf> {
        Self::cache_entry_holding(&Self::codeql_db_cache_dir(), path)
    }

    /// The entry directly under `cache` that holds `path`, both
    /// canonicalised; `None` when `path` is elsewhere, is `cache` itself, or
    /// either does not exist.
    pub(super) fn cache_entry_holding(cache: &Path, path: &Path) -> Option<PathBuf> {
        let cache = cache.canonicalize().ok()?;
        let path = path.canonicalize().ok()?;
        let first = path.strip_prefix(&cache).ok()?.components().next()?;
        Some(cache.join(first))
    }

    /// Remove the database at `path` from the list, deleting its files when
    /// croft made them, and save.
    pub(super) fn perform_remove_codeql_database(&mut self, path: &Path) {
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

    pub(super) fn prompt_rename_codeql_database(&mut self, index: usize) {
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

    pub(super) fn submit_rename_codeql_database(&mut self, path: &Path, value: &str) {
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
    pub(super) fn sort_codeql_databases(&mut self) {
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
    pub(super) fn reveal_codeql_database(&mut self, index: usize) {
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
    pub(super) fn upgrade_codeql_database(&mut self, index: usize) {
        self.start_codeql_db_job(index, crate::codeql_query::DbJob::Upgrade);
    }

    /// Run `job` on database `index` on a worker thread (#578): an upgrade,
    /// or VS Code's "CodeQL: Clear Cache", "Trim Cache" and "Trim Cache to
    /// Overlay Base". One at a time, never beside a query run, since each
    /// rewrites the database a query would read.
    pub(super) fn start_codeql_db_job(&mut self, index: usize, job: crate::codeql_query::DbJob) {
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

    /// Ask the CodeQL CLI for its version on a worker thread (#578, VS
    /// Code's "CodeQL: Copy Version Information");
    /// [`Self::drain_codeql_version`] copies the report.
    pub(super) fn copy_codeql_version(&mut self) {
        if self.codeql_version.is_some() {
            return;
        }
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::read_codeql_version(&program));
        });
        self.status = String::from("Reading the CodeQL CLI version\u{2026}");
        self.codeql_version = Some(rx);
    }

    /// Copy the version report once the CLI has answered (#578). A missing
    /// CLI still copies croft's version and the platform, saying why the
    /// CLI's could not be read.
    pub fn drain_codeql_version(&mut self) -> bool {
        let Some(rx) = self.codeql_version.as_ref() else {
            return false;
        };
        let cli = match rx.try_recv() {
            Ok(cli) => cli,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the version check stopped unexpectedly"))
            }
        };
        self.codeql_version = None;
        let text = crate::codeql_query::version_information(env!("CARGO_PKG_VERSION"), &cli);
        copy_to_clipboard(&text);
        self.status = match cli {
            Ok(_) => String::from("Copied CodeQL version information"),
            Err(why) => format!("Copied version information; the CodeQL CLI is unavailable: {why}"),
        };
        true
    }

    /// `program`'s bare version number, or why it could not be read.
    pub(super) fn read_codeql_version(program: &std::path::Path) -> Result<String, String> {
        let out = crate::review_ops::output_retrying_busy(
            std::process::Command::new(program).args(crate::codeql_query::version_args()),
        );
        match out {
            Ok(o) if o.status.success() => {
                Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("codeql failed")
                .to_string()),
            Err(e) => Err(format!("could not run codeql: {e}")),
        }
    }

    /// Download the newest known CodeQL CLI release into croft's cache on a
    /// worker thread (#578, the VS Code extension's managed CLI);
    /// [`Self::drain_codeql_cli_download`] switches to it.
    pub(super) fn download_codeql_cli(&mut self) {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let Some(asset) = crate::codeql_cli::asset_name(os, arch) else {
            self.status = format!(
                "GitHub publishes no CodeQL CLI for {os} on {arch}; install one and set codeql_cli_path"
            );
            return;
        };
        self.start_codeql_cli_download(asset);
    }

    pub(super) fn start_codeql_cli_download(&mut self, asset: &'static str) {
        use crate::codeql_cli::JobUpdate;
        if self.codeql_cli_download.is_some() {
            self.status = String::from("The CodeQL CLI is already downloading");
            return;
        }
        let version =
            crate::codeql_cli::install_version(self.codeql_cli_latest.as_deref()).to_string();
        let root = crate::codeql_cli::cli_root(&croft_cache_dir());
        let net = self.codeql_cli_net.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.status = format!("Downloading CodeQL CLI v{version}\u{2026}");
        std::thread::spawn(move || {
            let send = |update| {
                let _ = tx.send(update);
            };
            let out = crate::codeql_cli::download_and_install(&net, &root, &version, asset, &send);
            send(JobUpdate::Done(out.map(|program| (version, program))));
        });
        self.codeql_cli_download = Some(rx);
    }

    /// Show the CLI download's progress, and switch to the CLI once it is
    /// installed (#578), unless settings name another: that one always wins.
    pub fn drain_codeql_cli_download(&mut self) -> bool {
        use crate::codeql_cli::JobUpdate;
        use std::sync::mpsc::TryRecvError;
        let Some(rx) = self.codeql_cli_download.take() else {
            return false;
        };
        let mut changed = false;
        let outcome = loop {
            match rx.try_recv() {
                Ok(JobUpdate::Progress(line)) => {
                    self.status = line;
                    changed = true;
                }
                Ok(JobUpdate::Done(outcome)) => break outcome,
                Err(TryRecvError::Empty) => {
                    self.codeql_cli_download = Some(rx);
                    return changed;
                }
                Err(TryRecvError::Disconnected) => {
                    break Err(String::from("the download stopped unexpectedly"));
                }
            }
        };
        self.status = match outcome {
            Ok((version, _)) if self.codeql_cli_source == crate::codeql_cli::Source::Settings => {
                format!(
                    "Installed CodeQL CLI v{version}; codeql_cli_path in settings still takes precedence"
                )
            }
            Ok((version, program)) => {
                self.set_codeql_program(program);
                let status = format!("Installed CodeQL CLI v{version} and switched to it");
                self.codeql_cli_source = crate::codeql_cli::Source::Managed(version);
                status
            }
            Err(why) => format!("Could not install the CodeQL CLI: {why}"),
        };
        true
    }

    /// Look up the newest CodeQL CLI release and the version in use on a
    /// worker thread (#578, VS Code's "CodeQL: Check for CLI Updates");
    /// [`Self::drain_codeql_cli_check`] compares them.
    pub(super) fn check_codeql_cli_updates(&mut self) {
        if self.codeql_cli_check.is_some() {
            return;
        }
        // A managed copy's folder names its version; any other CLI is asked.
        let known = match &self.codeql_cli_source {
            crate::codeql_cli::Source::Managed(v) => Some(v.clone()),
            _ => None,
        };
        let program = self.codeql_program.clone();
        let latest = self.codeql_cli_net.latest.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let current = known.or_else(|| Self::read_codeql_version(&program).ok());
            let _ = tx.send((latest(), current));
        });
        self.status = String::from("Checking for CodeQL CLI updates\u{2026}");
        self.codeql_cli_check = Some(rx);
    }

    /// Report the update check (#578), remembering the latest release for
    /// "CodeQL: Download CLI".
    pub fn drain_codeql_cli_check(&mut self) -> bool {
        let Some(rx) = self.codeql_cli_check.as_ref() else {
            return false;
        };
        let (latest, current) = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                (Err(String::from("the check stopped unexpectedly")), None)
            }
        };
        self.codeql_cli_check = None;
        self.status = match latest {
            Ok(latest) => {
                let status = crate::codeql_cli::update_status(&latest, current.as_deref());
                self.codeql_cli_latest = Some(latest);
                status
            }
            Err(why) => format!("Could not check for CodeQL CLI updates: {why}"),
        };
        true
    }

    /// Add database `index`'s source to the workspace (#578, VS Code's
    /// "CodeQL: Add Database Source to Workspace"): its `src` folder, else
    /// its `src.zip`, extracted once into croft's cache.
    pub(super) fn add_codeql_database_source(&mut self, index: usize) {
        use crate::codeql_db::DbSource;
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.databases.get(index) else {
            return;
        };
        let folder = match crate::codeql_db::database_source(&db.path) {
            None => {
                self.status = format!("CodeQL database {} has no src folder or src.zip", db.name);
                return;
            }
            Some(DbSource::Folder(folder)) => folder,
            Some(DbSource::Zip(zip)) => {
                let cache = Self::codeql_source_cache_dir();
                let dest = cache.join(crate::codeql_db::source_cache_name(&db.path, &zip));
                if !dest.is_dir() {
                    // An older extraction of this database's source is out
                    // of date: its archive changed, which is why the name
                    // no longer matches. Drop it, unless it is open as a
                    // workspace root.
                    let prefix = crate::codeql_db::source_cache_prefix(&db.path);
                    for old in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
                        let path = old.path();
                        let stale = old.file_name().to_string_lossy().starts_with(&prefix);
                        let open = path
                            .canonicalize()
                            .is_ok_and(|p| self.roots.iter().any(|r| r == p));
                        if stale && !open {
                            let _ = std::fs::remove_dir_all(&path);
                        }
                    }
                    // Extract beside it and rename, so an interrupted
                    // extraction never passes for a finished one.
                    let partial = PathBuf::from(format!("{}.partial", dest.display()));
                    let _ = std::fs::remove_dir_all(&partial);
                    let done = crate::codeql_db::extract_zip(
                        &zip,
                        &partial,
                        Some(crate::codeql_db::SOURCE_ZIP_LIMIT),
                    )
                    .and_then(|_| std::fs::rename(&partial, &dest).map_err(|e| e.to_string()));
                    if let Err(e) = done {
                        let _ = std::fs::remove_dir_all(&partial);
                        self.status = format!("Could not extract {}: {e}", zip.display());
                        return;
                    }
                }
                dest
            }
        };
        let folder = folder.canonicalize().unwrap_or(folder);
        let name = db.name.clone();
        if self.roots.iter().any(|r| r == folder) {
            self.status =
                format!("The source of CodeQL database {name} is already in the workspace");
            return;
        }
        self.add_workspace_folder(folder.clone());
        if self.roots.iter().any(|r| r == folder) {
            self.status = format!("Added the source of CodeQL database {name} to the workspace");
        }
    }

    /// The databases "Delete Unused Databases" removes: copies in croft's
    /// cache that no query history entry ran on, never the current one or
    /// one being upgraded. A database the user pointed at elsewhere is
    /// theirs and never listed.
    pub(super) fn unused_codeql_databases(&self) -> Vec<PathBuf> {
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
    pub(super) fn confirm_delete_unused_codeql_databases(&mut self) {
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
    pub(super) fn perform_delete_unused_codeql_databases(&mut self, paths: &[PathBuf]) {
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

    pub(super) fn codeql_db_store_path() -> PathBuf {
        croft_cache_dir().join("codeql-databases.json")
    }

    /// Where fetched or extracted databases live: croft's cache, never
    /// beside the archive the user pointed at.
    pub(super) fn codeql_db_cache_dir() -> PathBuf {
        croft_cache_dir().join("codeql").join("databases")
    }

    /// Where a database's `src.zip` is extracted to be browsed, one folder
    /// per database, beside the database cache.
    pub(super) fn codeql_source_cache_dir() -> PathBuf {
        croft_cache_dir().join("codeql").join("sources")
    }

    pub(super) fn codeql_history_path() -> PathBuf {
        croft_cache_dir().join("codeql-history.json")
    }

    /// Where query runs write their results, one folder per run.
    pub(super) fn codeql_results_dir() -> PathBuf {
        croft_cache_dir().join("codeql").join("results")
    }

    /// Mirror the saved query history into the side bar.
    pub(super) fn refresh_codeql_history(&mut self) {
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
    pub(super) fn current_codeql_history(&mut self) -> Option<usize> {
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
    pub(super) fn codeql_cached_results(output: &Path) -> Option<PathBuf> {
        Self::cache_entry_holding(&Self::codeql_results_dir(), output.parent()?)
    }

    /// Ask before removing history entry `index` (#578), saying whether its
    /// results go too. A run still in flight is refused: its worker would
    /// write the results back.
    pub(super) fn confirm_remove_codeql_history(&mut self, index: usize) {
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
    pub(super) fn perform_remove_codeql_history(&mut self, output: &Path) {
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

    pub(super) fn prompt_rename_codeql_history(&mut self, index: usize) {
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

    pub(super) fn submit_rename_codeql_history(&mut self, output: &Path, value: &str) {
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
    pub(super) fn sort_codeql_history(&mut self) {
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
    pub(super) fn view_codeql_history_query(&mut self, index: usize) {
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
    pub(super) fn view_codeql_history_query_text(&mut self, index: usize) {
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
    pub(super) fn codeql_history_log(&mut self, index: usize) -> Option<PathBuf> {
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

    /// VS Code's "Show Evaluator Log (Raw JSON)" for history entry `index`.
    pub(super) fn show_codeql_evaluator_log(&mut self, index: usize) {
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
    pub(super) fn show_codeql_query_log(&mut self, index: usize) {
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
    pub(super) fn show_codeql_evaluator_log_summary(&mut self, index: usize) {
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
    pub(super) fn codeql_log_predicates(
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
    pub(super) fn show_codeql_evaluator_log_viewer(&mut self, index: usize) {
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
    pub(super) fn compare_codeql_performance(&mut self, index: usize) {
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

    /// VS Code's "Export Results" (#578): ask where to copy the results of
    /// the selected history entry, else of the newest successful run,
    /// suggesting `<query-stem>-results.<csv|sarif>` in the workspace.
    pub(super) fn prompt_export_codeql_results(&mut self) {
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
    pub(super) fn submit_export_codeql_results(&mut self, output: &Path, value: &str) {
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

    /// VS Code's "CodeQL: Preview Query Help" (#578): the help of the open
    /// `.ql` query, else of the query the selected or newest history entry
    /// ran, rendered to Markdown by `codeql generate query-help` on a worker
    /// thread and shown in the Markdown preview.
    pub(super) fn preview_codeql_query_help(&mut self) {
        let query = match self.editor.path.clone() {
            Some(p) if p.extension().is_some_and(|e| e == "ql") => p,
            _ => {
                let Some(i) = self.current_codeql_history() else {
                    self.status = String::from("Open a .ql query to preview its help");
                    return;
                };
                let history = crate::codeql_query::History::load(&Self::codeql_history_path());
                let Some(query) = history.entries.get(i).map(|e| e.query.clone()) else {
                    return;
                };
                query
            }
        };
        if self.codeql_query_help.is_some() {
            self.status = String::from("Query help is already being rendered");
            return;
        }
        let stem = query
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = query
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let help = croft_cache_dir()
            .join("codeql")
            .join("help")
            .join(format!("{stem}.md"));
        let program = self.codeql_program.clone();
        let args = crate::codeql_query::query_help_args(&query, &help);
        let out = help.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // An earlier preview's file must not stand in for help the CLI
            // did not write this time.
            let _ = std::fs::remove_file(&out);
            let outcome = out
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .map_err(|e| format!("{}: {e}", out.display()))
                .and_then(|()| Self::codeql_command(&program, &args));
            let _ = tx.send(outcome);
        });
        self.status = format!("Rendering the help of {name}\u{2026}");
        self.codeql_query_help = Some((rx, help, name));
    }

    /// Preview rendered query help (#578), or say the query has none: the
    /// CLI fails, or writes nothing, when there is no `.qhelp` or `.md`
    /// beside the query.
    pub fn drain_codeql_query_help(&mut self) -> bool {
        let Some((rx, _, _)) = self.codeql_query_help.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the help rendering stopped unexpectedly"))
            }
        };
        let Some((_, help, name)) = self.codeql_query_help.take() else {
            return false;
        };
        let written = std::fs::metadata(&help).is_ok_and(|m| m.len() > 0);
        match outcome {
            Err(why) => self.status = format!("{name} has no query help: {why}"),
            Ok(()) if !written => {
                self.status = format!("{name} has no query help (no .qhelp or .md beside it)");
            }
            Ok(()) => match self.editor.open(&help) {
                Ok(()) => {
                    self.sync_open_file_poll_mtime();
                    if self.editor.markdown_preview.is_none() {
                        self.editor.toggle_markdown_preview();
                    }
                    self.focus_pane(Pane::Editor);
                    self.status = format!("Query help for {name}");
                }
                Err(e) => self.status = format!("{}: {e}", help.display()),
            },
        }
        true
    }

    /// VS Code's "Compare Results" for history entry `index` (#578): its
    /// table against the latest earlier run of the same query, written
    /// beside its results as a CSV of the rows only one run has, and opened.
    pub(super) fn compare_codeql_results(&mut self, index: usize) {
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

    /// "CodeQL: Open Query Results Directory" (#578): the folder of the
    /// Query History run selected in the side bar, else of the newest run.
    pub(super) fn open_codeql_results_directory(&mut self) {
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
    pub(super) fn open_codeql_history_results_dir(&mut self, index: usize) {
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

    /// Find the workspace's queries for the side bar's Queries section now,
    /// for a caller that needs the list at once (a query just created, Run
    /// All Queries). It walks the tree, so opening the view does this off
    /// the UI thread instead. A walk still in flight is dropped: it may
    /// predate what this one finds.
    pub(super) fn refresh_codeql_queries(&mut self) {
        self.codeql_queries_job = None;
        self.codeql.discovering_queries = false;
        self.codeql.queries = crate::codeql_query::discover(self.workspace_root());
    }

    /// Find the workspace's queries on a worker thread (#840): the walk can
    /// take seconds on a large tree, and switching to the CodeQL view must
    /// not wait for it. [`Self::drain_codeql_queries`] lists what it finds;
    /// until then the section keeps its last list, or says it is looking.
    /// A walk of this root already in flight is left to finish rather than
    /// doubled; one of a root the workspace has since left is replaced.
    pub(super) fn discover_codeql_queries(&mut self) {
        let root = self.workspace_root().to_path_buf();
        if self
            .codeql_queries_job
            .as_ref()
            .is_some_and(|(walked, _)| *walked == root)
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let walk = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::codeql_query::discover(&walk));
        });
        self.codeql_queries_job = Some((root, rx));
        self.codeql.discovering_queries = true;
    }

    /// List the queries a discovery found, once it lands (#840). True when
    /// the side bar changed. A walk of a root the workspace has left since
    /// is dropped unread, landed or not, and the view, if showing, walks
    /// the root it shows now.
    pub fn drain_codeql_queries(&mut self) -> bool {
        let Some((walked, rx)) = self.codeql_queries_job.as_ref() else {
            return false;
        };
        if walked.as_path() != self.workspace_root() {
            self.codeql_queries_job = None;
            self.codeql.discovering_queries = false;
            if self.sidebar_view == SidebarView::CodeQL {
                self.discover_codeql_queries();
            }
            return true;
        }
        match rx.try_recv() {
            Ok(queries) => self.codeql.set_queries(queries),
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            // The walk died: stop saying it is looking.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.codeql.discovering_queries = false;
            }
        }
        self.codeql_queries_job = None;
        true
    }

    /// Ask for the name of a new query (#578, VS Code's "CodeQL: Create
    /// Query"). It goes into the pack whose line or query row is selected in
    /// the Queries section, else the workspace root, and is written in the
    /// pack's language, else the Language section's, else none.
    pub(super) fn prompt_create_codeql_query(&mut self) {
        use crate::widgets::codeql::LANGUAGE_IDS;
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let pack = self
            .codeql
            .selected_pack()
            .and_then(|p| self.codeql.queries.get(p));
        let root = self.workspace_root().to_path_buf();
        let dir = pack.map_or_else(|| root.clone(), |p| p.dir.clone());
        let language = pack
            .and_then(|p| p.language.clone())
            .or_else(|| self.codeql.language.map(|i| LANGUAGE_IDS[i].to_string()));
        let place = match pack {
            Some(p) if p.name != crate::codeql_query::NO_PACK => p.name.clone(),
            _ => String::from("the workspace root"),
        };
        let lang = language
            .as_deref()
            .map(|l| format!(" ({l})"))
            .unwrap_or_default();
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlCreateQuery { dir, language },
            format!("Create CodeQL Query in {place}{lang}"),
            "query name, e.g. find-unsafe-calls",
        ));
    }

    /// Write the new query, open it and list it in the Queries section;
    /// a refused name or an existing file is said on the status line.
    pub(super) fn submit_create_codeql_query(
        &mut self,
        dir: &Path,
        language: Option<&str>,
        name: &str,
    ) {
        let root = self.workspace_root().to_path_buf();
        let path = match crate::codeql_query::scaffold_query(&root, dir, name, language) {
            Ok(path) => path,
            Err(e) => {
                self.status = format!("Could not create the query: {e}");
                return;
            }
        };
        self.refresh_codeql_queries();
        match self.editor.open(&path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
                let shown = path.strip_prefix(&root).unwrap_or(&path);
                self.status = format!("Created CodeQL query {}", shown.display());
            }
            Err(e) => self.status = format!("{}: {e}", path.display()),
        }
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
                copy_to_clipboard(&list);
                format!(
                    "Copied the {} repositories of variant analysis {}",
                    list.lines().count(),
                    run.id
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

    /// Walk a CodeQL results table (#578, VS Code's `codeQLQueryResults`
    /// up/down/left/right): move its cursor by (`rows`, `cols`) and open
    /// the code of the cell's entity, else of the first entity in its row.
    /// The table is the active results sheet, else the one walked last,
    /// so the walk goes on once the code has the focus.
    pub(super) fn walk_codeql_results(&mut self, rows: isize, cols: isize) {
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
    pub(super) fn codeql_source_file(&self, path: &Path) -> Option<PathBuf> {
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
    pub(super) fn codeql_decode_sets(
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
    pub(super) fn show_codeql_result_sets(&mut self) {
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

    /// CodeQL: Open Referenced File (#578): the query the open `.qlref`
    /// test file names, as `codeql resolve qlref` finds it through the
    /// test pack's dependencies.
    pub(super) fn open_codeql_referenced_file(&mut self) {
        let Some(qlref) = self
            .editor
            .path
            .clone()
            .filter(|p| p.extension().is_some_and(|e| e == "qlref"))
        else {
            self.status = String::from("Open a .qlref file first");
            return;
        };
        if self.codeql_doc_job.is_some() {
            self.status = String::from("A CodeQL view is already being made");
            return;
        }
        let name = qlref
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let found = Self::codeql_stdout(&program, &crate::codeql_query::qlref_args(&qlref))
                .and_then(|json| {
                    crate::codeql_query::parse_qlref(&json)
                        .ok_or_else(|| String::from("the CLI named no query"))
                });
            let _ = tx.send(found);
        });
        self.status = format!("Resolving {name}\u{2026}");
        self.codeql_doc_job = Some((
            rx,
            format!("The query {name} references"),
            format!("Could not resolve {name}"),
        ));
    }

    /// The Query History run selected in the side bar, else the newest;
    /// says so when nothing has run.
    pub(super) fn codeql_chosen_run(&mut self) -> Option<crate::codeql_query::HistoryEntry> {
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
    pub(super) fn codeql_run_database(
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
    pub(super) fn view_codeql_alerts(&mut self, csv: bool) {
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
    pub(super) fn view_codeql_raw_results(&mut self) {
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

    /// Mirror the saved database list into the side bar.
    pub(super) fn refresh_codeql_databases(&mut self) {
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

    /// Add a CodeQL database from `value` (#578): a folder, an archive, a
    /// URL to a zip, or `owner/repo [language]` on GitHub.
    pub fn submit_codeql_database(
        &mut self,
        source: crate::widgets::input_prompt::CodeqlDbSource,
        value: &str,
    ) {
        use crate::widgets::input_prompt::CodeqlDbSource;
        if source == CodeqlDbSource::Folder {
            let dir = self.typed_path(value);
            match crate::codeql_db::find_database_in(&dir) {
                Some(found) => self.register_codeql_database(&found),
                None => {
                    self.status = format!(
                        "{} is not a CodeQL database (no codeql-database.yml)",
                        dir.display()
                    );
                }
            }
            return;
        }
        // Downloads and extraction run on a worker (#1155): a database is
        // commonly hundreds of megabytes, and inline the whole UI froze until
        // it was on disk.
        if let Some((what, _)) = &self.codeql_db_job {
            self.status = format!("Still adding the CodeQL database {what}");
            return;
        }
        let cache = Self::codeql_db_cache_dir();
        let (what, job): (String, Box<dyn FnOnce() -> Result<PathBuf, String> + Send>) =
            match source {
                CodeqlDbSource::Archive => {
                    let zip = self.typed_path(value);
                    let stem = zip
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| String::from("database"));
                    (
                        zip.display().to_string(),
                        Box::new(move || Self::extract_codeql_zip(&cache, &zip, &stem)),
                    )
                }
                CodeqlDbSource::Url => {
                    let url = value.trim().to_string();
                    (
                        url.clone(),
                        Box::new(move || Self::download_codeql_zip_from_url(&cache, &url)),
                    )
                }
                CodeqlDbSource::Github => {
                    let repo = value.trim().to_string();
                    let gh = self.gh_program.clone();
                    (
                        repo.clone(),
                        Box::new(move || Self::download_codeql_db_from_github(&gh, &cache, &repo)),
                    )
                }
                CodeqlDbSource::Folder => unreachable!("added above"),
            };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
        self.status = format!("Adding the CodeQL database {what}\u{2026}");
        self.codeql_db_job = Some((what, rx));
    }

    /// Register a database that finished downloading or extracting (#1155).
    pub fn drain_codeql_db_job(&mut self) -> bool {
        let Some((what, rx)) = self.codeql_db_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(format!(
                "Adding the CodeQL database {what} stopped unexpectedly"
            )),
        };
        self.codeql_db_job = None;
        match outcome {
            Ok(dir) => self.register_codeql_database(&dir),
            Err(why) => self.status = why,
        }
        true
    }

    /// Add a database folder to the saved list and make it current.
    pub(super) fn register_codeql_database(&mut self, dir: &std::path::Path) {
        let path = Self::codeql_db_store_path();
        let mut store = crate::codeql_db::DatabaseStore::load(&path);
        match store.add(dir) {
            Ok(i) => {
                if let Err(e) = store.save(&path) {
                    self.status = format!("Could not save the database list: {e}");
                    return;
                }
                let db = &store.databases[i];
                self.status = format!(
                    "Added CodeQL database {}{}",
                    db.name,
                    db.language
                        .as_deref()
                        .map(|l| format!(" ({l})"))
                        .unwrap_or_default()
                );
                self.refresh_codeql_databases();
            }
            Err(why) => self.status = why,
        }
    }

    /// Unpack `zip` into `cache/<name>`. Runs on the add-database worker.
    pub(super) fn extract_codeql_zip(
        cache: &std::path::Path,
        zip: &std::path::Path,
        name: &str,
    ) -> Result<PathBuf, String> {
        let dest = cache.join(name);
        crate::codeql_db::extract_zip(zip, &dest, None)?;
        crate::codeql_db::find_database_in(&dest)
            .ok_or_else(|| format!("{} holds no CodeQL database", zip.display()))
    }

    pub(super) fn download_codeql_zip_from_url(
        cache: &std::path::Path,
        url: &str,
    ) -> Result<PathBuf, String> {
        if !url.starts_with("https://") {
            return Err(String::from("A database URL must be https://"));
        }
        let resp = crate::net::get(url)
            .timeout(std::time::Duration::from_secs(300))
            .call()
            .map_err(|e| format!("Download failed: {e}"))?;
        let name = url
            .rsplit('/')
            .next()
            .unwrap_or("database")
            .trim_end_matches(".zip")
            .to_string();
        let tmp = cache.join(format!("{name}.download.zip"));
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        std::io::copy(&mut resp.into_reader(), &mut f).map_err(|e| e.to_string())?;
        let out = Self::extract_codeql_zip(cache, &tmp, &name);
        let _ = std::fs::remove_file(&tmp);
        out
    }

    /// `owner/repo` or `owner/repo language`. With no language, the one
    /// database GitHub has is used; several need the language named.
    pub(super) fn download_codeql_db_from_github(
        gh: &std::path::Path,
        cache: &std::path::Path,
        value: &str,
    ) -> Result<PathBuf, String> {
        let mut parts = value.split_whitespace();
        let repo = parts
            .next()
            .map(|r| {
                r.trim_start_matches("https://github.com/")
                    .trim_end_matches('/')
            })
            .filter(|r| r.split('/').count() == 2)
            .ok_or_else(|| {
                String::from("Give the repository as owner/repo (and optionally a language)")
            })?
            .to_string();
        let language = match parts.next() {
            Some(l) => l.to_string(),
            None => {
                let mut cmd = std::process::Command::new(gh);
                cmd.args([
                    "api",
                    &format!("/repos/{repo}/code-scanning/codeql/databases"),
                    "--jq",
                    ".[].language",
                ]);
                let out = crate::review_ops::output_retrying_busy(&mut cmd).map_err(|e| {
                    format!(
                        "Could not look up {repo}'s CodeQL databases: {}",
                        crate::gh::spawn_error(&e, None)
                    )
                })?;
                if crate::gh::signed_out(out.status) {
                    return Err(format!(
                        "Could not look up {repo}'s CodeQL databases: {}",
                        crate::gh::SIGNED_OUT
                    ));
                }
                if !out.status.success() {
                    return Err(format!(
                        "{repo} has no CodeQL databases on GitHub: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                let langs: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect();
                match langs.as_slice() {
                    [one] => one.clone(),
                    [] => return Err(format!("{repo} has no CodeQL databases on GitHub")),
                    many => {
                        return Err(format!(
                            "{repo} has databases for {}; add one with '{repo} <language>'",
                            many.join(", ")
                        ));
                    }
                }
            }
        };
        // Streamed straight to disk: a database can be larger than the
        // memory croft should hold at once.
        let name = format!("{}-{language}", repo.replace('/', "-"));
        let tmp = cache.join(format!("{name}.download.zip"));
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut cmd = std::process::Command::new(gh);
        cmd.args(crate::codeql_db::github_database_args(&repo, &language))
            .stdin(std::process::Stdio::null())
            .stdout(file)
            .stderr(std::process::Stdio::piped());
        let out = crate::review_ops::spawn_retrying_busy(&mut cmd)
            .and_then(std::process::Child::wait_with_output)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("Download failed: {}", crate::gh::spawn_error(&e, None))
            })?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            let why = if crate::gh::signed_out(out.status) {
                std::borrow::Cow::Borrowed(crate::gh::SIGNED_OUT)
            } else {
                String::from_utf8_lossy(&out.stderr)
            };
            return Err(format!("Download failed: {}", why.trim()));
        }
        let result = Self::extract_codeql_zip(cache, &tmp, &name);
        let _ = std::fs::remove_file(&tmp);
        result
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
