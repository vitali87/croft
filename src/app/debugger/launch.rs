//! Starting a launch: saving unsaved editors first, F5 (start or continue), the selected
//! configuration, resolving a launch.json configuration and its program, spawning its
//! adapter (a headless browser when there is no display), and installing the new session.

use super::*;

/// An lldb launch's `program`, resolved against the session's cwd (VS Code's
/// contract for relative programs, and NOT croft's own working directory) and
/// checked for existence, so the validation and the adapter can never read a
/// relative path differently. Returns the absolute path to hand the adapter.
pub(in crate::app) fn resolve_lldb_program(program: &str, cwd: &Path) -> Result<PathBuf, String> {
    let resolved = crate::dap::configs::absolute_in(program, cwd);
    if resolved.exists() {
        Ok(resolved)
    } else {
        Err(format!(
            "program {} does not exist — does the preLaunchTask build it?",
            resolved.display()
        ))
    }
}

impl App {
    /// Run `launch` with every unsaved editor written first (#1400), as VS
    /// Code's `debug.saveBeforeStart` does: the debuggee reads its files
    /// from disk, while breakpoints and the stop arrow follow the buffer,
    /// so launching over unsaved edits pauses on the wrong lines.
    ///
    /// The tabs are written as typed, through the auto-save sweep: a
    /// format-on-save reply lands later and could move lines the launch
    /// already sent breakpoints for. The sweep never overwrites a file that
    /// changed on disk (or writes a lossy encoding, a hex or sheet edit, an
    /// unresolved merge); whatever is still unsaved after it, or everything
    /// with `disable_save_before_debug`, is named in front of the launch's
    /// own status.
    pub(in crate::app) fn launch_with_unsaved_saved(&mut self, launch: impl FnOnce(&mut Self)) {
        self.run_with_unsaved_saved("the launch", launch);
    }

    /// [`Self::launch_with_unsaved_saved`] for anything that runs the
    /// project's files: a task, a Testing run (#1639). VS Code saves before
    /// a task by default (`task.saveBeforeRun`); without it a build or test
    /// run reported on code the editor had already changed. `what` names
    /// the run in the warning.
    pub(in crate::app) fn run_with_unsaved_saved(
        &mut self,
        what: &str,
        launch: impl FnOnce(&mut Self),
    ) {
        if self.save_before_debug && self.unsaved_count() > 0 {
            self.sweep_dirty_buffers(false, false, false);
        }
        let unsaved = self.unsaved_editor_names();
        launch(self);
        if unsaved.is_empty() {
            return;
        }
        let (names, verb) = match unsaved.as_slice() {
            [one] => (one.clone(), "has"),
            [first, second] => (format!("{first} and {second}"), "have"),
            [first, rest @ ..] => (format!("{first} and {} more", rest.len()), "have"),
            [] => unreachable!("checked above"),
        };
        let warning = format!("{names} {verb} unsaved changes: {what} uses the file on disk");
        let running = std::mem::take(&mut self.status);
        self.status = if running.is_empty() {
            warning
        } else {
            format!("{warning} - {running}")
        };
        // The run registered its "Running …" status for its outcome to
        // replace; that status now carries the warning, so the tracked text
        // follows it, or the outcome would never land.
        let tracked = self
            .test_run_status
            .iter_mut()
            .chain(self.pane_run_statuses.iter_mut().map(|(_, run)| run));
        for run in tracked {
            if run.shown == running {
                run.shown = self.status.clone();
            }
        }
    }

    /// File names of the editors with unsaved edits, deduped, the active
    /// tab first: what [`Self::launch_with_unsaved_saved`] warns about. A
    /// scratch tab (a Search Editor, the git output) is no file a run reads,
    /// so it is never named (#2074 review).
    pub(in crate::app) fn unsaved_editor_names(&self) -> Vec<String> {
        let mut seen: Vec<&std::path::Path> = Vec::new();
        let mut names = Vec::new();
        let groups = std::iter::once(&self.editor).chain(self.editor_layout.inactive_groups());
        let active = self.editor.editors.get(self.editor.active_index());
        let tabs = active
            .into_iter()
            .chain(groups.flat_map(|g| g.editors.iter()));
        for ed in tabs.filter(|e| e.dirty && !e.scratch) {
            let Some(path) = ed.path.as_deref() else {
                continue;
            };
            if seen.contains(&path) {
                continue;
            }
            seen.push(path);
            names.push(
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            );
        }
        names
    }

