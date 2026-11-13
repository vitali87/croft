//! The Testing view, Tasks (Run Task, the build task, task inputs, problem matchers),
//! runnable Markdown blocks, notebooks and kernels, Fleet, Live Run and .http requests.

use super::*;

/// How long one host gets in a fleet run (#363).
///
/// A ceiling on the CONNECT and a wall-clock bound on the whole thing: the
/// issue's criterion is that a host which times out shows red without
/// delaying the others, and only the second bound delivers that for a host
/// that connects and then hangs.
pub(super) const FLEET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What "open shell here" types for a fleet target (#363): nothing for
/// localhost (the pane is already a shell there), `docker exec -it` for a
/// container, `ssh` for a host. Names are quoted for the pane's shell.
pub(super) fn fleet_shell_command(host: &str) -> Option<String> {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    if host == "localhost" {
        return None;
    }
    Some(match host.strip_prefix("docker:") {
        Some(name) => format!("docker exec -it {} sh", quote(name)),
        None => format!("ssh {}", quote(host)),
    })
}

/// A `cargo test --no-run` running on a background thread: the receiver for
/// its result, the test's name, and the workspace root it builds for (so a
/// re-root mid-build discards the binary instead of debugging it in the
/// wrong workspace).
pub(super) type PendingTestDebug = (
    std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    String,
    PathBuf,
);

/// A CodeLLDB `cargo` launch building its program on a background thread
/// (#1392): the receiver for the executable (or the build error), the
/// config to launch with it, the slot it launches into, and the workspace
/// root it builds for (so a re-root mid-build discards the binary).
pub(super) type PendingCargoLaunch = (
    std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    Box<crate::dap::configs::ResolvedConfig>,
    LaunchSlot,
    PathBuf,
);

/// The test run Testing: Rerun Last Run repeats (#1535), as it was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LastTestRun {
    All,
    One(String),
    /// A filter run and the file it came from, which picks the runner
    /// (#1408).
    Filter(String, Option<PathBuf>),
    Suite(String),
    Many(Vec<String>),
}

impl App {
    /// The folder the Testing view follows for workspace folder `root`
    /// (#1520): `root`, or in a monorepo whose root no runner claims, the
    /// package of the focused file that one does. Resolved again only when
    /// the folder or the focused file changes, since it reads manifests.
    pub(super) fn followed_test_root(&mut self, root: PathBuf) -> PathBuf {
        let key = (root, self.editor.path.clone());
        if let Some((k, v)) = &self.test_root_follow
            && *k == key
        {
            return v.clone();
        }
        let found = crate::testing::registry::test_root_for(
            &self.extensions_dir(),
            &key.0,
            key.1.as_deref(),
        );
        self.test_root_follow = Some((key, found.clone()));
        found
    }

    /// Resolve the followed Testing folder afresh and rebind to it if it
    /// moved (#1520): a runner enabled or a package marker added since the
    /// focus last changed can change the answer the cache still holds.
    /// Whether it rebound.
    pub(super) fn refollow_test_root(&mut self) -> bool {
        self.test_root_follow = None;
        let root = self.followed_test_root(self.active_workspace_root());
        if root == self.active_test_root {
            return false;
        }
        self.rebind_test_root(root);
        true
    }

