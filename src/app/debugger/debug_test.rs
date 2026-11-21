//! Debug Test: the test at the caret or a named one, the runner for the focused file, its
//! build (`cargo test --no-run`) and launch under lldb-dap, debugpy or delve, and the
//! temporary breakpoint on the assertion the last run failed at.

use super::*;

impl App {
    /// Debug the test the editor caret sits in (Cmd+K Shift+Enter, palette).
    pub(in crate::app) fn debug_test_at_cursor(&mut self) {
        let Some(name) =
            crate::testing::locate::enclosing_fn_name(&self.editor.lines, self.editor.cursor_row)
        else {
            self.status = String::from("Debug Test at Cursor: no function at the caret");
            return;
        };
        self.debug_named_test(name);
    }

    /// The runner for a test in the focused file under `root` (#1408): the
    /// one for the file's language, so a pytest test in a maturin crate
    /// debugs under debugpy, not as a cargo test binary.
    pub(in crate::app) fn test_runner_for_focused_file(
        &self,
        root: &Path,
    ) -> Option<crate::testing::worker::Runner> {
        let runners = crate::testing::worker::runners_for(&self.extensions_dir(), root);
        crate::testing::worker::runner_for_file(&runners, self.editor.path.as_deref())
    }

    /// Hand the test `name` to the debugger — the M6 wedge: pytest runs as a
    /// debugpy module launch under the PROJECT's interpreter, a cargo test
    /// binary launches under lldb-dap with the name as its libtest filter.
    /// Shared by debug-at-cursor and Alt+click on the gutter play glyph.
    pub(in crate::app) fn debug_named_test(&mut self, name: String) {
        if !self.debug_sessions.is_empty() {
            self.status = String::from("A debug session is already running (Shift+F5 stops it)");
            return;
        }
        self.launch_with_unsaved_saved(|app| app.debug_named_test_now(name));
    }