    /// F5: start debugging the active file, or resume if already paused.
    pub fn debug_start_or_continue(&mut self) {
        use crate::dap::session::SessionPhase;
        if let Some(session) = self.debug_sessions.focused_mut() {
            if session.phase == SessionPhase::Stopped {
                session.continue_execution();
                self.editor.stop_line = None;
                self.clear_inline_values();
                self.announce_event(String::from("Continued"));
            }
            return;
        }
        self.start_selected_debug();
    }

    /// Start whatever F5 is pointed at: the selected launch.json
    /// configuration when one is chosen, else the zero-config active file.
    /// Configs are re-read here so an edited launch.json applies on the next
    /// F5 without reopening the picker.
    pub(in crate::app) fn start_selected_debug(&mut self) {
        self.launch_with_unsaved_saved(Self::start_selected_debug_now);
    }

    pub(in crate::app) fn start_selected_debug_now(&mut self) {
        if let Some(name) = self.selected_debug_compound.clone() {
            let root = self.active_workspace_root();
            self.debug_compounds = crate::dap::configs::discover_compounds(&root);
            if let Some(compound) = self
                .debug_compounds
                .iter()
                .find(|c| c.name == name)
                .cloned()
            {
                self.launch_compound(&compound);
                return;
            }
            self.selected_debug_compound = None;
            self.run_debug.selected_config = None;
            self.status =
                format!("Debug compound \"{name}\" no longer exists — debugging the active file");
            self.start_debug_session();
            return;
        }
        if let Some(name) = self.selected_debug_config.clone() {
            let root = self.active_workspace_root();
            self.debug_configs = crate::dap::configs::discover_configs(&root);
            if let Some(cfg) = self.debug_configs.iter().find(|c| c.name == name).cloned() {
                self.launch_debug_config(&cfg);
                return;
            }
            // The selection outlived its config (file edited/deleted): fall
            // back to zero-config and say so rather than failing silently.
            self.selected_debug_config = None;
            self.run_debug.selected_config = None;
            self.status = format!(
                "Debug configuration \"{name}\" no longer exists — debugging the active file"
            );
        }
        self.start_debug_session();
    }
}