    /// Testing view keys: Enter runs all tests, `r` re-discovers, `o` shows the
    /// runner's output, arrows scroll the tree, Esc returns to the Explorer
    /// (mirrors the Run-and-Debug view).
    pub(super) fn handle_testing_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
            KeyCode::Enter => self.run_all_tests(),
            KeyCode::Char('s' | 'S') if self.testing.is_busy() => self.cancel_test_run(),
            KeyCode::Char('r' | 'R') => self.rediscover_tests(),
            KeyCode::Char('w' | 'W') => {
                self.toggle_test_watch(crate::testing::watch::WatchScope::All)
            }
            KeyCode::Char('o' | 'O') => self.show_test_runner_output(),
            KeyCode::Up => self.testing.scroll_up(1),
            KeyCode::Down => self.testing.scroll_down(1),
            _ => {}
        }
    }

    /// Open the bottom panel on OUTPUT with the Test Runner channel selected
    /// (#845): where a failed discovery's or run's reason (the traceback,
    /// the compile error) actually is. `o` in the Testing view, or a click
    /// on its "see OUTPUT › Test Runner" pointer.
    pub(super) fn show_test_runner_output(&mut self) {
        // The panel mirrors the bus's channels only when it syncs, and the
        // runner may have written its first line since the last frame.
        self.output.sync();
        if !self.output.select_by_name(crate::output::CHANNEL_TESTS) {
            self.status = String::from("The test runner has not written any output yet");
        }
        self.set_bottom_panel_tab(BottomPanelTab::Output);
    }

    /// A new run while one is in flight is refused with the way out
    /// (#1534): it used to be dropped without a word, and a hung test left
    /// the view refusing every run until croft quit.
    pub(super) fn refuse_test_run_while_busy(&mut self) -> bool {
        if !self.testing.is_busy() {
            return false;
        }
        self.status = String::from(TEST_RUN_IN_PROGRESS);
        // The run in flight still reports its outcome over this (#856).
        if let Some(run) = self.test_run_status.as_mut() {
            run.shown = self.status.clone();
        }
        true
    }

    /// Testing: Cancel Test Run, `s` in the Testing view, or its Stop
    /// control (#1534): kill the run or discovery in flight. The panel
    /// settles when the worker reports the run's end.
    pub(super) fn cancel_test_run(&mut self) {
        if !self.testing.is_busy() {
            self.status = String::from("No test run in progress");
            return;
        }
        self.test_worker.cancel();
        self.status = String::from("Cancelling test run");
    }

    /// Whether an enabled runner extension claims this workspace. When none
    /// does — no recognised project, or its runner toggled off in the
    /// Extensions panel — every run gesture must refuse here: the worker's
    /// fallback used to shell `cargo test` regardless, so a disabled Rust
    /// runner still ran (and rebuilt) the workspace, and a disabled Python
    /// runner ran cargo in a directory with no Cargo.toml.
    pub(super) fn testing_runner_available(&mut self) -> bool {
        if crate::testing::worker::runner_for(&self.extensions_dir(), &self.active_test_root)
            .is_some()
        {
            return true;
        }
        if self.refollow_test_root()
            && crate::testing::worker::runner_for(&self.extensions_dir(), &self.active_test_root)
                .is_some()
        {
            return true;
        }
        self.status = String::from(crate::testing::NO_RUNNER_STATUS);
        false
    }

    /// Rebind the Testing stack to `root` — the focused file's workspace
    /// folder (#151). Exactly the re-root rebind: the worker's epoch drops
    /// in-flight results for the old root, the panel resets (its tree
    /// described another project), a pending debug build is dropped, and
    /// the front-of-view panel re-discovers immediately.
    pub(super) fn rebind_test_root(&mut self, root: PathBuf) {
        self.active_test_root = root.clone();
        self.test_worker.set_root(root);
        self.testing.reset();
        // The old root's run will never report its end (#856).
        self.test_run_status = None;
        // The build this armed for is being dropped (#373).
        self.disarm_failure_breakpoint();
        self.pending_test_debug = None;
        // Its names belong to the old project (#1535).
        self.last_test_run = None;
        if self.sidebar_view == SidebarView::Testing {
            self.discover_tests();
        }
    }

    /// Run everything with the runner's coverage tool (#263). Results
    /// stream into the tree as usual; the report lands in the editor's
    /// gutter, the status bar and the Testing summary.
    pub(super) fn run_all_tests_with_coverage(&mut self) {
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.test_run_with_unsaved_saved(|app| {
            app.test_worker.run_coverage();
            app.set_sidebar_view(SidebarView::Testing);
            app.status = String::from("Running tests with coverage");
        });
    }

    /// Fold a finished coverage run into the editor, and keep the active
    /// file's lens current: rebuilt when the report or the file changes,
    /// dimmed once the file is edited or rewritten after the run.
    pub(super) fn sync_coverage(&mut self) -> bool {
        use crate::testing::worker::CoverageError;
        let mut changed = false;
        match self.testing.take_coverage_error() {
            Some(CoverageError::Missing { tool, install }) => {
                self.status = format!(
                    "Coverage needs {tool}: run \"Testing: Install Coverage Tool\" ({install})"
                );
                self.coverage_install = Some(install);
                changed = true;
            }
            Some(CoverageError::NoReport) => {
                self.status = String::from("The coverage run wrote no report");
                changed = true;
            }
            Some(CoverageError::Unsupported { runner }) => {
                self.status = format!("Coverage is not available for {runner}");
                changed = true;
            }
            None => {}
        }
        let fresh = self.testing.take_coverage_fresh();
        if fresh {
            self.coverage_at = Some(std::time::SystemTime::now());
            if let Some(pct) = self.testing.coverage.as_ref().and_then(|c| c.percent()) {
                self.status = format!("Coverage: {pct:.0}% of lines");
            }
        }
        let path = self.editor.path.clone();
        if fresh || path != self.coverage_lens_path {
            self.coverage_lens_path = path.clone();
            let stale = path
                .as_ref()
                .is_some_and(|p| self.file_changed_since_coverage(p));
            self.editor.coverage = match (self.testing.coverage.as_ref(), path.as_ref()) {
                (Some(c), Some(p)) => c.lens(p, stale),
                _ => None,
            };
            changed = true;
        }
        if self.editor.dirty
            && let Some(lens) = self.editor.coverage.as_mut()
            && !lens.stale
        {
            lens.stale = true;
            changed = true;
        }
        changed
    }

    /// The file on disk is newer than the coverage run.
    pub(super) fn file_changed_since_coverage(&self, path: &Path) -> bool {
        let Some(at) = self.coverage_at else {
            return false;
        };
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m > at)
    }

    /// Drop the coverage report and give the gutter lane back to git.
    pub(super) fn clear_coverage(&mut self) {
        self.testing.coverage = None;
        self.editor.coverage = None;
        self.coverage_at = None;
        self.status = String::from("Coverage cleared");
    }

    /// Testing: Show Coverage Report (#263): the last coverage run as a
    /// tab, one file per line with its percentage, least covered first.
    pub(super) fn show_coverage_report(&mut self) {
        let Some(coverage) = self.testing.coverage.as_ref() else {
            self.status = String::from("No coverage yet: run Testing: Run All Tests with Coverage");
            return;
        };
        let report = coverage.report(&self.active_test_root);
        match self
            .editor
            .open_text_buffer(Path::new("Coverage Report"), &report)
        {
            Ok(()) => self.focus_pane(Pane::Editor),
            Err(e) => self.status = format!("Could not open the report: {e}"),
        }
    }

    /// Install the runner's coverage tool in a terminal pane, where its
    /// output and any prompt are visible.
    pub(super) fn install_coverage_tool(&mut self) {
        let Some(command) = self.coverage_install.clone() else {
            self.status = String::from("No coverage tool is missing");
            return;
        };
        match crate::widgets::terminal::PtyTerminal::new(&self.active_test_root) {
            Ok(mut term) => {
                term.set_manual_name(Some(String::from("coverage install")));
                term.write_input(format!("{command}\r").as_bytes());
                self.insert_terminal(term);
                self.show_terminal = true;
                self.focus_pane(Pane::Terminal);
                self.status = format!("Installing: {command}");
            }
            Err(e) => self.status = format!("Could not open a terminal: {e}"),
        }
    }

    /// Watch `scope`, or stop watching it (#263).
    pub(super) fn toggle_test_watch(&mut self, scope: crate::testing::watch::WatchScope) {
        use crate::testing::watch::WatchScope;
        let label = match &scope {
            WatchScope::All => String::from("all tests"),
            WatchScope::Suite(s) => format!("suite {s}"),
            WatchScope::Test(t) => t.clone(),
        };
        self.status = if self.testing.watch.toggle(scope) {
            format!("Watching {label}: it reruns when a file is saved")
        } else {
            format!("Stopped watching {label}")
        };
    }

    /// Rerun what is watched, if a save made it due, and report a watched
    /// run that has just turned red. A rerun leaves the sidebar where the
    /// user has it: a save should not pull them away from what they were
    /// doing. A red run already raises the activity-bar badge; the status
    /// line adds which test and how to get to it.
    pub(super) fn tick_test_watch(&mut self) -> bool {
        use crate::testing::watch::{WatchNotice, WatchScope};
        // The one reader of the run-ended latch also settles a run's
        // "Running …" status (#856).
        let finished = self.testing.take_finished();
        let mut changed = self.announce_test_run_end(finished);
        self.speak_test_run_end(finished, changed);
        if let Some(ok) = finished
            && self.testing.watch.finished(ok) == WatchNotice::NewlyRed
        {
            self.status = match self.testing.first_failed() {
                Some(name) => {
                    format!("Watched tests failed: {name} (Testing: Go to First Failure)")
                }
                None => String::from("Watched tests failed"),
            };
            changed = true;
        }
        if self.testing.watch.scope().is_none() {
            return changed;
        }
        // Only a due rerun needs the runner looked up: that lookup reads the
        // runner manifests, and this runs on every pass of the main loop
        // while a watch is on (#1841).
        let now = std::time::Instant::now();
        if self.testing.watch.due_at().is_none_or(|due| now < due) {
            return changed;
        }
        let busy = self.testing.is_busy()
            || crate::testing::worker::runner_for(&self.extensions_dir(), &self.active_test_root)
                .is_none();
        let Some(scope) = self.testing.watch.take_due(now, busy) else {
            return changed;
        };
        match scope {
            WatchScope::All => self.test_worker.run_all(),
            WatchScope::Suite(suite) => {
                self.testing
                    .start_filter(&crate::testing::suite_pattern(&suite));
                self.test_worker.run_suite(suite);
            }
            WatchScope::Test(name) => {
                self.testing.start_single(&name);
                self.test_worker.run_one(name);
            }
        }
        true
    }

    /// Jump to the first failing test's source.
    pub(super) fn go_to_first_failed_test(&mut self) {
        match self.testing.first_failed() {
            Some(name) => self.jump_to_test_source(name),
            None => self.status = String::from("No failing tests"),
        }
    }

    /// Kick off a full test run on the worker (no-op if a run/discovery is
    /// already in flight) and reveal the Testing view so results stream in.
    pub(super) fn run_all_tests(&mut self) {
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.last_test_run = Some(LastTestRun::All);
        self.test_run_with_unsaved_saved(|app| {
            app.test_worker.run_all();
            app.set_sidebar_view(SidebarView::Testing);
        });
    }

    /// Run a single test by exact name (click-to-run from the tree). Marks just
    /// that case Running and keeps the rest of the discovered list intact.
    pub(super) fn run_test(&mut self, name: String) {
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.last_test_run = Some(LastTestRun::One(name.clone()));
        self.test_run_with_unsaved_saved(|app| {
            app.testing.start_single(&name);
            app.test_worker.run_one(name);
            app.set_sidebar_view(SidebarView::Testing);
        });
    }

    /// Jump the editor to a test's `fn` definition (Cmd/Opt+click in the tree).
    /// `--list` carries no location, so we grep the source for the function —
    /// a whole-workspace walk, so it runs on a background thread and the jump
    /// lands via the drain in [`Self::sync_explorer_panels`].
    pub(super) fn jump_to_test_source(&mut self, name: String) {
        let root = self.active_test_root.clone();
        let tx = self.test_jump_tx.clone();
        self.status = format!("Locating test {name}");
        std::thread::spawn(move || {
            let hit = crate::testing::locate::find_test_source(&root, &name);
            let _ = tx.send((root, name, hit));
        });
    }

    /// Run every test in a suite (click on its header in the tree). cargo's
    /// filter and the panel's marking are substring matches, so the pattern is
    /// anchored (`parse::`) to keep `parse` from sweeping `parse_utils::b`.
    pub(super) fn run_suite(&mut self, suite: String) {
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.last_test_run = Some(LastTestRun::Suite(suite.clone()));
        self.test_run_with_unsaved_saved(|app| {
            app.testing
                .start_filter(&crate::testing::suite_pattern(&suite));
            app.test_worker.run_suite(suite);
            app.set_sidebar_view(SidebarView::Testing);
        });
    }

    /// Run the test the editor caret sits in (Cmd+K Enter / palette). Finds the
    /// enclosing `fn` and runs it by name; results stream into the
    /// Testing view like any other run.
    pub(super) fn run_test_at_cursor(&mut self) {
        let Some(name) =
            crate::testing::locate::enclosing_fn_name(&self.editor.lines, self.editor.cursor_row)
        else {
            self.status = String::from("Run Test at Cursor: no function at the caret");
            return;
        };
        self.run_named_test(name);
    }

    /// Start the test `name` through the worker and reveal the Testing view —
    /// the shared tail of run-at-cursor and the editor's gutter play glyph.
    /// A leaf unique in the discovered tree resolves to its full name for an
    /// exact run; otherwise cargo's substring filter is the best available.
    pub(super) fn run_named_test(&mut self, name: String) {
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.test_run_with_unsaved_saved(|app| app.run_named_test_now(name));
    }

    pub(super) fn run_named_test_now(&mut self, name: String) {
        let (run, exact) = match self.testing.sole_case_with_leaf(&name) {
            Some(full) => (full, true),
            None => (name, false),
        };
        if exact {
            self.testing.start_single(&run);
            self.last_test_run = Some(LastTestRun::One(run.clone()));
            self.test_worker.run_one(run.clone());
        } else {
            self.testing.start_filter(&run);
            // The caret's file picks the runner where two languages'
            // tests share the folder (#1408).
            let file = self.editor.path.clone();
            self.last_test_run = Some(LastTestRun::Filter(run.clone(), file.clone()));
            self.test_worker.run_filter(run.clone(), file);
        }
        self.set_sidebar_view(SidebarView::Testing);
        self.status = format!("Running test {run}");
        self.await_test_run_outcome(crate::testing::leaf_name(&run).to_string());
    }

    /// When a test run is already going, say so (#1535) rather than let a
    /// palette command do nothing; true when it was.
    pub(super) fn testing_busy_says_so(&mut self) -> bool {
        if self.testing.is_busy() {
            self.status = String::from("Tests are already running");
            return true;
        }
        false
    }

    /// Testing: Run Tests in Current File (#1535): the suite the active
    /// file is, in the runner's own naming (see [`file_test_suite`]). A file
    /// the runner cannot select by itself says so.
    pub(super) fn run_tests_in_current_file(&mut self) {
        if self.testing_busy_says_so() || !self.testing_runner_available() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Run Tests in Current File: open a test file first");
            return;
        };
        let root = self.active_test_root.clone();
        let suite = crate::testing::worker::runner_for(&self.extensions_dir(), &root)
            .and_then(|runner| file_test_suite(runner, &root, &path));
        let Some(suite) = suite else {
            self.status = String::from(
                "Run Tests in Current File: the runner cannot pick out this file's tests; use Run All Tests",
            );
            return;
        };
        self.run_suite(suite.clone());
        self.status = format!("Running tests in {suite}");
        self.await_test_run_outcome(suite);
    }

    /// Testing: Rerun Failed Tests (#1535): each test the tree shows failed,
    /// by its exact name.
    pub(super) fn rerun_failed_tests(&mut self) {
        if self.testing_busy_says_so() || !self.testing_runner_available() {
            return;
        }
        let failed = self.testing.failed_names();
        if failed.is_empty() {
            self.status = String::from("Rerun Failed Tests: no test has failed");
            return;
        }
        self.start_many_tests(failed);
    }

    /// Run `names` exactly, one after another, as one run.
    pub(super) fn start_many_tests(&mut self, names: Vec<String>) {
        self.testing.start_many(&names);
        self.last_test_run = Some(LastTestRun::Many(names.clone()));
        let label = format!("{} tests", names.len());
        self.test_worker.run_many(names);
        self.set_sidebar_view(SidebarView::Testing);
        self.status = format!("Running {label}");
        self.await_test_run_outcome(label);
    }

    /// Testing: Rerun Last Run (#1535): the last run started, as it was
    /// asked (everything, one test, a filter, a suite or a set).
    pub(super) fn rerun_last_test_run(&mut self) {
        if self.testing_busy_says_so() {
            return;
        }
        let Some(last) = self.last_test_run.clone() else {
            self.status = String::from("Rerun Last Run: no test has run yet");
            return;
        };
        if !self.testing_runner_available() {
            return;
        }
        match last {
            LastTestRun::All => self.run_all_tests(),
            LastTestRun::One(name) => self.run_test(name),
            LastTestRun::Suite(suite) => self.run_suite(suite),
            LastTestRun::Filter(pattern, file) => {
                self.testing.start_filter(&pattern);
                self.last_test_run = Some(LastTestRun::Filter(pattern.clone(), file.clone()));
                self.test_worker.run_filter(pattern, file);
                self.set_sidebar_view(SidebarView::Testing);
            }
            LastTestRun::Many(names) => self.start_many_tests(names),
        }
    }

    /// Keep the "Running …" status a test run just set until the run ends,
    /// when [`Self::announce_test_run_end`] replaces it with the outcome
    /// (#856).
    pub(super) fn await_test_run_outcome(&mut self, label: String) {
        self.test_run_status = Some(RunningStatus::new(&self.status, label));
    }

    /// Once the test run that set a "Running …" status is over, say how it
    /// went there (#856): the panel's tree updates, but nothing else touched
    /// the status bar, which went on claiming a run long finished. Only
    /// while the bar still shows what the run set, so a newer message is
    /// never clobbered. Nothing has ended until the run-ended latch fires: a
    /// run-all is idle until the worker's start lands. A run that ended with
    /// no verdict (the coverage tool missing) just lets go. `finished` is
    /// the panel's run-ended latch, which the caller has consumed.
    pub(super) fn announce_test_run_end(&mut self, finished: Option<Option<bool>>) -> bool {
        let Some(verdict) = finished else {
            return false;
        };
        // Another run already started in the same drain: the status that
        // waits is its, and its own end settles it.
        if self.testing.is_busy() {
            return false;
        }
        let Some(run) = self.test_run_status.take() else {
            return false;
        };
        let Some(ok) = verdict else {
            return false;
        };
        let testing = &self.testing;
        run.settle(&mut self.status, |label, took| {
            testing.run_outcome(label, ok, took.as_secs_f64())
        })
    }

    /// Screen reader mode reads out how a finished test run went (#1547),
    /// Run All included, which sets no "Running …" status to settle. When
    /// [`Self::announce_test_run_end`] already put the counts in the status
    /// (`settled`), which is read too, only the first failure is added.
    pub(super) fn speak_test_run_end(&mut self, finished: Option<Option<bool>>, settled: bool) {
        let Some(Some(ok)) = finished else {
            return;
        };
        if self.testing.is_busy() {
            return;
        }
        let said = if settled {
            match self.testing.run_first_failure() {
                Some(name) => format!("First failure: {name}"),
                None => return,
            }
        } else {
            self.testing.spoken_outcome(ok)
        };
        self.announce_event(said);
    }

    /// Keep the "Running …" status a task or fenced block just set until
    /// pane `pane` reports the command finished (#856). A newer run in the
    /// same pane takes over the wait.
    pub(super) fn await_pane_outcome(&mut self, pane: u64, label: String) {
        self.pane_run_statuses.retain(|(uid, _)| *uid != pane);
        self.pane_run_statuses
            .push((pane, RunningStatus::new(&self.status, label)));
    }

    /// Testing: Run Test at Cursor with Coverage (#263): the caret's test
    /// alone, resolved as run-at-cursor resolves it, under the coverage
    /// tool. The report replaces the last one, and so describes only the
    /// code that test reached.
    pub(super) fn run_test_at_cursor_with_coverage(&mut self) {
        let Some(name) =
            crate::testing::locate::enclosing_fn_name(&self.editor.lines, self.editor.cursor_row)
        else {
            self.status =
                String::from("Run Test at Cursor with Coverage: no function at the caret");
            return;
        };
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.test_run_with_unsaved_saved(|app| app.run_test_at_cursor_with_coverage_now(name));
    }

    pub(super) fn run_test_at_cursor_with_coverage_now(&mut self, name: String) {
        let (run, exact) = match self.testing.sole_case_with_leaf(&name) {
            Some(full) => (full, true),
            None => (name, false),
        };
        if exact {
            self.testing.start_single(&run);
        } else {
            self.testing.start_filter(&run);
        }
        self.test_worker
            .run_coverage_scoped(crate::testing::worker::CoverageScope {
                name: run.clone(),
                exact,
                suite: false,
            });
        self.set_sidebar_view(SidebarView::Testing);
        self.status = format!("Running test {run} with coverage");
        self.await_test_run_outcome(crate::testing::leaf_name(&run).to_string());
    }

    /// A Testing-tree row's coverage glyph (#263): that test, or that whole
    /// suite, under the coverage tool, marked in the tree as a plain run of
    /// it would be. The report replaces the last one.
    pub(super) fn run_scope_with_coverage(&mut self, name: String, suite: bool) {
        if self.refuse_test_run_while_busy() || !self.testing_runner_available() {
            return;
        }
        self.test_run_with_unsaved_saved(|app| app.run_scope_with_coverage_now(name, suite));
    }

    pub(super) fn run_scope_with_coverage_now(&mut self, name: String, suite: bool) {
        if suite {
            self.testing
                .start_filter(&crate::testing::suite_pattern(&name));
        } else {
            self.testing.start_single(&name);
        }
        self.test_worker
            .run_coverage_scoped(crate::testing::worker::CoverageScope {
                name: name.clone(),
                exact: !suite,
                suite,
            });
        self.status = if suite {
            format!("Running suite {name} with coverage")
        } else {
            format!("Running test {name} with coverage")
        };
        let label = if suite {
            format!("suite {name}")
        } else {
            crate::testing::leaf_name(&name).to_string()
        };
        self.await_test_run_outcome(label);
    }

    /// List tests without running them (populate the tree). No-op while busy or
    /// while one is already queued. Outside a recognised test project the
    /// worker finds no runner and says nothing — the tool would error with no
    /// manifest, and `cargo test -- --list` compiles the test binary so we
    /// never auto-fire it per-keystroke. Finding the runner is the worker's
    /// job, not this thread's (#1841): it reads every runner manifest and
    /// can walk the whole tree for CodeQL test packs, which stalled the
    /// switch to the Testing view.
    pub(super) fn discover_tests(&mut self) {
        // The rebind re-discovers on its own while the view is showing.
        if !self.testing.is_busy()
            && self.refollow_test_root()
            && self.sidebar_view == SidebarView::Testing
        {
            return;
        }
        if self.testing.is_busy() || self.test_worker.discover_queued() {
            return;
        }
        self.test_worker.discover_quietly();
    }

    /// `r` in the Testing view: discover again, saying so when no runner
    /// claims the workspace rather than doing nothing. Like the view's own
    /// discover, the worker is the one that looks for the runner (#1841).
    pub(super) fn rediscover_tests(&mut self) {
        if !self.testing.is_busy() {
            self.test_worker.discover();
        }
    }

    /// Reveal the Testing view (the user-open gesture: activity icon, Cmd+K B,
    /// palette). Auto-discovers once when the tree is still empty so it
    /// populates like VS Code, without recompiling on every subsequent open.
    pub(super) fn open_testing_view(&mut self) {
        self.show_tree = true;
        self.set_sidebar_view(SidebarView::Testing);
        if self.testing.is_empty() {
            self.discover_tests();
        }
    }

    /// [`Self::run_with_unsaved_saved`] for a Testing run: the saves it
    /// makes are for this run, so a watched scope does not also hear
    /// about them and rerun after it.
    pub(super) fn test_run_with_unsaved_saved(&mut self, launch: impl FnOnce(&mut Self)) {
        let due = self.testing.watch.due_at();
        self.run_with_unsaved_saved("the test run", |app| {
            app.testing.watch.set_due_at(due);
            launch(app);
        });
    }

    /// The `${input:…}` ids the task labelled `label` uses, added to
    /// `refs`, and its file's `inputs` to `defs` (#1424).
    pub(super) fn task_inputs(
        &self,
        label: &str,
        refs: &mut Vec<String>,
        defs: &mut Vec<crate::dap::configs::LaunchInput>,
    ) {
        let tasks = crate::tasks::discover_tasks(&self.active_workspace_root());
        let Some(task) = tasks.into_iter().find(|t| t.label == label) else {
            return;
        };
        for r in task.input_refs() {
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
        if let Some(v) = task.vscode {
            defs.extend(v.inputs);
        }
    }

    /// Whether every id in `refs` already has an answer. When some do not,
    /// their questions are asked one at a time and `then` runs once the
    /// last is answered (`Ok(false)`); Esc on any of them drops the run
    /// with no error. An id no `inputs` entry declares is an error naming
    /// it (#1424).
    pub(super) fn ask_inputs(
        &mut self,
        refs: &[String],
        defs: &[crate::dap::configs::LaunchInput],
        answers: &std::collections::BTreeMap<String, String>,
        then: InputThen,
    ) -> Result<bool, String> {
        let ask = crate::dap::configs::inputs_to_ask(refs, answers, defs)?;
        if ask.is_empty() {
            return Ok(true);
        }
        self.input_run = Some(InputRun {
            ask,
            answers: answers.clone(),
            then,
        });
        self.ask_next_input();
        Ok(false)
    }

    /// Open the question for the next unanswered input, or run what the
    /// answers were for once there are none left.
    pub(super) fn ask_next_input(&mut self) {
        use crate::dap::configs::InputKind;
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let Some(run) = self.input_run.as_ref() else {
            return;
        };
        let Some(input) = run.ask.first().cloned() else {
            let Some(run) = self.input_run.take() else {
                return;
            };
            match run.then {
                InputThen::Launch(mut cfg) => {
                    cfg.answers = run.answers;
                    self.launch_debug_config(&cfg);
                }
                InputThen::Compound(mut compound) => {
                    compound.answers = run.answers;
                    self.launch_compound(&compound);
                }
                InputThen::Task(task) => {
                    self.run_project_task(with_answers(*task, &run.answers));
                }
            }
            return;
        };
        match input.kind {
            InputKind::Prompt { password } => {
                let mut prompt = InputPrompt::new(InputPurpose::LaunchInput, input.description, "")
                    .allowing_blank();
                if let Some(default) = input.default {
                    prompt = prompt.with_value(default);
                }
                if password {
                    prompt = prompt.masked();
                }
                self.open_input_prompt(prompt);
            }
            InputKind::Pick { options } => {
                let selected = input
                    .default
                    .and_then(|d| options.iter().position(|(_, value)| *value == d))
                    .unwrap_or(0);
                let rows = options
                    .into_iter()
                    .map(|(label, value)| ListRow { id: value, label })
                    .collect();
                let mut picker = ListPicker::new(ListPurpose::LaunchInput, input.description, rows);
                picker.selected = selected;
                self.open_list_picker(picker, "");
            }
            InputKind::Command { command } => {
                let task = matches!(run.then, InputThen::Task(_));
                self.input_run = None;
                let msg = format!(
                    "${{input:{}}} runs the command \"{command}\", which croft cannot run",
                    input.id
                );
                if task {
                    self.status = msg;
                } else {
                    self.debug_error(msg);
                }
            }
        }
    }

    /// The user answered the input being asked.
    pub(super) fn answer_input(&mut self, value: String) {
        if let Some(run) = self.input_run.as_mut()
            && !run.ask.is_empty()
        {
            let input = run.ask.remove(0);
            run.answers.insert(input.id, value);
        }
        self.ask_next_input();
    }

    /// Tasks: Run Task — discover the workspace's tasks and open the
    /// picker over them.
    pub fn open_run_task_picker(&mut self) {
        self.run_tasks = crate::tasks::discover_tasks(&self.active_workspace_root());
        let rows: Vec<crate::widgets::list_picker::ListRow> = self
            .run_tasks
            .iter()
            .enumerate()
            .map(|(i, t)| crate::widgets::list_picker::ListRow {
                id: i.to_string(),
                label: format!("{} — {}", t.label, t.source),
            })
            .collect();
        self.open_list_picker(
            crate::widgets::list_picker::ListPicker::new(
                crate::widgets::list_picker::ListPurpose::RunTask,
                "Run Task",
                rows,
            ),
            "No tasks found (Makefile, justfile, package.json, Cargo.toml, pyproject.toml, go.mod, CMakeLists.txt, .vscode/tasks.json)",
        );
    }

    /// The line `run_project_task` types for `task`: tasks.json variables
    /// expanded against the active workspace and file. The preLaunchTask
    /// gate matches the pane's finished command against this same line.
    pub(super) fn task_command_line(&self, task: &crate::tasks::Task) -> Result<String, String> {
        self.task_command_line_in(task, self.active_workspace_root())
    }

    /// [`Self::task_command_line`] against the workspace folder `root`: a
    /// compound member's preLaunchTask from another folder (#1425) expands
    /// `${workspaceFolder}` to that folder, not the active one.
    pub(super) fn task_command_line_in(
        &self,
        task: &crate::tasks::Task,
        root: PathBuf,
    ) -> Result<String, String> {
        task.command_line(&crate::dap::configs::SubstCtx {
            workspace_folder: root,
            file: self.editor.path.clone(),
            inputs: Default::default(),
            folders: self.workspace_folders(),
        })
    }

    /// Run `task` in its own named terminal pane. Rerunning a task whose
    /// pane sits idle at a prompt writes the command into that pane
    /// instead of stacking a new one; a busy pane gets a fresh sibling.
    /// Run a task in its named pane. Returns the pane's uid so a caller can
    /// correlate the task's FinishedCommand (the preLaunchTask gate, #250);
    /// `None` when no pane could be started (already reported in `status`).
    pub fn run_project_task(&mut self, task: crate::tasks::Task) -> Option<u64> {
        // Its `${input:…}` values first (#1424); the task runs once they
        // are answered.
        let refs = task.input_refs();
        if !refs.is_empty() {
            let (defs, answers) = task
                .vscode
                .as_ref()
                .map(|v| (v.inputs.clone(), v.answers.clone()))
                .unwrap_or_default();
            let label = task.label.clone();
            match self.ask_inputs(
                &refs,
                &defs,
                &answers,
                InputThen::Task(Box::new(task.clone())),
            ) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => {
                    self.status = format!("Task \u{201c}{label}\u{201d} not run: {e}");
                    return None;
                }
            }
        }
        let root = self.active_workspace_root();
        self.run_project_task_in(task, root)
    }

    /// [`Self::run_project_task`] in the workspace folder `folder`: the pane
    /// starts there and the task's variables expand against it, so a
    /// compound member's preLaunchTask from another folder (#1425) builds
    /// that folder's project.
    pub(super) fn run_project_task_in(
        &mut self,
        task: crate::tasks::Task,
        folder: PathBuf,
    ) -> Option<u64> {
        let pane_name = format!("Task: {}", task.label);
        // The task's problemMatcher (#252), translated up front so both the
        // reuse and fresh-pane paths assign the same thing (or clear a
        // previous task's leftover).
        let matcher = task
            .problem_matcher
            .as_ref()
            .and_then(crate::problem_matchers::from_tasks_json)
            .map(std::sync::Arc::new);
        // The line cleared first: the idle shell's line editor may hold a
        // half-typed command that would otherwise concatenate and run
        // (same rule `format_cd_command` pins for the cd seed).
        let line = match self.task_command_line_in(&task, folder.clone()) {
            Ok(line) => line,
            Err(e) => {
                self.status = format!("Task \u{201c}{}\u{201d} not run: {e}", task.label);
                return None;
            }
        };
        let command = format!("{}{line}\r", crate::widgets::terminal::CLEAR_TYPED_LINE);
        self.last_task = Some(task.clone());
        // Reuse only a pane whose shell is standing exactly where this
        // task will run: after a re-root the old pane's label still
        // matches, and running the new project's build there builds the
        // old one. Shares `pane_is_idle_at` with the runnable-fence path
        // (#430) — this guard existed twice and only one copy was fixed,
        // which is precisely how the two defects survived a green suite.
        let canonical = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        if let Some(idx) = self
            .terminals
            .iter()
            .position(|t| pane_is_idle_at(t, &pane_name, &canonical))
        {
            // Reused panes may be folded (#313): the release notes name the
            // build pane as the thing you would fold, and a task written into
            // a pane that is never rendered runs invisibly under a status line
            // saying it started.
            self.reveal_terminal_pane(idx);
            // The matcher goes in before the command does: a watcher whose
            // first cycle comes at once would otherwise be read with no
            // matcher, and a launch parked on that cycle (#1541) never ends.
            let uid = self.terminals[idx].uid();
            self.assign_task_matcher(uid, matcher);
            self.terminals[idx].write_input(command.as_bytes());
            self.show_terminal = true;
            self.focus_pane(Pane::Terminal);
            self.status = format!("Running {}", task.label);
            self.await_pane_outcome(uid, task.label);
            return Some(uid);
        }
        match crate::widgets::terminal::PtyTerminal::new(&folder) {
            Ok(mut term) => {
                term.set_manual_name(Some(pane_name));
                let uid = term.uid();
                self.insert_terminal(term);
                // Matcher first, then the command, as above.
                self.assign_task_matcher(uid, matcher);
                if let Some(t) = self.terminals.iter_mut().find(|t| t.uid() == uid) {
                    t.write_input(command.as_bytes());
                }
                self.status = format!("Running {}", task.label);
                self.await_pane_outcome(uid, task.label);
                Some(uid)
            }
            Err(e) => {
                self.status = format!("Could not start task pane: {e}");
                None
            }
        }
    }

    /// Record (or clear) the problemMatcher owning a task pane (#252): the
    /// map drives the batch scan at the FinishedCommand boundary, the
    /// pane's watch slot drives the mid-run engine when the matcher is a
    /// background one. Rerunning a matcher-less task in a pane clears the
    /// previous task's leftover, and either way the pane starts the new
    /// run without a stale watch-published flag.
    pub(super) fn assign_task_matcher(
        &mut self,
        pane: u64,
        matcher: Option<std::sync::Arc<crate::problem_matchers::CompiledMatcher>>,
    ) {
        self.watch_published_panes.remove(&pane);
        match &matcher {
            Some(m) => {
                self.task_matcher_by_pane.insert(pane, m.clone());
            }
            None => {
                self.task_matcher_by_pane.remove(&pane);
            }
        }
        if let Some(t) = self.terminals.iter().find(|t| t.uid() == pane) {
            t.set_pane_watch(matcher.filter(|m| m.background.is_some()));
        }
    }

    /// Tasks: Run Build Task (`Cmd+Shift+B`) — run the conventional build
    /// for whatever the workspace is; fall back to the picker when no
    /// manifest declares one.
    pub fn run_build_task(&mut self) {
        let tasks = crate::tasks::discover_tasks(&self.active_workspace_root());
        match crate::tasks::default_build_task(&tasks).cloned() {
            Some(task) => self.start_project_task(task),
            None => {
                self.open_run_task_picker();
                self.status = "No build task detected; pick one".to_string();
            }
        }
    }

    /// Tasks: Rerun Last Task.
    pub fn rerun_last_task(&mut self) {
        match self.last_task.clone() {
            Some(mut task) => {
                // The last run's `${input:…}` answers are asked again rather
                // than reused, as VS Code does (#1424).
                if let Some(v) = task.vscode.as_mut() {
                    v.answers.clear();
                }
                self.start_project_task(task)
            }
            None => self.status = "No task has run yet".to_string(),
        }
    }

    /// Run `task` as the user asked for it (Run Task, the build task, Rerun
    /// Last Task), unsaved tabs written first (#1639).
    pub(super) fn start_project_task(&mut self, task: crate::tasks::Task) {
        self.run_with_unsaved_saved("the task", |app| {
            app.run_project_task(task);
        });
    }

    /// The status bar's kernel chip for the notebook in the editor (#355):
    /// the kernel's name once it is up, and whether a cell is running.
    /// `None` for any other file, or before a kernel has started.
    pub(super) fn notebook_kernel_chip(&self) -> Option<String> {
        let run = self.notebook_kernels.get(self.editor.path.as_ref()?)?;
        let name = run.kernel.as_deref()?;
        Some(if run.running_indices().is_empty() {
            format!(" kernel: {name} ")
        } else {
            format!(" kernel: {name} \u{b7} busy ")
        })
    }

    /// Run `command` on every configured ssh host and report what differs
    /// (#363).
    ///
    /// In PARALLEL, one thread per host, because the whole point is
    /// comparing a fleet: run serially, ten hosts at a two-second timeout
    /// each is twenty seconds of staring, and a single unreachable host
    /// would hold up every other result behind it.
    pub(super) fn run_fleet_command(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            self.status = String::from("Fleet run needs a command");
            return;
        }
        let mut known: Vec<String> = crate::remote::discover_ssh_targets()
            .into_iter()
            .map(|t| t.alias)
            .collect();
        let no_ssh_hosts = known.is_empty();
        // `localhost` and running containers can be named explicitly; `*`
        // still means the ssh hosts only (see `parse_request_with_groups`).
        known.extend(crate::fleet::local_targets_in(command, &self.fleet_groups));
        if no_ssh_hosts
            && !crate::fleet::split_request(command).is_some_and(|(spec, _)| {
                spec.split(',')
                    .any(|n| crate::fleet::is_local_target(n.trim()))
            })
        {
            self.status = String::from(
                "No hosts in ~/.ssh/config to run on (localhost and docker:<container> work too)",
            );
            return;
        }
        // The fleet must be NAMED. Defaulting to every configured host means
        // running arbitrary text on every remote the user ever set up — and
        // a confirmation saying "run on 5 hosts?" asks them to approve a list
        // they cannot see, which is not consent. `*` is how to say "all of
        // them" deliberately.
        let (picked, command) =
            match crate::fleet::parse_request_with_groups(command, &known, &self.fleet_groups) {
                Ok(parsed) => parsed,
                Err(refusal) => {
                    self.status = refusal.message(&known);
                    return;
                }
            };
        let hosts: Vec<String> = picked.into_iter().cloned().collect();
        if self.fleet_running {
            self.status = String::from("A fleet run is already in flight");
            return;
        }
        crate::output::push(
            crate::output::CHANNEL_FLEET,
            crate::output::OutputLevel::Info,
            &format!("$ {command}   on {}", hosts.join(", ")),
        );
        // Off the event loop: the run waits on the network for up to
        // FLEET_TIMEOUT, and blocking the loop for that would freeze the
        // editor — the user could not scroll, type, or cancel while their
        // fleet answered. Results arrive through `fleet_rx` and are drained
        // on a later tick, the same shape the port poll and label lookup use.
        let host_count = hosts.len();
        let tx = self.fleet_tx.clone();
        let command = command.to_string();
        self.fleet_command = command.clone();
        self.fleet_running = true;
        std::thread::spawn(move || {
            let _ = tx.send(crate::fleet::run_on_hosts(&hosts, &command, FLEET_TIMEOUT));
        });
        self.status = format!("Fleet: running on {host_count} hosts…");
    }

    /// Report a finished fleet run, if one arrived (#363).
    ///
    /// Drained on the tick like the other background results. Returns
    /// whether anything landed, so the caller can redraw only when there is
    /// something new.
    pub(super) fn drain_fleet_results(&mut self) -> bool {
        // Every queued run, not just the newest. Each message is ONE run's
        // results, unlike the label channel this idiom comes from where each
        // message is a complete snapshot — taking `.last()` there is right
        // and here would drop a whole run's output with no trace of it.
        let batches: Vec<Vec<crate::fleet::HostResult>> = self.fleet_rx.try_iter().collect();
        if batches.is_empty() {
            return false;
        }
        self.fleet_running = false;
        for results in batches {
            let reference = crate::fleet::reference_output(&results).map(String::from);
            for r in &results {
                let mark = match (r.ok(), Some(r.output.as_str()) == reference.as_deref()) {
                    (false, _) => "FAILED",
                    (true, true) => "same",
                    (true, false) => "DIFFERS",
                };
                crate::output::push(
                    crate::output::CHANNEL_FLEET,
                    if r.ok() {
                        crate::output::OutputLevel::Info
                    } else {
                        crate::output::OutputLevel::Error
                    },
                    &format!(
                        "{}  [{mark}]  ({:.1}s)  {}",
                        r.host,
                        r.elapsed.as_secs_f64(),
                        r.output.replace('\n', " ")
                    ),
                );
                // A differing host lists the lines that differ from the
                // reference, so the row says WHAT differs, not only that
                // something does.
                if mark == "DIFFERS"
                    && let Some(reference) = reference.as_deref()
                {
                    for line in crate::fleet::differing_lines(reference, &r.output) {
                        crate::output::push(
                            crate::output::CHANNEL_FLEET,
                            crate::output::OutputLevel::Info,
                            &format!("    {line}"),
                        );
                    }
                }
            }
            let summary = crate::fleet::summarise(&results, reference.as_deref());
            crate::output::push(
                crate::output::CHANNEL_FLEET,
                crate::output::OutputLevel::Info,
                &summary.line(),
            );
            self.status = format!("Fleet: {}", summary.line());
            // The tiles (#363); OUTPUT keeps the text record above.
            self.editor
                .open_fleet(crate::widgets::fleet::FleetView::new(
                    self.fleet_command.clone(),
                    results,
                ));
            self.focus_pane(Pane::Editor);
        }
        true
    }

    /// FLEET tab keys (#363).
    pub(super) fn handle_fleet_key(&mut self, key: KeyEvent) {
        let Some(view) = self.editor.fleet.as_mut() else {
            return;
        };
        let cols = view.columns.max(1) as isize;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => view.move_selection(-1),
            KeyCode::Right | KeyCode::Char('l') => view.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => view.move_selection(-cols),
            KeyCode::Down | KeyCode::Char('j') => view.move_selection(cols),
            KeyCode::Char('d') => view.diff_mode = !view.diff_mode,
            KeyCode::Char('r') => {
                view.toggle_reference();
                self.status = match &view.reference_host {
                    Some(h) => format!("Fleet: comparing with {h}"),
                    None => String::from("Fleet: comparing with the most common output"),
                };
            }
            KeyCode::Char('s') => self.save_fleet_capture(),
            KeyCode::Enter => self.open_fleet_shell(),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.editor.close_active();
                self.status = String::from("Closed the fleet results");
            }
            _ => {}
        }
    }

    /// Save the fleet tab's run to `fleet-<time>.txt` in the workspace
    /// (#363): the command, the summary and every host's full output.
    pub(super) fn save_fleet_capture(&mut self) {
        let Some(view) = self.editor.fleet.as_ref() else {
            return;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = self.workspace_root().join(format!("fleet-{stamp}.txt"));
        self.status = match std::fs::write(&path, view.capture()) {
            Ok(()) => format!("Fleet run saved to {}", path.display()),
            Err(e) => format!("Could not save the fleet run: {e}"),
        };
    }

    /// "Open shell here" (#363): a terminal pane on the selected tile's
    /// host: `ssh` for a host, a shell for localhost, `docker exec` for a
    /// container. Typed into a fresh pane so the user sees what runs.
    pub(super) fn open_fleet_shell(&mut self) {
        let Some(host) = self
            .editor
            .fleet
            .as_ref()
            .and_then(|v| v.selected_result())
            .map(|r| r.host.clone())
        else {
            return;
        };
        let command = fleet_shell_command(&host);
        match crate::widgets::terminal::PtyTerminal::new(self.workspace_root()) {
            Ok(mut term) => {
                term.set_manual_name(Some(host.clone()));
                if let Some(cmd) = command {
                    term.write_input(format!("{cmd}\r").as_bytes());
                }
                self.insert_terminal(term);
                self.show_terminal = true;
                self.focus_pane(Pane::Terminal);
                self.status = format!("Shell on {host}");
            }
            Err(e) => self.status = format!("Could not open a terminal: {e}"),
        }
    }

    /// `.http`/`.rest` (#370): send the request under the caret. The env
    /// file's values are substituted before the send; a hole with no value
    /// refuses to send rather than leaking `{{name}}` to a server. The send
    /// runs on a worker thread and [`Self::drain_http_responses`] opens the
    /// response tab when it lands.
    pub(crate) fn send_http_request_under_caret(&mut self) {
        let Some((req, resolved, path)) = self.http_request_under_caret() else {
            return;
        };
        if self.http_run.is_some() {
            self.status = String::from("An HTTP request is already in flight");
            return;
        }
        // History records the RAW line: what the user wrote, secrets still
        // folded inside their {{names}}.
        let raw_line = format!("{} {}", req.method, req.url);
        let (tx, rx) = std::sync::mpsc::channel();
        let file = path.clone();
        let line = raw_line.clone();
        let spawned = std::thread::Builder::new()
            .name(String::from("croft-http"))
            .spawn(move || {
                let result = crate::http_file::send(&resolved, crate::http_file::REQUEST_TIMEOUT);
                let _ = tx.send(HttpRunOutcome {
                    file,
                    raw_line: line,
                    result,
                });
            });
        if spawned.is_err() {
            self.status = String::from("HTTP: could not start the request worker");
            return;
        }
        self.http_run = Some(rx);
        self.status = format!("Sending {raw_line}");
    }

    /// `.http` (#370): put the request under the caret on the clipboard as
    /// a curl command, variables substituted — the clipboard is the user's
    /// own hand, unlike history and the response tab.
    pub(crate) fn copy_http_request_as_curl(&mut self) {
        let Some((_, resolved, _)) = self.http_request_under_caret() else {
            return;
        };
        copy_to_clipboard(&crate::http_file::to_curl(&resolved));
        self.status = String::from("curl command copied");
    }

    /// The parsed and variable-resolved request under the caret, or a
    /// status naming why there is none (wrong file, no block, or a
    /// `{{name}}` with no value in `.http.env.json` / the environment).
    pub(super) fn http_request_under_caret(
        &mut self,
    ) -> Option<(
        crate::http_file::HttpRequest,
        crate::http_file::ResolvedRequest,
        PathBuf,
    )> {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return None;
        };
        if !crate::http_file::is_http_file(&path) {
            self.status = String::from("Not a .http/.rest file");
            return None;
        }
        let text = self.editor.lines.join("\n");
        let requests = crate::http_file::parse_requests(&text);
        let Some(req) = crate::http_file::request_at(&requests, self.editor.cursor_row).cloned()
        else {
            self.status = String::from("No request under the caret");
            return None;
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        let env = crate::http_file::load_env(dir, self.http_environment.as_deref());
        let vars = crate::http_file::variables(&text, env.vars);
        let (resolved, missing) = crate::http_file::resolve(&req, &vars);
        if !missing.is_empty() {
            let holes: Vec<String> = missing.iter().map(|m| format!("{{{{{m}}}}}")).collect();
            let active = self
                .http_environment
                .as_ref()
                .filter(|e| env.environments.contains(e));
            // Named environments nobody picked are the likely gap (#924):
            // say so rather than point at the files that hold the values.
            self.status = match active {
                None if !env.environments.is_empty() => format!(
                    "HTTP: no value for {}; pick an environment ({}) with HTTP: Switch Environment",
                    holes.join(", "),
                    env.environments.join(", ")
                ),
                Some(e) => format!(
                    "HTTP: no value for {} in environment {e} (or set it with @name = … in the file, or $env)",
                    holes.join(", "),
                ),
                None => format!(
                    "HTTP: no value for {} (set it with @name = … in the file, in {} beside it, or $env)",
                    holes.join(", "),
                    crate::http_file::ENV_FILE
                ),
            };
            return None;
        }
        Some((req, resolved, path))
    }

    /// HTTP: Switch Environment (#924): pick which named environment of the
    /// env files beside the active `.http` file fills its requests, or none.
    pub(super) fn open_http_environment_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        if !crate::http_file::is_http_file(&path) {
            self.status = String::from("Not a .http/.rest file");
            return;
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        let envs = crate::http_file::load_env(dir, None).environments;
        if envs.is_empty() {
            self.status = format!(
                "No named environments in {} beside this file",
                crate::http_file::ENV_FILES.join(" or ")
            );
            return;
        }
        let current = self.http_environment.clone().unwrap_or_default();
        let rows: Vec<ListRow> = std::iter::once(String::new())
            .chain(envs)
            .map(|id| {
                let name = if id.is_empty() {
                    format!("No Environment ({} only)", crate::http_file::SHARED_ENV)
                } else {
                    id.clone()
                };
                let mark = if id == current { "  (current)" } else { "" };
                ListRow {
                    label: format!("{name}{mark}"),
                    id,
                }
            })
            .collect();
        let mut picker = ListPicker::new(ListPurpose::HttpEnvironment, "HTTP Environment", rows);
        picker.selected = picker
            .rows
            .iter()
            .position(|r| r.id == current)
            .unwrap_or(0);
        self.open_list_picker(picker, "");
    }

    /// Fold in a finished `.http` request (#370): record it in the durable
    /// command history (raw line, status as the exit, elapsed as the
    /// duration), materialise the response under the cache, and open it —
    /// a text document with status/headers as comments, or the image
    /// itself. Returns whether a repaint is due.
    pub fn drain_http_responses(&mut self) -> bool {
        let Some(rx) = self.http_run.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.http_run = None;
                self.status = String::from("HTTP: the request worker died");
                return true;
            }
        };
        self.http_run = None;
        let cwd = outcome
            .file
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match outcome.result {
            Err(e) => {
                self.command_history
                    .append(crate::command_history::HistoryEntry {
                        cmd: format!("http {}", outcome.raw_line),
                        cwd,
                        host: String::new(),
                        exit: None,
                        dur_ms: 0,
                        ts,
                    });
                self.status = format!("HTTP: {} failed: {e}", outcome.raw_line);
            }
            Ok(resp) => {
                self.command_history
                    .append(crate::command_history::HistoryEntry {
                        cmd: format!("http {}", outcome.raw_line),
                        cwd,
                        host: String::new(),
                        exit: Some(i32::from(resp.status)),
                        dur_ms: resp.elapsed_ms,
                        ts,
                    });
                let dir = croft_cache_dir().join("http");
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    self.status = format!("HTTP: cannot write the response: {e}");
                    return true;
                }
                let slug: String = outcome
                    .raw_line
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect::<String>()
                    .trim_matches('-')
                    .chars()
                    .take(48)
                    .collect();
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let (file, bytes): (PathBuf, Vec<u8>) =
                    match crate::http_file::response_kind(&resp.content_type()) {
                        crate::http_file::ResponseKind::Image(ext) => {
                            (dir.join(format!("{slug}-{stamp}.{ext}")), resp.body.clone())
                        }
                        crate::http_file::ResponseKind::Text(_) => {
                            let (ext, doc) =
                                crate::http_file::response_doc(&outcome.raw_line, &resp);
                            (dir.join(format!("{slug}-{stamp}.{ext}")), doc.into_bytes())
                        }
                    };
                if let Err(e) = std::fs::write(&file, &bytes) {
                    self.status = format!("HTTP: cannot write the response: {e}");
                    return true;
                }
                match self.editor.open_pinned(&file) {
                    Ok(()) => {
                        self.focus_pane(Pane::Editor);
                        self.status = format!(
                            "{} \u{2192} {} {} \u{00b7} {} ms",
                            outcome.raw_line, resp.status, resp.status_text, resp.elapsed_ms
                        );
                    }
                    Err(e) => self.status = format!("HTTP: response open failed: {e}"),
                }
            }
        }
        true
    }

    /// The confirm popup for runnable fence `idx` of the current document.
    /// EVERY block confirms (#353): a README from a cloned repo is untrusted
    /// input, and a substring matcher cannot tell a benign block from a
    /// disguised one; the matcher only decides whether the popup is red.
    pub(super) fn pending_run_block_for(
        &self,
        idx: usize,
        block: &crate::markdown::MdRunnable,
    ) -> PendingRunBlock {
        let doc = self.editor.path.clone();
        // The pane is named by the document's path under the workspace, so
        // two READMEs in one tree do not share `README.md:1`.
        let root = self.active_workspace_root();
        let file = doc
            .as_ref()
            .map(|p| {
                p.strip_prefix(&root)
                    .map(|rel| rel.display().to_string())
                    .unwrap_or_else(|_| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| String::from("doc"))
                    })
            })
            .unwrap_or_else(|| String::from("doc"));
        let cwd = if block.cwd_root {
            self.active_workspace_root()
        } else {
            doc.as_ref()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.active_workspace_root())
        };
        PendingRunBlock {
            pane_name: format!("{file}:{}", idx + 1),
            cwd,
            command: fence_command(
                block.interpreter,
                &block.code,
                &block_script_dir(),
                // The shell `run_block_in_pane` spawns, or spawned the pane
                // it reuses.
                &std::env::var("SHELL").unwrap_or_default(),
            ),
            code: block.code.clone(),
            destructive: block.destructive,
            doc: doc.clone(),
            block_line: block.lines.0,
            persist: block.persist,
            capture_timeout: block.capture_timeout.map(std::time::Duration::from_secs),
        }
    }

    /// A click on runnable fence `idx` of the open preview: park it behind
    /// the confirm popup.
    pub(super) fn run_markdown_block(&mut self, idx: usize) {
        let Some(md) = self.editor.markdown_preview.as_ref() else {
            return;
        };
        let Some(block) = md.runnables.get(idx).cloned() else {
            return;
        };
        // A notebook cell runs in its kernel straight away, as in Jupyter:
        // no pane, no confirm.
        if let Some(cell) = block.kernel_cell {
            let text = self.editor.lines.join("\n");
            if let Some(cell) = crate::notebook_kernel::code_cell(&text, cell) {
                self.run_notebook_cells(vec![cell]);
            }
            return;
        }
        self.pending_run_block = Some(self.pending_run_block_for(idx, &block));
    }

    /// Send `cells` to the active notebook's kernel, starting it on the
    /// first run. Each cell's old outputs are cleared in one edit, so what
    /// the file shows is always this run's: now when the kernel is up,
    /// else once it is (`NotebookRun::fold`), so a kernel that fails to
    /// start leaves the notebook as it was (#878).
    pub(super) fn run_notebook_cells(&mut self, cells: Vec<crate::notebook_kernel::CellRef>) {
        use crate::notebook_kernel::{CellEdit, NotebookRun, apply, kernel_name};
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        if cells.is_empty() {
            self.status = String::from("No code cells to run");
            return;
        }
        let mut text = self.editor.lines.join("\n");
        let starting = !self.notebook_kernels.contains_key(&path);
        if starting {
            let session = self.start_kernel_session(&path, &kernel_name(&text));
            self.notebook_kernels
                .insert(path.clone(), NotebookRun::new(session));
        }
        let Some(run) = self.notebook_kernels.get_mut(&path) else {
            return;
        };
        let up = run.kernel.is_some();
        for cell in cells {
            if up && let Some(next) = apply(&text, &cell, CellEdit::Begin) {
                text = next;
            }
            run.execute(cell);
        }
        let running = run.running_indices();
        if up {
            self.editor
                .replace_all_lines(text.split('\n').map(str::to_string).collect());
        }
        self.editor.set_notebook_running(running);
        if starting {
            self.status = String::from("Starting kernel…");
        }
    }

    /// Fold what each kernel said into its notebook. A notebook that is not
    /// the active tab keeps its events until it is.
    pub(super) fn poll_notebook_kernels(&mut self) -> bool {
        let active = self.editor.path.clone();
        let mut changed = self.poll_kernel_listing();
        let mut choice = None;
        let mut failed = None;
        for (path, run) in self.notebook_kernels.iter_mut() {
            if !run.collect() || active.as_ref() != Some(path) {
                continue;
            }
            let text = self.editor.lines.join("\n");
            let (next, notes) = run.fold(&text);
            if let Some(next) = next {
                self.editor
                    .replace_all_lines(next.split('\n').map(str::to_string).collect());
            }
            self.editor.set_notebook_running(run.running_indices());
            if let Some(note) = notes.last() {
                self.status = note.clone();
            }
            if let Some(c) = run.choice.take() {
                choice = Some((path.clone(), c));
            }
            if run.ended() {
                failed = Some(path.clone());
            }
            changed = true;
        }
        // A kernel that never started is forgotten (#878), so the next run
        // starts a new one and finds an ipykernel installed since.
        if let Some(path) = failed {
            self.notebook_kernels.remove(&path);
        }
        if let Some((path, c)) = choice {
            self.open_kernel_picker(path, c.specs, Some(c.requested));
        }
        changed
    }

    /// Notebook: Select Kernel (#1377): ask the bridge which kernels are
    /// installed; the picker opens when it answers.
    pub(super) fn select_notebook_kernel(&mut self) {
        let Some(path) = self.editor.path.clone().filter(|_| {
            self.editor
                .markdown_preview
                .as_ref()
                .is_some_and(|m| m.notebook)
        }) else {
            self.status = String::from("Select Kernel works on a notebook's rendered view");
            return;
        };
        let listing = self.start_kernel_session(&path, crate::notebook_kernel::LIST_KERNELS);
        self.kernel_listing = Some((path, listing));
        self.status = String::from("Looking for installed kernels…");
    }

    /// A kernel bridge for the notebook at `path`, run in its folder.
    pub(super) fn start_kernel_session(
        &self,
        path: &Path,
        kernel: &str,
    ) -> crate::notebook_kernel::KernelSession {
        #[cfg(test)]
        if let Some(start) = &self.kernel_start_override {
            return start(kernel);
        }
        let root = self.workspace_root();
        let dir = path.parent().unwrap_or(root);
        crate::notebook_kernel::KernelSession::start(root, dir, kernel)
    }

    /// The installed kernels arriving for Select Kernel.
    pub(super) fn poll_kernel_listing(&mut self) -> bool {
        use crate::notebook_kernel::Event;
        let Some((path, listing)) = self.kernel_listing.as_ref() else {
            return false;
        };
        let path = path.clone();
        for event in listing.try_events() {
            match event {
                Event::Specs { specs } => {
                    self.kernel_listing = None;
                    if self.editor.path.as_ref() == Some(&path) {
                        self.status.clear();
                        self.open_kernel_picker(path, specs, None);
                    }
                    return true;
                }
                Event::Error { message } => {
                    self.kernel_listing = None;
                    self.status = format!("Kernel: {message}");
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// The kernel picker for the notebook at `path`, opened on its own
    /// kernel when installed, else on one for its language. `missing` is
    /// the kernel it names that this machine does not have.
    pub(super) fn open_kernel_picker(
        &mut self,
        path: PathBuf,
        specs: Vec<crate::notebook_kernel::KernelSpec>,
        missing: Option<String>,
    ) {
        use crate::notebook_kernel::{kernel_name, notebook_language, preferred_kernel};
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        if self.editor.path.as_ref() != Some(&path) {
            return;
        }
        let text = self.editor.lines.join("\n");
        let rows = specs
            .iter()
            .map(|s| ListRow {
                id: s.name.clone(),
                label: format!("{}  ({})", s.display, s.name),
            })
            .collect();
        let title = match &missing {
            Some(k) => format!("Kernel '{k}' is not installed: Select Kernel"),
            None => String::from("Select Kernel"),
        };
        let mut picker = ListPicker::new(ListPurpose::NotebookKernel, title, rows);
        picker.selected = preferred_kernel(
            &specs,
            &kernel_name(&text),
            notebook_language(&text).as_deref(),
        );
        self.kernel_choice = Some((path, specs, missing));
        self.open_list_picker(picker, "No Jupyter kernels are installed");
    }

    /// The kernel picked for a notebook: written to its
    /// `metadata.kernelspec` (Save keeps it, as in Jupyter), and a kernel
    /// already started, or waiting on a missing one, is replaced by it.
    pub(super) fn choose_notebook_kernel(&mut self, name: &str) {
        let Some((path, specs, _)) = self.kernel_choice.take() else {
            return;
        };
        let Some(spec) = specs.into_iter().find(|s| s.name == name) else {
            return;
        };
        if self.editor.path.as_ref() != Some(&path) {
            return;
        }
        let text = self.editor.lines.join("\n");
        if let Some(next) = crate::notebook_kernel::set_kernelspec(&text, &spec) {
            self.editor
                .replace_all_lines(next.split('\n').map(str::to_string).collect());
        }
        if !self.notebook_kernels.contains_key(&path) {
            self.status = format!("Kernel: {} (starts on the next run)", spec.display);
            return;
        }
        let session = self.start_kernel_session(&path, &spec.name);
        if let Some(run) = self.notebook_kernels.get_mut(&path) {
            run.restart_with(session);
            self.editor.set_notebook_running(run.running_indices());
            self.status = format!("Starting kernel {}…", spec.display);
        }
    }

    /// The kernel picker closed without a choice. Cells waiting on a
    /// kernel that is not installed cannot run, so they stop waiting.
    pub(super) fn kernel_choice_dismissed(&mut self) {
        let Some((path, _, Some(missing))) = self.kernel_choice.take() else {
            return;
        };
        if self.notebook_kernels.remove(&path).is_some() && self.editor.path.as_ref() == Some(&path)
        {
            self.editor.set_notebook_running(Vec::new());
        }
        self.status = format!(
            "Kernel '{missing}' is not installed: run Notebook: Select Kernel to pick another"
        );
    }

    /// Interrupt or restart the active notebook's kernel. A kernel whose
    /// bridge is gone takes neither (#878): its cells stop waiting, and a
    /// restart starts a new kernel.
    pub(super) fn notebook_kernel_request(&mut self, request: crate::notebook_kernel::Request) {
        use crate::notebook_kernel::{NotebookRun, Request, kernel_name};
        let Some(path) = self
            .editor
            .path
            .clone()
            .filter(|p| self.notebook_kernels.contains_key(p))
        else {
            self.status = String::from("This notebook has no running kernel");
            return;
        };
        let restart = matches!(request, Request::Restart);
        if self
            .notebook_kernels
            .get(&path)
            .is_some_and(|run| run.send(request))
        {
            return;
        }
        self.notebook_kernels.remove(&path);
        self.editor.set_notebook_running(Vec::new());
        if restart {
            let text = self.editor.lines.join("\n");
            let session = self.start_kernel_session(&path, &kernel_name(&text));
            self.notebook_kernels
                .insert(path, NotebookRun::new(session));
            self.status = String::from("Starting kernel…");
        } else {
            self.status = String::from("The kernel had stopped: the next run starts it again");
        }
    }

    /// Cmd+Enter in a Markdown SOURCE buffer (#353): the fence under the
    /// caret, found by the parser's own source ranges (the same walk the
    /// preview uses, so the block number - and its pane - agree).
    /// `report_miss`: a Markdown source caret outside every runnable fence
    /// sets guidance and counts as handled (the palette command); without
    /// it that case returns false so the key can fall through (#1571).
    pub(super) fn run_fence_at_cursor(&mut self, report_miss: bool) -> bool {
        let is_markdown = self
            .editor
            .path
            .as_deref()
            .and_then(|p| p.extension().and_then(|e| e.to_str()))
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown"));
        if self.editor.markdown_preview.is_some() || !is_markdown {
            return false;
        }
        let text = self.editor.lines.join("\n");
        let (_, _, runnables) = crate::markdown::render_markdown_full(
            &text,
            self.theme,
            &mut crate::highlight::LangRegistry::new(),
            None,
            crate::markdown::BlockOutputs::new(),
        );
        let row = self.editor.cursor_row;
        let Some((idx, block)) = runnables
            .iter()
            .enumerate()
            .find(|(_, r)| row >= r.lines.0 && row < r.lines.1)
        else {
            if !report_miss {
                return false;
            }
            self.status = String::from(
                "Run code block: put the caret inside a runnable shell fence (sh/bash/zsh/fish; not {run=false})",
            );
            return true;
        };
        self.pending_run_block = Some(self.pending_run_block_for(idx, block));
        true
    }

    /// Type a fence into its pane (#353): the pane named after the document
    /// and block, reused when it sits idle at a prompt in the block's
    /// directory, else a fresh one there. The block is typed, not spawned,
    /// so the user sees it and shell history records it.
    pub(super) fn run_block_in_pane(&mut self, block: PendingRunBlock) {
        // The line cleared first, as tasks do: an idle shell's line editor
        // may hold a half-typed command.
        let bytes = format!(
            "{}{}",
            crate::widgets::terminal::CLEAR_TYPED_LINE,
            block.command
        );
        // Same guard as `run_project_task`, and the SAME predicate rather
        // than a second copy: `README.md:1` is a likely name across two
        // repos opened in sequence, and a pane whose shell still sits in
        // the other repo must not receive this one's block (#430).
        let root = block
            .cwd
            .canonicalize()
            .unwrap_or_else(|_| block.cwd.clone());
        // `pane_is_idle_at` is main's shared predicate (#430): one
        // definition for tasks and for runnable blocks, so a correction to
        // the cwd rule cannot land on one and miss the other.
        if let Some(idx) = self
            .terminals
            .iter()
            .position(|t| pane_is_idle_at(t, &block.pane_name, &root))
        {
            self.arm_block_capture(&block, idx);
            self.reveal_terminal_pane(idx);
            self.terminals[idx].write_input(bytes.as_bytes());
            self.show_terminal = true;
            self.focus_pane(Pane::Terminal);
            self.status = format!("Running {}", block.pane_name);
            let uid = self.terminals[idx].uid();
            self.await_pane_outcome(uid, block.pane_name);
            return;
        }
        match crate::widgets::terminal::PtyTerminal::new(&block.cwd) {
            Ok(mut term) => {
                term.set_manual_name(Some(block.pane_name.clone()));
                term.write_input(bytes.as_bytes());
                let uid = term.uid();
                self.insert_terminal(term);
                // A fresh pane has no decorations yet, so the capture arms
                // at zero and the block's own command is the first to
                // finish. Armed AFTER the insert, since that is what puts
                // the pane in `self.terminals` for the settle to find.
                if let Some(idx) = self
                    .terminals
                    .iter()
                    .position(|t| t.label() == block.pane_name)
                {
                    self.arm_block_capture(&block, idx);
                }
                self.status = format!("Running {}", block.pane_name);
                self.await_pane_outcome(uid, block.pane_name);
            }
            Err(e) => {
                self.status = format!("Could not start a pane for {}: {e}", block.pane_name);
            }
        }
    }

    pub(super) fn confirm_pending_run_block(&mut self) {
        if let Some(block) = self.pending_run_block.take() {
            self.run_block_in_pane(block);
        }
    }

    pub(super) fn cancel_pending_run_block(&mut self) {
        self.pending_run_block = None;
        self.status = String::from("Block not run");
    }

    /// Start waiting for `block`'s output on pane `idx` (#354).
    ///
    /// The pane's CURRENT decoration count is recorded, so the capture is
    /// the first command to finish after this moment rather than whatever
    /// the pane happened to run before. Re-running a block replaces its
    /// pending capture: the newest run is the one the preview describes.
    pub(super) fn arm_block_capture(&mut self, block: &PendingRunBlock, idx: usize) {
        // The newest id the pane has already finished. Ids are monotonic,
        // so "greater than this" is exactly "after the block was typed",
        // and it stays true however many marks are collected in between.
        let after = self.terminals[idx]
            .command_decorations()
            .iter()
            .map(|d| d.id)
            .max();
        self.pending_captures
            .retain(|c| c.pane_name != block.pane_name || c.block_line != block.block_line);
        self.pending_captures.push(PendingCapture {
            pane_name: block.pane_name.clone(),
            doc: block.doc.clone(),
            block_line: block.block_line,
            after,
            started: std::time::Instant::now(),
            timeout: block.capture_timeout,
            persist: block.persist,
        });
    }

    /// Write a capture into the document as an output fence (#354).
    ///
    /// The fence goes immediately after the block's closer, marked with an
    /// HTML comment so a later run can find and REPLACE it rather than
    /// stacking a new one each time. Exactly one output fence per block, and
    /// no other fence is touched: the search starts at the block's closer
    /// and stops at the first line that is not part of the marked pair.
    ///
    /// This writes to the user's file from a preview action, so it is
    /// deliberately narrow. It refuses rather than guesses when the document
    /// on disk no longer looks like the one the block came from, since the
    /// block's line numbers are only meaningful against the text that
    /// produced them.
    pub(super) fn persist_block_output(
        &mut self,
        path: &Path,
        block_line: usize,
        out: &crate::markdown::BlockOutput,
    ) {
        let Ok(text) = std::fs::read_to_string(path) else {
            self.status = format!("Could not read {} to persist the output", path.display());
            return;
        };
        let Some(updated) = crate::markdown::replace_output_fence(&text, block_line, &out.text)
        else {
            self.status =
                String::from("The document changed since the block ran; output not saved");
            return;
        };
        if std::fs::write(path, &updated).is_err() {
            self.status = format!("Could not write the captured output to {}", path.display());
        }
    }

    /// Settle any capture whose command has finished, or whose
    /// `{timeout=N}` has elapsed. Returns true when something changed, so
    /// the caller knows to repaint.
    ///
    /// Deliberately its own pass rather than a branch inside
    /// `drain_terminal_bells`: that function is about notifying, this one
    /// is about recording, and they answer to different pane states.
    pub(super) fn settle_block_captures(&mut self) -> bool {
        if self.pending_captures.is_empty() {
            return false;
        }
        let mut settled: Vec<(
            String,
            usize,
            crate::markdown::BlockOutput,
            Option<PathBuf>,
            bool,
        )> = Vec::new();
        self.pending_captures.retain_mut(|c| {
            let Some(term) = self.terminals.iter().find(|t| t.label() == c.pane_name) else {
                // The pane is gone, so nothing will ever finish on it.
                return false;
            };
            let decorations = term.command_decorations();
            // The first command to finish AFTER the block was typed, by id
            // rather than by position (#440). Ids are monotonic and never
            // renumbered, so this survives any amount of mark collection:
            // an evicted command is simply absent rather than silently
            // replaced by its neighbour, which is what the old positional
            // index could not distinguish.
            // The EARLIEST command to finish after the block was typed.
            // Earliest rather than latest because a block that finishes
            // while a later one is still running must not be described by
            // the later one - but see `PendingCapture::after`: earliest is
            // also what makes a foreign command completing in the
            // arm-to-keystroke gap win, which is this rule's residual and
            // not `after`'s.
            let mine = decorations
                .iter()
                .filter(|d| c.after.is_none_or(|a| d.id > a))
                .min_by_key(|d| d.id);
            if let Some(d) = mine {
                settled.push((
                    c.pane_name.clone(),
                    c.block_line,
                    crate::markdown::BlockOutput {
                        text: term.command_output_text(d),
                        exit: d.exit,
                        timed_out: false,
                    },
                    c.doc.clone(),
                    // Unconditional now. The gate existed because a
                    // positional index could name a stranger; an id cannot.
                    c.persist,
                ));
                return false;
            }
            // The TIMEOUT stops the capture, never the process: the block
            // keeps running in its pane and the box says the wait gave up.
            if c.timeout.is_some_and(|t| c.started.elapsed() >= t) {
                settled.push((
                    c.pane_name.clone(),
                    c.block_line,
                    crate::markdown::BlockOutput {
                        text: String::new(),
                        exit: None,
                        timed_out: true,
                    },
                    c.doc.clone(),
                    c.persist,
                ));
                return false;
            }
            true
        });
        if settled.is_empty() {
            return false;
        }
        for (pane, line, out, doc, persist) in settled {
            if persist
                && !out.timed_out
                && let Some(path) = doc.as_ref()
            {
                self.persist_block_output(path, line, &out);
            }
            self.block_outputs.insert((pane, line), out);
        }
        self.publish_block_outputs();
        true
    }

    /// Hand the editor the captures for the document it is showing (#354).
    ///
    /// The editor has no view of which pane ran what, so the app narrows
    /// `block_outputs` to the current document's pane prefix and copies the
    /// result in. Called after a capture settles and when the preview is
    /// rebuilt, so a box appears without the user touching anything.
    pub(super) fn publish_block_outputs(&mut self) {
        // The common case by far: nothing has been run, so there is nothing
        // to narrow and nothing to compare. Checked before the map is built
        // rather than after, since this runs every tick.
        if self.block_outputs.is_empty() && self.editor.md_outputs.is_empty() {
            return;
        }
        let prefix = self.block_pane_prefix();
        let narrowed: crate::markdown::BlockOutputs = self
            .block_outputs
            .iter()
            .filter(|((pane, _), _)| pane.starts_with(&prefix))
            .map(|((_, line), out)| (*line, out.clone()))
            .collect();
        // Idempotent, so the tick can call it unconditionally and a
        // DOCUMENT SWITCH is picked up without every switch path having to
        // remember to. Keying only by source line meant `B.md`'s line-12
        // fence rendered `A.md`'s capture until something else rebuilt the
        // preview; narrowing by pane prefix fixes that only if it actually
        // re-runs, which is what this comparison buys.
        if narrowed == self.editor.md_outputs {
            return;
        }
        self.editor.md_outputs = narrowed;
        if self.editor.markdown_preview.is_some() {
            self.editor.rebuild_markdown_preview();
        }
    }

    /// The pane-name prefix for the document the editor is showing: the
    /// same `<file>:` the run path builds its pane names from, so a capture
    /// from another document cannot appear under this one's fences.
    pub(super) fn block_pane_prefix(&self) -> String {
        let root = self.active_workspace_root();
        let file = self
            .editor
            .path
            .as_ref()
            .map(|p| {
                p.strip_prefix(&root)
                    .map(|rel| rel.display().to_string())
                    .unwrap_or_else(|_| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| String::from("doc"))
                    })
            })
            .unwrap_or_else(|| String::from("doc"));
        format!("{file}:")
    }

    /// Arm or disarm Live Run for the active file (Cmd+K V).
    pub(crate) fn toggle_live_run(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Live Run: save the buffer as a .py file first");
            return;
        };
        if !crate::live_run::supports(&path) {
            self.status = String::from("Live Run runs Python files (.py)");
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.live_run_files.remove(&path) {
            self.live_run_sent.remove(&path);
            self.for_each_tab_of(&path, |tab| tab.live_run = None);
            self.status = format!("Live Run: off for {name}");
            return;
        }
        self.live_run_files.insert(path);
        self.live_run_runner
            .get_or_insert_with(crate::live_run::Runner::spawn);
        self.status = format!("Live Run: on for {name}, re-running on every pause in typing");
    }

    /// Apply `f` to every open tab of `path`, in every editor group.
    pub(super) fn for_each_tab_of(
        &mut self,
        path: &Path,
        mut f: impl FnMut(&mut crate::widgets::editor::Editor),
    ) {
        for tab in self.editor.editors.iter_mut() {
            if tab.path.as_deref() == Some(path) {
                f(tab);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for tab in group.editors.iter_mut() {
                if tab.path.as_deref() == Some(path) {
                    f(tab);
                }
            }
        }
    }

    /// Live Run's per-tick work: land finished runs on their tabs, then send
    /// the active buffer for a re-run once typing has paused. Returns true
    /// when anything on screen changed.
    pub fn tick_live_run(&mut self) -> bool {
        if self.live_run_files.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut landed = Vec::new();
        if let Some(runner) = &self.live_run_runner {
            while let Some(done) = runner.try_recv() {
                landed.push(done);
            }
        }
        for done in landed {
            // A run that lands after its file was disarmed is dropped: the
            // user turned it off and must not see it come back.
            if !self.live_run_files.contains(&done.path) {
                continue;
            }
            let active = self.editor.path.as_deref() == Some(done.path.as_path());
            match done.outcome {
                Ok(report) => {
                    if active {
                        self.status = report.summary.clone();
                    }
                    let view = crate::live_run::View {
                        lines: done.lines,
                        report,
                    };
                    self.for_each_tab_of(&done.path, |tab| tab.live_run = Some(view.clone()));
                }
                Err(msg) => {
                    if active {
                        self.status = msg;
                    }
                }
            }
            changed = true;
        }
        let tab = &*self.editor;
        let Some(path) = tab.path.clone() else {
            return changed;
        };
        if !self.live_run_files.contains(&path)
            || tab.has_non_text_view()
            || tab
                .last_edit_at
                .is_some_and(|t| t.elapsed() < crate::live_run::DEBOUNCE)
            || self.live_run_sent.get(&path) == Some(&tab.lines)
        {
            return changed;
        }
        let lines = tab.lines.clone();
        let root = self.roots.primary().to_path_buf();
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let python = project_python_for(&dir, &root);
        self.live_run_sent.insert(path.clone(), lines.clone());
        if let Some(runner) = &self.live_run_runner {
            runner.submit(crate::live_run::Job {
                path,
                lines,
                python,
            });
        }
        changed
    }
}

/// What Problems: Check Whole Project runs (#256, #1291).
pub(super) enum ProjectCheck {
    /// Each `(program, args)` in turn, with their output merged.
    Run(Vec<(std::ffi::OsString, Vec<String>)>),
    /// The root tsconfig names no files and no references: tsc would
    /// compile nothing and exit clean.
    Empty,
}

/// The command typed into a pane for a fenced block (#353): a one-line shell
/// block as written (a trailing newline runs it). Any other language
/// is written to a private file under `scripts` and read from it on stdin,
/// `python3 - < 'file'` (#1630): the heredoc it used to be typed as is
/// POSIX shell syntax, so with fish as the pane's shell every python or
/// node block failed. A redirect from a quoted path reads the same in bash,
/// zsh and fish, and the block reaches the interpreter byte for byte, tabs
/// and `!` included. The interpreter still reads stdin, so the block runs
/// with the pane's directory on its import path, as before. Should the file
/// not be written, the heredoc is the fallback.
///
/// A shell block of several lines, or holding a tab, `!` or `#`, goes as one
/// single-quoted `printf '%b'` argument instead. Typed line by line, each
/// line was a command of its own, so the capture under the fence and the
/// pane's outcome described only the first one to finish (#920); as one
/// command the shell reports the whole block: every line's output and the
/// status it ends with. Typed into an interactive shell, a tab is
/// completion and bash expands `!` as history, so what ran was not what
/// the confirm popup showed. Inside single quotes neither happens, and the
/// tab travels as `\t`. zsh reads a typed `#` as a word, not a comment,
/// unless `interactive_comments` is set, and it is off by default: `echo
/// done  # note` printed the note and a comment alone failed with exit 127
/// (#1367). Text `eval`ed or `source`d is a script, where every shell reads
/// a comment as one, and a `#` that is not a comment parses as typed. A
/// backslash travels as `\0134`, since fish folds `\\` inside single
/// quotes to one backslash where bash and zsh keep both.
/// `shell` is the pane's shell: fish before 3.4 rejects `$(...)` as
/// syntax, so a fish pane `source`s the decoded block from a `psub` file
/// rather than `eval`ing it, which every fish reads alike. Not piped to
/// `source`: the block's own stdin would be the pipe, and a `read` or an
/// install's "continue? [Y/n]" would get nothing from the pane.
pub(super) fn fence_command(interpreter: &str, code: &str, scripts: &Path, shell: &str) -> String {
    let code = code.trim_end_matches('\n');
    if interpreter != "sh" {
        if let Ok(file) = stage_block(scripts, interpreter, code) {
            return format!("{interpreter} - < '{}'\r", file.display());
        }
        let mut end = String::from("CROFT_BLOCK");
        let mut n = 0u32;
        while code.contains(&end) {
            n += 1;
            end = format!("CROFT_BLOCK_{n}");
        }
        return format!("{interpreter} - <<'{end}'\n{code}\n{end}\r");
    }
    if code.contains(['\n', '\t', '!', '#']) {
        let escaped = format!("{code}\n")
            .replace('\\', "\\0134")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
            .replace('\'', "'\\''");
        // A shell block runs IN the pane's shell, as a typed one does (a
        // `cd` or `export` persists, bash-isms still work): `eval` of the
        // decoded text, or fish's `source` of it.
        if Path::new(shell).file_name().is_some_and(|n| n == "fish") {
            return format!("source (printf '%b' '{escaped}' | psub)\r");
        }
        return format!("eval \"$(printf '%b' '{escaped}')\"\r");
    }
    format!("{code}\r")
}

/// Where [`fence_command`] writes the blocks it runs (#1630). Always
/// absolute: the typed command runs in the pane's directory, so a relative
/// cache (HOME unset or relative) would name a file that is not there.
pub(super) fn block_script_dir() -> PathBuf {
    // Never the user's own cache from a test.
    let dir = if cfg!(test) {
        std::env::temp_dir().join(format!("croft-test-blocks-{}", std::process::id()))
    } else {
        croft_cache_dir().join("blocks")
    };
    absolute_block_dir(dir)
}

/// `dir` made absolute against croft's own directory, where it was meant.
pub(super) fn absolute_block_dir(dir: PathBuf) -> PathBuf {
    std::path::absolute(&dir).unwrap_or(dir)
}

/// How long a written block is kept: long enough for its interpreter to
/// have read it, short enough that the directory never grows.
pub(super) const BLOCK_SCRIPT_TTL: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// Write `code` and a final newline to a new file under `dir` that only the
/// user can read, for `interpreter` (#1630). Refuses a path the typed
/// command could not quote in single quotes. Blocks older than
/// [`BLOCK_SCRIPT_TTL`] are cleared first.
pub(super) fn stage_block(dir: &Path, interpreter: &str, code: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > BLOCK_SCRIPT_TTL);
            if old && entry.file_name().to_string_lossy().starts_with("block-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let ext = match interpreter {
        "python3" => "py",
        "node" => "js",
        _ => "txt",
    };
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!(
        "block-{}-{seq}-{}.{ext}",
        std::process::id(),
        now_millis()
    ));
    if path.to_string_lossy().contains(['\'', '\\', '\n', '\r']) {
        return Err(std::io::Error::other("the block path cannot be quoted"));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(format!("{code}\n").as_bytes())?;
    Ok(path)
}

/// The suite that is the file `path` for `runner` (#1535: Run Tests in
/// Current File), in the runner's own naming: pytest takes the file's path
/// as a node-ID prefix; cargo a source file's module path (`src/a/b.rs` and
/// `src/a/b/mod.rs` are `a::b`); go the
/// file's package directory; CodeQL the test file itself. `None` for a file
/// the runner cannot select alone: a cargo crate root (its tests carry no
/// module prefix to filter on), a binary under `src/bin` or an integration
/// test (each its own test binary, which the name filter cannot pick), any
/// jest or vitest file
/// (their suites are describe titles, not files), and a file outside `root`.
pub(super) fn file_test_suite(
    runner: crate::testing::worker::Runner,
    root: &Path,
    path: &Path,
) -> Option<String> {
    use crate::testing::worker::Runner;
    let rel = path.strip_prefix(root).ok()?;
    let rel_str = rel.to_str()?.replace('\\', "/");
    match runner {
        Runner::Pytest | Runner::Codeql => Some(rel_str),
        Runner::Go => {
            let dir = rel.parent().and_then(Path::to_str).unwrap_or("");
            Some(if dir.is_empty() {
                String::from(".")
            } else {
                format!("./{}", dir.replace('\\', "/"))
            })
        }
        Runner::Cargo => {
            // Relative to the package the file belongs to: in a workspace,
            // `crates/foo/src/parse.rs` is module `parse`.
            let in_package = cargo_package_relative(root, path)?;
            let in_src = in_package.strip_prefix("src/")?.strip_suffix(".rs")?;
            let module = in_src.strip_suffix("/mod").unwrap_or(in_src);
            // A crate root's tests have no module prefix to filter on: a
            // `tests::` filter is a substring, and would sweep every
            // module's `tests` too.
            match module {
                "main" | "lib" => None,
                _ if module.starts_with("bin/") => None,
                _ => Some(module.replace('/', "::")),
            }
        }
        Runner::Vitest | Runner::Jest => None,
    }
}

/// `path` relative to the nearest package directory above it, up to
/// `root`: the closest ancestor holding a `Cargo.toml` with the file under
/// its `src/`, or `root` itself.
pub(super) fn cargo_package_relative(root: &Path, path: &Path) -> Option<String> {
    let rel_to = |dir: &Path| {
        path.strip_prefix(dir)
            .ok()
            .and_then(Path::to_str)
            .map(|s| s.replace('\\', "/"))
    };
    for dir in path.ancestors().skip(1) {
        if dir == root || !dir.starts_with(root) {
            break;
        }
        if let Some(rel) = rel_to(dir)
            && rel.starts_with("src/")
            && dir.join("Cargo.toml").is_file()
        {
            return Some(rel);
        }
    }
    rel_to(root)
}

/// Whether `t` is the idle pane named `pane_name` whose shell is standing
/// at `root` — or whose directory croft cannot read at all. The one
/// predicate behind every "reuse a pane rather than stack a new one"
/// decision (#430).
///
/// Shared deliberately. This guard existed as two copies, and when #392
/// fixed the runnable-fence copy the task copy kept both defects: a
/// `starts_with` that accepted any DESCENDANT, so a shell that had `cd`'d
/// into a subdirectory was reused and the command ran there; and an
/// `is_none_or` that opened the guard whenever the cwd could not be read.
/// One predicate means a future correction cannot land on one caller and
/// miss the other.
///
/// The comparison is exact on purpose. Both sides are resolved paths — the
/// kernel reports the shell's cwd fully resolved, and `canonicalize()`
/// resolves symlinks and never emits a trailing separator — so equality
/// holds wherever reuse is genuinely wanted, including through a
/// `/tmp` -> `/private/tmp` symlink. When it does not hold, the caller
/// opens a fresh pane at the right directory, so the worst case is a spare
/// pane rather than a command run somewhere the user was not told about.
///
/// When `canonicalize()` itself fails — a root deleted or renamed under a
/// live session — `unwrap_or` leaves an unresolved path and the comparison
/// cannot match, which costs a spare pane and never a misdirected command.
///
/// The cwd decision itself lives in [`cwd_matches`], which distinguishes a
/// shell that moved from a platform that cannot say where it is.
///
/// A DEAD shell never reaches that decision: `foreground_is_shell` is
/// checked first and answers false for one (#155), so the unreadable arm
/// only ever sees panes croft cannot ask about. Relaxing that check would
/// make identity-fallback start reusing corpses, so the two belong
/// together.
pub(super) fn pane_is_idle_at(
    t: &crate::widgets::terminal::PtyTerminal,
    pane_name: &str,
    root: &Path,
) -> bool {
    t.label() == pane_name
        && t.foreground_is_shell()
        && cwd_matches(t.kernel_shell_cwd().as_deref(), root, cwd_is_observable())
}

/// The cwd half of [`pane_is_idle_at`], as a pure function so both arms are
/// testable on any platform (#430).
///
/// Three cases, not two. A cwd croft READ and that differs is a shell that
/// wandered off — refuse. A cwd croft read and that matches is this
/// command's pane — reuse. A cwd croft could not read splits on whether
/// the platform can read one at all: where it can, `None` is the failure
/// the guard exists for and stays closed; where it cannot (Android), a
/// closed guard would stack a new pane on every single run, so reuse falls
/// back to the pane's identity, which is all croft had before the guard
/// existed.
pub(super) fn cwd_matches(cwd: Option<&Path>, root: &Path, observable: bool) -> bool {
    match cwd {
        Some(c) => c == root,
        None => !observable,
    }
}

/// The status a run gesture gets while another run is in flight (#1534).
pub(super) const TEST_RUN_IN_PROGRESS: &str = "A test run is in progress (s in Testing stops it)";

/// `task` with `answers` for its `${input:…}` variables (#1424).
pub(super) fn with_answers(
    mut task: crate::tasks::Task,
    answers: &std::collections::BTreeMap<String, String>,
) -> crate::tasks::Task {
    if let Some(v) = task.vscode.as_mut() {
        v.answers
            .extend(answers.iter().map(|(k, a)| (k.clone(), a.clone())));
    }
    task
}

/// ` (did you mean "npm run build"?)` for a task label that was not found
/// but has a near match (#1412), else nothing.
pub(super) fn did_you_mean(near: Option<String>) -> String {
    near.map(|n| format!(" (did you mean \"{n}\"?)"))
        .unwrap_or_default()
}