    pub(in crate::app) fn debug_named_test_now(&mut self, name: String) {
        use std::collections::BTreeMap;
        let root = self.tree.root.clone();
        // #373: if the last run of this test failed and named a place in
        // the user's own code, break there — the whole point of "debug this
        // test" is landing on the assertion without being asked to find it
        // first. Set BEFORE the launch set is collected so it travels with
        // the user's own breakpoints, and recorded so it can be taken back
        // out when the session ends.
        self.arm_failure_breakpoint(&name, &root);
        let breakpoints: BTreeMap<PathBuf, Vec<crate::dap::session::SourceBreakpoint>> = self
            .editor
            .breakpoints
            .iter()
            .map(|(p, lines)| (p.clone(), self.editor.source_breakpoints(p, lines)))
            .collect();
        match self.test_runner_for_focused_file(&root) {
            Some(crate::testing::worker::Runner::Pytest) => {
                // The interpreter F5 and Live Run pick, from the test file's
                // folder up (#864), not only a root `.venv`; refused before
                // any setup when debugpy cannot run under it.
                let test_dir = self
                    .editor
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .filter(|dir| dir.starts_with(&root))
                    .unwrap_or(&root)
                    .to_path_buf();
                let project_py =
                    debuggee_python(&test_dir, &root, std::env::var_os("PATH").as_deref());
                let project_found = project_py.is_some();
                let adapter_py = match crate::dap::install::ensure_debug_venv() {
                    Ok(py) => py,
                    Err(e) => {
                        self.debug_error(format!("Debugger setup failed: {e}"));
                        return;
                    }
                };
                let project_py = project_py.unwrap_or_else(|| adapter_py.clone());
                let mut request =
                    crate::dap::session::pytest_debug_launch_request(&project_py, &root, &name);
                // A launch.json config for debugging tests (#1398) brings
                // its env, cwd, args and debugpy settings, as in VS Code.
                let configs = crate::dap::configs::discover_configs(&root);
                let test_config = crate::dap::configs::debug_test_config(&configs);
                if let Some(cfg) = test_config {
                    let ctx = crate::dap::configs::SubstCtx {
                        workspace_folder: root.clone(),
                        file: self.editor.path.clone(),
                        inputs: cfg.answers.clone(),
                        folders: self.workspace_folders(),
                    };
                    match crate::dap::configs::resolve(cfg, &ctx) {
                        Ok(rc) => {
                            request = crate::dap::configs::with_debug_test_config(request, &rc);
                        }
                        Err(e) => {
                            self.disarm_failure_breakpoint();
                            self.debug_error(e);
                            return;
                        }
                    }
                }
                // Checked once the config has had its say, so a `python`
                // it names is the one validated (#1398).
                let note = match debug_test_python(&request, &root, &adapter_py, project_found)
                    .as_deref()
                    .map_or(Ok(None), debuggee_python_note)
                {
                    Ok(note) => note,
                    Err(refusal) => {
                        self.disarm_failure_breakpoint();
                        self.debug_error(refusal);
                        return;
                    }
                };
                let using = test_config
                    .map(|c| format!(" with \"{}\"", c.name))
                    .unwrap_or_default();
                let adapter_args = vec![String::from("-m"), String::from("debugpy.adapter")];
                match crate::dap::session::DapSession::launch_with(
                    &adapter_py.to_string_lossy(),
                    &adapter_args,
                    &root,
                    request,
                    breakpoints,
                ) {
                    Ok(session) => {
                        self.adopt_debug_session(name.clone(), session, LaunchSlot::Replace);
                        self.run_debug.feedback = Some(format!("Debugging test {name}{using}"));
                        self.run_debug.feedback_is_error = false;
                        let lead = with_python_note(
                            format!("Debugging test {name}{using}"),
                            note.as_deref(),
                        );
                        self.status = self.with_failure_note(format!(
                            "{lead} — F5 continue · F10 step over · Shift+F5 stop"
                        ));
                        self.reveal_debug_view();
                        if note.is_some() {
                            self.debug_console_push(lead);
                        }
                    }
                    Err(e) => self.debug_error(format!("Failed to start debugger: {e}")),
                }
            }
            Some(crate::testing::worker::Runner::Cargo) => {
                if lldb_dap_path().is_none() {
                    self.debug_error(lldb_dap_missing_message());
                    return;
                }
                // The file the gesture happened in narrows the harness: a
                // lib test and an integration test can share a bare fn name.
                let source = self.editor.path.clone();
                self.start_test_binary_build(root, name, source);
            }
            Some(crate::testing::worker::Runner::Go) => {
                self.start_delve_test_session(&root, &name, breakpoints);
            }
            Some(crate::testing::worker::Runner::Vitest | crate::testing::worker::Runner::Jest) => {
                // No session will start, so the armed breakpoint has nothing
                // to be cleaned up by (#373).
                self.disarm_failure_breakpoint();
                self.status =
                    String::from("Debugging JS tests is not wired yet — the play glyph runs them");
            }
            Some(crate::testing::worker::Runner::Codeql) => {
                // No session starts here either (#373).
                self.disarm_failure_breakpoint();
                self.status = String::from(
                    "Debugging is not available for CodeQL tests — the play glyph runs them",
                );
            }
            None => {
                self.disarm_failure_breakpoint();
                self.status = String::from("No test runner detected in this workspace");
            }
        }
    }

