//! The workspace's queries: finding them for the side bar on a worker thread, creating
//! one, a quick query, the query a `.qlref` points at, and previewing a query's help.

use super::*;

impl App {
    /// VS Code's "CodeQL: Quick Query" (#578): open a scratch query for the
    /// selected database's language, kept in croft's cache so it outlives
    /// the session. The first time, its pack is written; until `codeql pack
    /// install` has left a lock file, its library dependency is installed.
    pub(in crate::app) fn open_codeql_quick_query(&mut self) {
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

    /// VS Code's "CodeQL: Preview Query Help" (#578): the help of the open
    /// `.ql` query, else of the query the selected or newest history entry
    /// ran, rendered to Markdown by `codeql generate query-help` on a worker
    /// thread and shown in the Markdown preview.
    pub(in crate::app) fn preview_codeql_query_help(&mut self) {
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

    /// Find the workspace's queries for the side bar's Queries section now,
    /// for a caller that needs the list at once (a query just created, Run
    /// All Queries). It walks the tree, so opening the view does this off
    /// the UI thread instead. A walk still in flight is dropped: it may
    /// predate what this one finds.
    pub(in crate::app) fn refresh_codeql_queries(&mut self) {
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
    pub(in crate::app) fn discover_codeql_queries(&mut self) {
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
    pub(in crate::app) fn prompt_create_codeql_query(&mut self) {
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
    pub(in crate::app) fn submit_create_codeql_query(
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
    /// CodeQL: Open Referenced File (#578): the query the open `.qlref`
    /// test file names, as `codeql resolve qlref` finds it through the
    /// test pack's dependencies.
    pub(in crate::app) fn open_codeql_referenced_file(&mut self) {
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
}