impl App {
    /// Resolve and launch one launch.json configuration: substitution against
    /// the active editor state, then either straight to the adapter or parked
    /// behind its `preLaunchTask`.
    pub(in crate::app) fn launch_debug_config(&mut self, cfg: &crate::dap::configs::DebugConfig) {
        use crate::dap::configs;
        // Asked before anything running is stopped, so Esc on a question
        // leaves the current session alone (#1424).
        let mut refs = cfg.input_refs();
        let mut defs = cfg.inputs.clone();
        if let Some(label) = cfg.pre_launch_task() {
            self.task_inputs(label, &mut refs, &mut defs);
        }
        let then = InputThen::Launch(Box::new(cfg.clone()));
        match self.ask_inputs(&refs, &defs, &cfg.answers, then) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                self.debug_error(format!("config \"{}\": {e}", cfg.name));
                return;
            }
        }
        if !self.debug_sessions.is_empty() {
            self.debug_stop();
        }
        self.pending_debug_launch = None;
        self.debug_build = None;
        self.pending_cargo_launches.clear();
        // A new run: the previous run's cleanup is not this one's.
        self.debug_post_tasks.clear();
        // A compound member from another workspace folder resolves, and
        // finds its preLaunchTask, there (#1425).
        let root = cfg
            .root
            .clone()
            .unwrap_or_else(|| self.active_workspace_root());
        let ctx = configs::SubstCtx {
            workspace_folder: root.clone(),
            file: self.editor.path.clone(),
            inputs: cfg.answers.clone(),
            folders: self.workspace_folders(),
        };
        let mut rc = match configs::resolve(cfg, &ctx) {
            Ok(rc) => rc,
            Err(e) => {
                self.debug_error(e);
                return;
            }
        };
        // Pinned to the root `program`/`outFiles` were resolved against
        // (#1414): a `preLaunchTask` can run while the user focuses a file in
        // another root, and recomputing the root at launch would point
        // js-debug's source-map defaults at the wrong folder.
        if rc.kind == crate::dap::session::AdapterKind::JsDebug {
            rc.extra
                .entry("__workspaceFolder")
                .or_insert_with(|| serde_json::json!(root.to_string_lossy()));
        }
        self.record_post_debug_task(&rc);
        if let Some(task_label) = rc.pre_launch_task.clone() {
            let tasks = crate::tasks::discover_tasks(&root);
            let task = match crate::tasks::find_task(tasks, &task_label) {
                Ok(task) => task,
                Err(near) => {
                    self.debug_error(format!(
                        "preLaunchTask \"{task_label}\" not found{} — Tasks: Run Task lists what the workspace declares",
                        did_you_mean(near)
                    ));
                    return;
                }
            };
            let task = with_answers(task, &cfg.answers);
            let command = self
                .task_command_line_in(&task, root.clone())
                .unwrap_or_else(|_| task.command.clone());
            self.park_launch_behind(
                task,
                root,
                command,
                PendingLaunchStep::Resolved(Box::new(rc)),
                "preLaunchTask",
                "debug starts",
            );
            return;
        }
        self.launch_resolved_config(rc);
    }

    /// Launch a resolved config, replacing whatever was running.
    pub(in crate::app) fn launch_resolved_config(
        &mut self,
        rc: crate::dap::configs::ResolvedConfig,
    ) {
        self.launch_resolved_in(rc, LaunchSlot::Replace);
    }

    /// Launch a resolved config BESIDE what is running, for a compound member.
    pub(in crate::app) fn launch_resolved_into_set(
        &mut self,
        rc: crate::dap::configs::ResolvedConfig,
    ) {
        self.launch_resolved_in(rc, LaunchSlot::Add);
    }

    /// Spawn the adapter for a resolved launch.json configuration and hand it
    /// the built launch/attach request.
    pub(in crate::app) fn launch_resolved_in(
        &mut self,
        mut rc: crate::dap::configs::ResolvedConfig,
        slot: LaunchSlot,
    ) {
        use crate::dap::configs::{self, RequestKind};
        use crate::dap::session::AdapterKind;
        // The picker attaches by pid, which is lldb's attach; debugpy and
        // js-debug attach by other means, and dropping the key silently would
        // launch something the config did not ask for.
        if rc.pick_process && rc.kind != AdapterKind::LldbDap {
            self.debug_error(format!(
                "config \"{}\": ${{command:pickProcess}} is supported for lldb attach only",
                rc.name
            ));
            return;
        }
        // `${command:pickProcess}`: ask first, then come back here with the
        // pid filled in (#250). Asked before the adapter lookup so the choice
        // does not depend on it; a missing lldb-dap is reported on resume.
        if rc.pick_process && rc.process_id.is_none() {
            self.open_attach_process_picker(rc, slot);
            return;
        }
        // A CodeLLDB `cargo` launch names no program: cargo builds it first
        // (#1392), off the UI thread, and the launch resumes here with it.
        if rc.kind == AdapterKind::LldbDap
            && rc.request == RequestKind::Launch
            && rc.program.is_none()
            && let Some(spec) = configs::cargo_spec(&rc)
        {
            match spec {
                Ok(spec) => self.start_cargo_program_build(rc, slot, spec),
                Err(e) => self.debug_error(format!("config \"{}\": {e}", rc.name)),
            }
            return;
        }
        let breakpoints = self.collect_editor_breakpoints();
        let cwd = rc
            .cwd
            .clone()
            .unwrap_or_else(|| self.active_workspace_root());
        let name = rc.name.clone();
        // A note for the status: the debuggee interpreter's (#864) for a
        // Python launch, or a browser run headless (#1410).
        let mut launch_note = None;
        let result = match rc.kind {
            AdapterKind::Debugpy => {
                if rc.request == RequestKind::Launch && rc.program.is_none() && rc.module.is_none()
                {
                    self.debug_error(format!(
                        "config \"{name}\": a python launch needs a \"program\" or \"module\""
                    ));
                    return;
                }
                // Remote Attach (#1417): the port a `debugpy --listen`
                // process serves is its own adapter, so the session talks to
                // it directly; a second, local adapter in front of it waits
                // for a debug server that never comes.
                let attach = (rc.request == RequestKind::Attach)
                    .then(|| configs::debugpy_request(&rc, Path::new("python3")));
                let direct = attach
                    .and_then(|r| configs::debugpy_connect_address(&r).map(|address| (r, address)));
                if let Some((request, (host, port))) = direct {
                    crate::dap::session::DapSession::attach_tcp(&host, port, request, breakpoints)
                        .map_err(|e| {
                            e.context(format!(
                                "nothing answered on {host}:{port}; start the program with `python -m debugpy --listen {host}:{port}` first"
                            ))
                        })
                } else {
                    match crate::dap::install::ensure_debug_venv() {
                        Ok(py) => {
                            let adapter_args =
                                vec![String::from("-m"), String::from("debugpy.adapter")];
                            let program_py = config_project_python(
                                &rc,
                                &cwd,
                                &self.active_workspace_root(),
                                &py,
                            );
                            let request = configs::debugpy_request(&rc, &program_py);
                            // Whichever interpreter runs the program, the
                            // config's own included (#864).
                            if rc.request == RequestKind::Launch {
                                match launch_request_python(&request, &cwd)
                                    .as_deref()
                                    .map_or(Ok(None), debuggee_python_note)
                                {
                                    Ok(note) => launch_note = note,
                                    Err(refusal) => {
                                        self.debug_error(format!("config \"{name}\": {refusal}"));
                                        return;
                                    }
                                }
                            }
                            crate::dap::session::DapSession::launch_with(
                                &py.to_string_lossy(),
                                &adapter_args,
                                &cwd,
                                request,
                                breakpoints,
                            )
                        }
                        Err(e) => {
                            self.debug_error(format!("Debugger setup failed: {e}"));
                            return;
                        }
                    }
                }
            }
            AdapterKind::LldbDap => {
                let Some(adapter) = lldb_dap_path() else {
                    self.debug_error(lldb_dap_missing_message());
                    return;
                };
                match rc.request {
                    RequestKind::Launch => {
                        let Some(program) = rc.program.as_deref() else {
                            self.debug_error(format!(
                                "config \"{name}\": an lldb launch needs a \"program\" (the built binary)"
                            ));
                            return;
                        };
                        match resolve_lldb_program(program, &cwd) {
                            Ok(resolved) => {
                                rc.program = Some(resolved.to_string_lossy().into_owned())
                            }
                            Err(e) => {
                                self.debug_error(format!("config \"{name}\": {e}"));
                                return;
                            }
                        }
                    }
                    // By pid, by program name or through attachCommands
                    // (gdb-remote), sent as written (#1419).
                    RequestKind::Attach => {
                        if !configs::lldb_attach_has_target(&rc) {
                            self.debug_error(format!(
                                "config \"{name}\": an lldb attach needs a numeric \"processId\", a \"program\" or \"attachCommands\""
                            ));
                            return;
                        }
                    }
                }
                crate::dap::session::DapSession::launch_with(
                    &adapter,
                    &[],
                    &cwd,
                    configs::lldb_request(&rc),
                    breakpoints,
                )
            }
            AdapterKind::Delve => {
                if rc.request == RequestKind::Launch && rc.program.is_none() {
                    self.debug_error(format!(
                        "config \"{name}\": a go launch needs a \"program\" (a package directory or file)"
                    ));
                    return;
                }
                // "Connect to server" (#1418): a headless delve already
                // serves DAP at host:port, so croft connects to it rather
                // than starting a dlv of its own.
                let remote = configs::delve_remote_address(&rc);
                if rc.request == RequestKind::Attach && rc.process_id.is_none() && remote.is_none()
                {
                    let remote_mode =
                        rc.extra.get("mode").and_then(serde_json::Value::as_str) == Some("remote");
                    self.debug_error(if remote_mode {
                        format!("config \"{name}\": a go remote attach needs a \"port\"")
                    } else {
                        format!(
                            "config \"{name}\": a go attach needs a \"processId\", or \"mode\": \"remote\" with a \"port\""
                        )
                    });
                    return;
                }
                if let Some((host, port)) = remote {
                    crate::dap::session::DapSession::attach_delve_server(
                        &host,
                        port,
                        configs::delve_request(&rc),
                        breakpoints,
                    )
                    .with_context(|| {
                        format!(
                            "nothing answered on {host}:{port}; start it first, e.g. `dlv debug <package> --headless --listen=:{port} --api-version=2 --accept-multiclient`"
                        )
                    })
                } else {
                    match crate::dap::install::dlv_program() {
                        Ok(dlv) => crate::dap::session::DapSession::launch_delve(
                            &dlv,
                            &cwd,
                            configs::delve_request(&rc),
                            breakpoints,
                        ),
                        Err(e) => {
                            self.debug_error(format!("{e}"));
                            return;
                        }
                    }
                }
            }
            AdapterKind::JsDebug => {
                if let Some(e) = configs::js_config_error(&rc) {
                    self.debug_error(format!("config \"{name}\": {e}"));
                    return;
                }
                let browser = configs::browser_type(&rc.type_name).is_some();
                let node = match crate::dap::install::node_program() {
                    Ok(n) => n,
                    Err(e) => {
                        self.debug_error(format!("{e}"));
                        return;
                    }
                };
                let server = match crate::dap::install::ensure_js_debug() {
                    Ok(s) => s,
                    Err(e) => {
                        self.debug_error(format!("Debugger setup failed: {e}"));
                        return;
                    }
                };
                let mut request = configs::js_request(&rc);
                // A browser croft has no screen for would open where no one
                // sees it (#1410): it runs headless, and the status says so.
                if browser && rc.request == RequestKind::Launch && no_display_for_a_browser() {
                    request = configs::with_headless_browser(request);
                    launch_note = Some(String::from(
                        "(no display here, so the browser runs headless)",
                    ));
                }
                crate::dap::session::DapSession::launch_js_with(
                    &node,
                    &server,
                    &cwd,
                    configs::with_workspace_folder(request, &self.active_workspace_root()),
                    breakpoints,
                )
            }
        };
        match result {
            Ok(session) => {
                self.adopt_debug_session(&name, session, slot);
                let verb = match rc.request {
                    RequestKind::Launch => "Debugging",
                    RequestKind::Attach => "Attached:",
                };
                self.run_debug.feedback = Some(format!("{verb} {name}"));
                self.run_debug.feedback_is_error = false;
                let lead = with_python_note(format!("{verb} {name}"), launch_note.as_deref());
                self.status = format!("{lead} — F5 continue · F10 step over · Shift+F5 stop");
                self.reveal_debug_view();
                if launch_note.is_some() {
                    self.debug_console_push(lead);
                }
                // A cppdbg configuration runs under lldb-dap (#1394): say
                // which of its gdb settings have no effect there. Here, not
                // at the call site, so a preLaunchTask resume and a compound
                // member say it too.
                if !rc.ignored_keys.is_empty() {
                    self.debug_console_push(format!(
                        "config \"{name}\" runs under lldb-dap, which ignores its {}",
                        rc.ignored_keys.join(", ")
                    ));
                }
            }
            Err(e) => self.debug_error(format!("Failed to start debugger: {e}")),
        }
    }

    /// Install a freshly launched debug session, replacing the running ones
    /// or, for a compound member, beside them. Every launch path comes
    /// through here so the stored function breakpoints go with each new
    /// session (held until `initialized`, then pushed with the rest): set one
    /// and press F5 with no launch.json, and the program still pauses (#939).
    pub(in crate::app) fn adopt_debug_session(
        &mut self,
        name: impl Into<String>,
        mut session: crate::dap::session::DapSession,
        slot: LaunchSlot,
    ) {
        session.set_function_breakpoints(self.function_breakpoints.clone());
        match slot {
            LaunchSlot::Replace => self.debug_sessions.replace_with(name, session),
            LaunchSlot::Add => self.debug_sessions.push(name, session),
        }
    }
}

/// Whether a browser croft launches would have no screen to open on
/// (#1410): croft is reached over SSH, or on Linux no X or Wayland display
/// is set. macOS and Windows always have one locally.
pub(in crate::app) fn no_display_for_a_browser() -> bool {
    no_display(
        crate::remote::running_over_ssh(),
        cfg!(target_os = "linux"),
        |k| std::env::var_os(k).is_some_and(|v| !v.is_empty()),
    )
}

/// Whether there is no display, given whether croft runs over SSH, whether
/// it is on Linux, and which environment variables are `set` (#1410).
pub(crate) fn no_display(over_ssh: bool, linux: bool, set: impl Fn(&str) -> bool) -> bool {
    over_ssh || (linux && !set("DISPLAY") && !set("WAYLAND_DISPLAY"))
}