    /// Kick off `cargo test --no-run` on a background thread and remember the
    /// receiver: a cold build takes minutes, and running it on the UI thread
    /// froze every pane — no keys, no redraw, no PTY pumping — while the
    /// "Building…" status could never even paint before the launch overwrote
    /// it. The drain launches the debugger when the binary arrives.
    pub(in crate::app) fn start_test_binary_build(
        &mut self,
        root: PathBuf,
        name: String,
        source: Option<PathBuf>,
    ) {
        if self.pending_test_debug.is_some() {
            // The launch this armed for is not happening (#373).
            self.disarm_failure_breakpoint();
            self.status = String::from("A test binary is already building");
            return;
        }
        self.status = self.with_failure_note(format!("Building test binary for {name}"));
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let name = name.clone();
            let root = root.clone();
            std::thread::spawn(move || {
                let _ = tx.send(
                    crate::testing::worker::build_test_binary(&root, &name, source.as_deref())
                        .map_err(|e| e.to_string()),
                );
            });
        }
        self.pending_test_debug = Some((rx, name, root));
    }

    /// Drain the background test-binary build: on success launch lldb-dap
    /// (with the editor's breakpoints as they are NOW — the user may have set
    /// more while the build ran), on failure surface the error. Returns true
    /// when anything landed, for the redraw aggregate.
    pub(in crate::app) fn drain_pending_test_debug(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let Some((rx, _, _)) = self.pending_test_debug.as_ref() else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(String::from("the build thread died")),
        };
        let (_, name, build_root) = self.pending_test_debug.take().expect("checked above");
        // Re-rooting the Explorer mid-build would otherwise launch the old
        // workspace's binary with the new workspace's cwd and breakpoints.
        if build_root != self.tree.root {
            // No launch, so nothing will clean up after it (#373).
            self.disarm_failure_breakpoint();
            self.status = format!(
                "Test binary for {name} finished, but the workspace changed — debug it again from its workspace"
            );
            return true;
        }
        match result {
            Ok(binary) => self.launch_lldb_test_debug(&binary, &name),
            Err(e) => self.debug_error(format!("Test build failed: {e}")),
        }
        true
    }

    /// The launch tail of debug-a-cargo-test, run from the drain once the
    /// background build hands over the binary.
    pub(in crate::app) fn launch_lldb_test_debug(&mut self, binary: &Path, name: &str) {
        use std::collections::BTreeMap;
        if !self.debug_sessions.is_empty() {
            // The build took minutes and the user started another session
            // meanwhile: this launch is abandoned, so the breakpoint it
            // armed has nothing to clean up after it (#373).
            self.disarm_failure_breakpoint();
            self.status = String::from("A debug session is already running (Shift+F5 stops it)");
            return;
        }
        let Some(adapter) = lldb_dap_path() else {
            self.debug_error(lldb_dap_missing_message());
            return;
        };
        let root = self.tree.root.clone();
        let breakpoints: BTreeMap<PathBuf, Vec<crate::dap::session::SourceBreakpoint>> = self
            .editor
            .breakpoints
            .iter()
            .map(|(p, lines)| (p.clone(), self.editor.source_breakpoints(p, lines)))
            .collect();
        // The bare name is libtest's substring filter (run-at-cursor has no
        // full module path); --nocapture keeps output visible.
        let args = vec![name.to_string(), String::from("--nocapture")];
        let request = crate::dap::session::lldb_test_launch_request(binary, &args);
        match crate::dap::session::DapSession::launch_with(
            &adapter,
            &[],
            &root,
            request,
            breakpoints,
        ) {
            Ok(session) => {
                self.adopt_debug_session(name, session, LaunchSlot::Replace);
                self.run_debug.feedback = Some(format!("Debugging test {name} (lldb)"));
                self.run_debug.feedback_is_error = false;
                self.status = self.with_failure_note(format!(
                    "Debugging test {name} via lldb-dap — F5 continue · Shift+F5 stop"
                ));
                self.reveal_debug_view();
            }
            Err(e) => self.debug_error(format!("Failed to start lldb-dap: {e}")),
        }
    }
}

impl App {
    /// Where the last run of `name` failed, if the runner named a place in
    /// the user's own code (#373). Read back from the Test Runner output
    /// channel, which already holds every line the run printed.
    ///
    /// Scoped to the failing test's own block: a run of the whole suite
    /// prints many failures, and breaking on another test's assertion would
    /// stop the debugger somewhere the user did not ask about. The block
    /// starts at the runner's "failures:"-style banner for this test and
    /// ends at the next test's banner.
    pub(in crate::app) fn failure_site_of(
        &self,
        name: &str,
    ) -> Option<crate::testing::failure_site::FailureSite> {
        use crate::testing::failure_site::is_failure_banner;
        let lines = crate::output::snapshot(crate::output::CHANNEL_TESTS)?;
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        // Walk backwards to the most recent BANNER for this test, so a
        // re-run's output wins over an earlier one in the same channel.
        // Anchoring on the banner rather than on any mention is what makes
        // this work against real output: libtest reprints every failing
        // name in its trailing `failures:` summary, so "the last line
        // mentioning the name" lands after every block and finds nothing.
        let start = texts.iter().rposition(|t| is_failure_banner(t, name))?;
        // The block runs to the next test's banner, or to the end.
        let end = texts[start + 1..]
            .iter()
            .position(|t| {
                (t.trim_start().starts_with("---- ")
                    || t.trim_start().starts_with('_')
                    || t.trim_start().starts_with('\u{25cf}'))
                    && !is_failure_banner(t, name)
            })
            .map(|i| start + 1 + i)
            .unwrap_or(texts.len());
        crate::testing::failure_site::failure_site(&texts[start..end].join("\n"))
    }

