//! Running a query: the open query on the current database or another, quick
//! evaluation, Debug Query, running `codeql` and cancelling it, and recording a
//! finished run in the query history.

use super::*;

impl App {
    /// Run the open `.ql` query or `.qls` suite on the current database
    /// (#578). The CLI reads the file from disk, so an unsaved buffer is
    /// refused rather than silently running the saved text.
    pub(in crate::app) fn run_codeql_query(&mut self) {
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

    /// Run `query`, whose text is `source`, on the current database (#578):
    /// alerts through `database analyze` into SARIF, anything else through
    /// `query run` into a table decoded as CSV. The run happens on a worker
    /// thread; [`Self::drain_codeql_run`] collects it.
    pub(in crate::app) fn run_codeql_file(&mut self, query: PathBuf, source: &str) {
        self.run_codeql_file_with(query, source, None);
    }

    /// [`Self::run_codeql_file`], or with `quick` a quick evaluation of a
    /// span of `query` through the CLI's query server (#578), recorded in
    /// the history like any run and decoded to a table.
    pub(in crate::app) fn run_codeql_file_with(
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

    /// [`Self::run_codeql_file_with`] on the database at `database`, or on
    /// the current one when that is `None`.
    pub(in crate::app) fn run_codeql_file_on(
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

    /// CodeQL: Quick Evaluation, and its count variant (#578): evaluate the
    /// predicate under the cursor, or the selected expression, of the open
    /// query on the current database, through the CLI's query server.
    pub(in crate::app) fn quick_eval_codeql(&mut self, count: bool) {
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
    pub(in crate::app) fn debug_codeql_query(&mut self) {
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
    pub(in crate::app) fn codeql_command_until(
        program: &Path,
        args: &[String],
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String> {
        use std::io::Read as _;
        use std::sync::atomic::Ordering;
        if cancel.load(Ordering::SeqCst) {
            return Err(String::from("cancelled"));
        }
        let mut child = crate::proc::spawn_retrying_busy(
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
    pub(in crate::app) fn cancel_codeql_run(&mut self) {
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
    pub(in crate::app) fn codeql_stdout(program: &Path, args: &[String]) -> Result<String, String> {
        let out = crate::proc::output_retrying_busy(std::process::Command::new(program).args(args))
            .map_err(|e| format!("could not run codeql: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(crate::codeql_query::failure_reason(
            &String::from_utf8_lossy(&out.stderr),
        ))
    }

    /// Run `codeql` with `args` and wait. A failure is the reason and the
    /// fix the CLI printed on stderr ([`crate::codeql_query::failure_reason`]).
    pub(in crate::app) fn codeql_command(program: &Path, args: &[String]) -> Result<(), String> {
        let out = crate::proc::output_retrying_busy(std::process::Command::new(program).args(args))
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
    pub(in crate::app) fn record_codeql_run(
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
}
