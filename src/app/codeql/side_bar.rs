//! The CodeQL side bar itself: whether it is available, revealing it, its keys, what
//! each row does when activated, and keeping the chrome in step when CodeQL is added,
//! removed, enabled or disabled.

use super::*;

impl App {
    /// Why CodeQL's side bar and commands are unavailable, or `None` when
    /// it is installed and enabled. It is not built in (#1842): it waits
    /// under AVAILABLE in the Extensions view until the user adds it.
    pub(in crate::app) fn codeql_unavailable(&self) -> Option<&'static str> {
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
    pub(in crate::app) fn open_codeql_view(&mut self) {
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
    pub(in crate::app) fn handle_codeql_key(&mut self, key: KeyEvent) {
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
    pub(in crate::app) fn activate_codeql(&mut self, hit: crate::widgets::codeql::Hit) {
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

    /// After a row went away, keep the selection on a row rather than on
    /// the welcome text that moved up under it.
    pub(in crate::app) fn keep_codeql_selection_on_a_row(&mut self) {
        let last = self.codeql.lines().len().saturating_sub(1);
        self.codeql.selected = self.codeql.selected.min(last);
        if self.codeql.selected_hit().is_none() {
            self.codeql.move_selection(false);
        }
    }
}

impl App {
    /// Keep the chrome in step once CodeQL is added, uninstalled, enabled or
    /// disabled: a CodeQL that is off takes its open side bar with it.
    pub(in crate::app) fn codeql_availability_changed(&mut self) {
        if !self.is_extension_enabled("codeql") && self.sidebar_view == SidebarView::CodeQL {
            self.set_sidebar_view(SidebarView::Explorer);
        }
        // The icon appears or disappears, so the baked bar redraws.
        self.overlays.activity.mark_dirty();
    }
}
