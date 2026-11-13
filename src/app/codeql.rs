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