    /// Set a TEMPORARY breakpoint at the assertion the last run of `name`
    /// failed on (#373), unless the user already has one there.
    ///
    /// Recorded in `debug_temp_breakpoint` so [`Self::disarm_failure_breakpoint`]
    /// can take exactly this one back out: a breakpoint croft set and then
    /// left behind would be indistinguishable from one the user set, and
    /// would keep stopping them on a line they never chose.
    pub(in crate::app) fn arm_failure_breakpoint(&mut self, name: &str, root: &Path) {
        self.disarm_failure_breakpoint();
        let Some((file, line)) = self
            .failure_site_of(name)
            .and_then(|site| site.resolve(root))
        else {
            return;
        };
        let line = line as usize;
        let lines = self.editor.breakpoints.entry(file.clone()).or_default();
        // The user's own breakpoint there is left alone: it is theirs, it
        // outlives the session, and removing it on stop would delete work.
        if !lines.insert(line) {
            return;
        }
        self.debug_temp_breakpoint = Some((file.clone(), line));
        // Recorded for the caller to append to whatever message it shows:
        // setting `status` here would be overwritten moments later by the
        // launch's own ("Building test binary…"), so the user would never
        // learn a breakpoint had been set for them.
        self.debug_temp_note = Some(format!(
            "breaking at {}:{line} where it last failed",
            file.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    }

    /// Append the armed-breakpoint note to `msg`, if one was armed (#373).
    pub(in crate::app) fn with_failure_note(&self, msg: String) -> String {
        match &self.debug_temp_note {
            Some(note) => format!("{msg} — {note}"),
            None => msg,
        }
    }

    /// Remove the breakpoint [`Self::arm_failure_breakpoint`] set, if it is
    /// still ours. The user may have removed it themselves mid-session, in
    /// which case there is nothing to take back.
    pub(in crate::app) fn disarm_failure_breakpoint(&mut self) {
        self.debug_temp_note = None;
        let Some((file, line)) = self.debug_temp_breakpoint.take() else {
            return;
        };
        if let Some(lines) = self.editor.breakpoints.get_mut(&file) {
            lines.remove(&line);
            if lines.is_empty() {
                self.editor.breakpoints.remove(&file);
            }
        }
    }

    /// Debug one Go test under delve's `test` mode (#264), scoped to it
    /// with `-test.run`. `name` is a tree id (`./pkg::TestName`), or a bare
    /// function name from the caret, whose package is the open file's.
    pub(in crate::app) fn start_delve_test_session(
        &mut self,
        root: &Path,
        name: &str,
        breakpoints: std::collections::BTreeMap<
            PathBuf,
            Vec<crate::dap::session::SourceBreakpoint>,
        >,
    ) {
        let dir = match name.split_once("::") {
            Some((pkg, _)) => crate::testing::gotest::package_dir(root, pkg),
            None => self
                .editor
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
        };
        let Some(dir) = dir else {
            self.disarm_failure_breakpoint();
            self.status = format!("Debug Test: no package directory for {name}");
            return;
        };
        let dlv = match crate::dap::install::dlv_program() {
            Ok(d) => d,
            Err(e) => {
                self.disarm_failure_breakpoint();
                self.debug_error(format!("{e}"));
                return;
            }
        };
        let request = crate::dap::session::delve_test_request(
            &dir,
            &crate::testing::gotest::test_binary_args(name),
        );
        match crate::dap::session::DapSession::launch_delve(&dlv, &dir, request, breakpoints) {
            Ok(session) => {
                self.adopt_debug_session(name.to_string(), session, LaunchSlot::Replace);
                self.run_debug.feedback = Some(format!("Debugging test {name}"));
                self.run_debug.feedback_is_error = false;
                self.status = self.with_failure_note(format!(
                    "Debugging test {name} — F5 continue · F10 step over · Shift+F5 stop"
                ));
                self.reveal_debug_view();
            }
            Err(e) => self.debug_error(format!("Failed to start debugger: {e}")),
        }
    }
}
