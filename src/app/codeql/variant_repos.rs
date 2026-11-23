//! What variant analysis runs against: its config, the controller repository, lists,
//! repositories and owners, adding repositories from a Code Search, and opening the
//! selected one on GitHub.

use super::*;

impl App {
    pub(in crate::app) fn codeql_variant_path() -> PathBuf {
        croft_cache_dir().join("codeql-variant-analysis.json")
    }

    /// Reload the variant analysis config into the side bar. One that
    /// cannot be read is reported, and the section says so rather than
    /// showing it empty.
    pub(in crate::app) fn refresh_codeql_variant(&mut self) {
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
    pub(in crate::app) fn edit_codeql_variant(
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
    pub(in crate::app) fn prompt_codeql_controller(&mut self) {
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

    pub(in crate::app) fn submit_codeql_controller(&mut self, value: &str) {
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
    pub(in crate::app) fn prompt_add_codeql_variant_repo(&mut self) {
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

    pub(in crate::app) fn submit_add_codeql_variant_repo(
        &mut self,
        list: Option<&str>,
        value: &str,
    ) {
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
    pub(in crate::app) fn prompt_codeql_variant_code_search(&mut self) {
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
    pub(in crate::app) fn codeql_search_language(&self) -> Option<&'static str> {
        self.codeql
            .language
            .and_then(|i| crate::widgets::codeql::LANGUAGE_IDS.get(i).copied())
    }

    /// Run the Code Search off the UI thread; [`Self::drain_codeql_code_search`]
    /// adds what it finds to `list`.
    pub(in crate::app) fn start_codeql_variant_code_search(&mut self, list: String, value: &str) {
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

    pub(in crate::app) fn prompt_add_codeql_variant_list(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlAddVariantList,
            String::from("Add Repository List"),
            "the list's name",
        ));
    }

    pub(in crate::app) fn submit_add_codeql_variant_list(&mut self, value: &str) {
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

    pub(in crate::app) fn prompt_add_codeql_variant_owner(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlAddVariantOwner,
            String::from("Add Variant Analysis Owner"),
            "a user or organisation, or its GitHub URL",
        ));
    }

    pub(in crate::app) fn submit_add_codeql_variant_owner(&mut self, value: &str) {
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

    pub(in crate::app) fn prompt_rename_codeql_variant_list(&mut self, index: usize) {
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

    pub(in crate::app) fn submit_rename_codeql_variant_list(&mut self, name: &str, value: &str) {
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
    pub(in crate::app) fn remove_codeql_variant_item(&mut self, item: crate::codeql_variant::Item) {
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

    pub(in crate::app) fn perform_remove_codeql_variant_list(&mut self, name: &str) {
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

    /// Make `item` what a variant analysis runs against (VS Code's "Select").
    pub(in crate::app) fn select_codeql_variant_item(&mut self, item: crate::codeql_variant::Item) {
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
    pub(in crate::app) fn open_codeql_variant_config(&mut self) {
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
    pub(in crate::app) fn codeql_variant_github_url(&self) -> Option<String> {
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
    pub(in crate::app) fn open_codeql_variant_on_github(&mut self) {
        let Some(url) = self.codeql_variant_github_url() else {
            self.status = String::from(
                "Select a variant analysis repository or owner in the CodeQL side bar first",
            );
            return;
        };
        self.open_url_for_user(&url);
    }
}
