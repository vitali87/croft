//! The active file: Run (how to run a file, in a new terminal) and F5 with no launch
//! configuration, which picks the adapter by the file's kind: debugpy, delve, js-debug or
//! lldb-dap.

use super::*;

impl App {
    /// Resolve how to run `file`: which command line to feed the PTY and
    /// which directory the PTY should `chdir` to. Returns `None` for
    /// extensions we don't know how to run.
    ///
    /// For Python files the resolver walks from the file's directory up to
    /// (and including) `workspace_root`, picking the first `.venv/bin/python`,
    /// `venv/bin/python`, or `.env/bin/python` it finds. The cwd is set to
    /// the directory that owns the venv so relative imports, `.env` reads,
    /// and any path-relative work the script does line up with how the user
    /// would run it from their own shell. With no venv anywhere on the
    /// path the resolver falls back to the system `python3` and the file's
    /// own directory.
    pub(in crate::app) fn run_spec_for(file: &Path, workspace_root: &Path) -> Option<RunSpec> {
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let file_str = file.to_string_lossy().into_owned();
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        if matches!(ext.as_str(), "py" | "pyi" | "pyw") {
            if let Some((py, venv_dir)) = find_python_venv(&file_dir, workspace_root) {
                return Some(RunSpec {
                    program: py.to_string_lossy().into_owned(),
                    args: vec![file_str],
                    cwd: venv_dir,
                });
            }
            return Some(RunSpec {
                program: String::from("python3"),
                args: vec![file_str],
                cwd: file_dir,
            });
        }
        let runner: &str = match ext.as_str() {
            "js" | "mjs" | "cjs" => "node",
            "ts" | "tsx" | "mts" | "cts" => "tsx",
            "rb" => "ruby",
            "lua" => "lua",
            "php" => "php",
            "pl" => "perl",
            "sh" | "bash" => "bash",
            "zsh" => "zsh",
            "fish" => "fish",
            _ => {
                // No known script runner. If the file is itself an executable
                // (a compiled binary, usually extensionless), run it directly.
                if is_executable_file(file) {
                    return Some(RunSpec {
                        program: file_str,
                        args: vec![],
                        cwd: file_dir,
                    });
                }
                return None;
            }
        };
        Some(RunSpec {
            program: String::from(runner),
            args: vec![file_str],
            cwd: file_dir,
        })
    }
}

