//! Contextual queries: what they run on and the queries they resolve, View AST and its
//! viewer, and View CFG.

use super::*;

impl App {
    /// What a contextual query (AST, CFG) runs on (#578): the open file,
    /// the current database, its language, and the file's name in that
    /// database's source archive. Says what is missing when something is.
    pub(in crate::app) fn codeql_contextual_target(
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
    pub(in crate::app) fn codeql_contextual_dir(kind: &str) -> PathBuf {
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
    pub(in crate::app) fn codeql_contextual_query(
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
    pub(in crate::app) fn view_codeql_cfg(&mut self) {
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

    /// CodeQL: View AST (#578): the AST of the open file, read from the
    /// current database with its language's print-AST query, off the UI
    /// thread; [`Self::drain_codeql_ast`] shows it in the AST Viewer.
    pub(in crate::app) fn view_codeql_ast(&mut self) {
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
}