impl App {
    /// Launch a debug session for the active file, choosing the adapter by file
    /// type. Python (debugpy) is the verified path; other recognised compiled
    /// languages route to lldb-dap.
    pub(in crate::app) fn start_debug_session(&mut self) {
        use crate::dap::session::AdapterKind;
        // Same as every other launch path: a parked older launch must not
        // fire later and replace this one (#567), nor one waiting for its
        // debugger to be set up (#2803).
        self.supersede_waiting_launches();
        self.debug_build = None;
        // Zero-config "Debug active file" declares no postDebugTask.
        self.debug_post_tasks.clear();
        let Some(path) = self.editor.path.clone() else {
            self.debug_error(String::from("Open a file to debug"));
            return;
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match crate::dap::registry::adapter_for_extension(&ext) {
            Some(AdapterKind::Debugpy) => self.start_python_debug_session(&path),
            Some(AdapterKind::LldbDap) => self.start_lldb_debug_session(&path),
            Some(AdapterKind::JsDebug) => self.start_js_debug_session(&path),
            Some(AdapterKind::Delve) => self.start_delve_debug_session(&path),
            None => {
                // An executable binary (e.g. a compiled `target/debug/app`) can
                // be debugged directly by lldb-dap with no build step.
                if is_executable_file(&path) {
                    self.launch_lldb(&path, &path);
                } else if let Some(ext_id) = crate::dap::registry::disabled_extension_for(&ext) {
                    // A debugger exists for this file type but its extension is
                    // turned off — say so rather than claim none exists.
                    self.debug_error(format!(
                        "The {ext_id} debugger extension is disabled — enable it in Extensions"
                    ));
                } else {
                    self.debug_error(format!(
                        "No debugger for .{ext} files (Python, JS/TS, Rust, C/C++ are supported)"
                    ));
                }
            }
        }
    }

    /// Debug the Python file at `path` under debugpy: the adapter runs from
    /// croft's debug venv, built on a job the first time (#2803), and the
    /// program under the interpreter Run would use.
    pub(in crate::app) fn start_python_debug_session(&mut self, path: &Path) {
        let debug_root = self
            .roots
            .owning_root(path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        // The debug venv hosts only the adapter; the program runs under
        // the interpreter Run would use, so its packages import (#864).
        // One debugpy cannot run under is refused before any setup.
        let program_py = debuggee_python(
            path.parent().unwrap_or(&debug_root),
            &debug_root,
            std::env::var_os("PATH").as_deref(),
        );
        let note = match program_py.as_deref().map_or(Ok(None), debuggee_python_note) {
            Ok(note) => note,
            Err(refusal) => {
                self.debug_error(refusal);
                return;
            }
        };
        let Some(py) = self.debug_python_or_park(|| ParkedLaunch::PythonFile(path.to_path_buf()))
        else {
            return;
        };
        let program_py = program_py.unwrap_or_else(|| py.clone());
        let breakpoints = self.collect_editor_breakpoints();
        let adapter_args = vec![String::from("-m"), String::from("debugpy.adapter")];
        let py_str = py.to_string_lossy().into_owned();
        match crate::dap::session::DapSession::launch(
            &py_str,
            &adapter_args,
            &debug_root,
            path,
            &program_py,
            breakpoints,
            false,
        ) {
            Ok(session) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.adopt_debug_session(name.clone(), session, LaunchSlot::Replace);
                self.run_debug.feedback = Some(format!("Debugging {name}"));
                self.run_debug.feedback_is_error = false;
                let lead = with_python_note(format!("Debugging {name}"), note.as_deref());
                self.status = format!("{lead} — F5 continue · F10 step over · Shift+F5 stop");
                self.reveal_debug_view();
                // After the reveal, which clears the console; it outlasts
                // the status line.
                if note.is_some() {
                    self.debug_console_push(lead);
                }
            }
            Err(e) => {
                self.debug_error(format!("Failed to start debugger: {e}"));
            }
        }
    }

    /// Debug the Go file at `path` under delve (#264): its package's `main`,
    /// or its tests for a `_test.go` file. delve builds the package itself.
    pub(in crate::app) fn start_delve_debug_session(&mut self, path: &Path) {
        let dlv = match crate::dap::install::dlv_program() {
            Ok(d) => d,
            Err(e) => {
                self.debug_error(format!("{e}"));
                return;
            }
        };
        let breakpoints = self.collect_editor_breakpoints();
        let cwd = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.roots.primary().to_path_buf());
        let request = crate::dap::session::delve_zero_config_request(path);
        match crate::dap::session::DapSession::launch_delve(&dlv, &cwd, request, breakpoints) {
            Ok(session) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.adopt_debug_session(name.clone(), session, LaunchSlot::Replace);
                self.run_debug.feedback = Some(format!("Debugging {name}"));
                self.run_debug.feedback_is_error = false;
                self.status =
                    format!("Debugging {name} — F5 continue · F10 step over · Shift+F5 stop");
                self.reveal_debug_view();
            }
            Err(e) => {
                self.debug_error(format!("Failed to start debugger: {e}"));
            }
        }
    }

    /// Launch a vscode-js-debug session for a `.js`/`.ts` file run under Node.
    /// Provisions node and the js-debug server on a job on first use (#2803),
    /// then drives the shared DAP
    /// session machinery (which transparently spawns the parent + child
    /// connections js-debug requires). TypeScript binds via source maps.
    pub(in crate::app) fn start_js_debug_session(&mut self, path: &Path) {
        let Some((node, server)) =
            self.debug_js_or_park(|| ParkedLaunch::JsFile(path.to_path_buf()))
        else {
            return;
        };
        let breakpoints = self.collect_editor_breakpoints();
        let debug_root = self
            .roots
            .owning_root(path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        match crate::dap::session::DapSession::launch_js(
            &node,
            &server,
            &debug_root,
            path,
            breakpoints,
            false,
        ) {
            Ok(session) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.adopt_debug_session(name.clone(), session, LaunchSlot::Replace);
                self.run_debug.feedback = Some(format!("Debugging {name}"));
                self.run_debug.feedback_is_error = false;
                self.status =
                    format!("Debugging {name} — F5 continue · F10 step over · Shift+F5 stop");
                self.reveal_debug_view();
            }
            Err(e) => {
                self.debug_error(format!("Failed to start debugger: {e}"));
            }
        }
    }

    /// Launch lldb-dap on a Rust / C / C++ source file: build a debuggable
    /// binary, then drive the same DAP session machinery as Python. The binary
    /// build is the only language-specific step; breakpoints, stepping, call
    /// stack, variables, and the REPL all flow through the shared session.
    ///
    /// Note: lldb-dap requires a permitted `debugserver` (macOS gates this
    /// behind code-signing / `DevToolsSecurity`); where that isn't granted the
    /// process runs without binding breakpoints. croft surfaces the adapter's
    /// own status rather than pretending otherwise.
    pub(in crate::app) fn start_lldb_debug_session(&mut self, path: &Path) {
        if lldb_dap_path().is_none() {
            self.debug_error(lldb_dap_missing_message());
            return;
        }
        let is_rust = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"));
        if !is_rust {
            self.begin_debug_build(path);
            return;
        }
        // Compiled source: build a debuggable binary first, then debug it.
        let binary = match self.build_debuggable_binary(path) {
            Ok(b) => b,
            Err(e) => {
                self.debug_error(format!("Build for debug failed: {e}"));
                return;
            }
        };
        self.launch_lldb(&binary, path);
    }

    /// Spawn a fresh PTY at the resolved working directory, seed it with the
    /// runner command for the active editor tab's file, and focus the
    /// terminal pane so the user sees output immediately.
    pub fn run_active_file(&mut self) {
        if self.editor.path.is_none() {
            self.run_debug.feedback = Some(String::from("Open a file first"));
            self.run_debug.feedback_is_error = true;
            return;
        }
        self.launch_with_unsaved_saved(Self::run_active_file_now);
    }

    pub(in crate::app) fn run_active_file_now(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let run_root = self
            .roots
            .owning_root(&path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        let Some(spec) = Self::run_spec_for(&path, &run_root) else {
            self.run_debug.feedback = Some(format!(
                "Don't know how to run {}",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            ));
            self.run_debug.feedback_is_error = true;
            return;
        };
        match PtyTerminal::new_running(&spec.program, &spec.args, &spec.cwd) {
            Ok(term) => {
                let target = self.active_terminal + 1;
                self.terminals.insert(target, term);
                self.active_terminal = target;
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.focus_pane(Pane::Terminal);
                self.run_debug.feedback = Some(format!(
                    "Running {}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                ));
                self.run_debug.feedback_is_error = false;
            }
            Err(e) => {
                self.run_debug.feedback = Some(format!("Failed to spawn terminal: {e}"));
                self.run_debug.feedback_is_error = true;
            }
        }
    }
}
