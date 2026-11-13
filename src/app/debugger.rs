//! Run and Debug: launch configs and Add Configuration, compounds and preLaunchTask waits,
//! adapters, debug builds, attach pickers, the paused view, watches, the console,
//! stepping, every kind of breakpoint, Debug Test and inline values.

use super::*;

/// Rank an executable basename as an lldb-dap candidate, or `None` if it is not
/// one. LLVM ships the adapter under several names: the bare `lldb-dap`, the
/// versioned `lldb-dap-18` that Debian/Ubuntu install (often with no
/// unversioned symlink), and the legacy `lldb-vscode[-NN]` name from older
/// releases. Higher rank wins: an unversioned binary (tracks the system
/// default) beats any versioned one, a newer LLVM release beats an older one,
/// and at equal version the modern `lldb-dap` name beats legacy `lldb-vscode`.
pub(super) fn lldb_dap_rank(name: &str) -> Option<i64> {
    if name == "lldb-dap" {
        return Some(i64::MAX);
    }
    if name == "lldb-vscode" {
        return Some(i64::MAX - 1);
    }
    // `family` breaks ties at equal version: lldb-dap (1) over lldb-vscode (0).
    let (version, family) = name
        .strip_prefix("lldb-dap-")
        .map(|v| (v, 1))
        .or_else(|| name.strip_prefix("lldb-vscode-").map(|v| (v, 0)))?;
    let version: i64 = version.parse().ok()?;
    Some(version * 2 + family)
}

/// Resolve the `lldb-dap` adapter binary: the Xcode Command Line Tools copy
/// first (always present on a dev Mac), then the best-ranked match found by
/// scanning `PATH` for `lldb-dap`/`lldb-dap-NN`/`lldb-vscode[-NN]`. `None` if
/// none is installed.
pub(super) fn lldb_dap_path() -> Option<String> {
    let xcode = "/Library/Developer/CommandLineTools/usr/bin/lldb-dap";
    if std::path::Path::new(xcode).is_file() {
        return Some(xcode.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    let mut best: Option<(i64, String)> = None;
    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(rank) = lldb_dap_rank(&name) else {
                continue;
            };
            let full = entry.path();
            if is_executable_file(&full) && best.as_ref().is_none_or(|(r, _)| rank > *r) {
                best = Some((rank, full.to_string_lossy().into_owned()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The error shown when no lldb-dap adapter is installed, with a host-correct
/// install hint: Xcode Command Line Tools on macOS, the LLVM package elsewhere.
pub(super) fn lldb_dap_missing_message() -> String {
    let hint = if cfg!(target_os = "macos") {
        "install Xcode Command Line Tools"
    } else {
        "install LLVM, e.g. apt install lldb"
    };
    format!("lldb-dap not found ({hint})")
}

/// The interpreter that runs a Python project's own code: its `.venv` python
/// (where the project's deps, pytest included, are importable) or the system
/// python3. The debug venv's python is only the debugpy ADAPTER host — the
/// debuggee must be this one, or a pytest module launch can't import pytest.
pub(super) fn project_python(root: &Path) -> PathBuf {
    project_python_on(root, std::env::var_os("PATH").as_deref())
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// [`project_python`] with `path_var` as `PATH`, None where it would fall
/// back to a bare `python3`: no `.venv` and no `python3` on `path_var`.
pub(super) fn project_python_on(
    root: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let venv = root.join(".venv").join("bin").join("python");
    if venv.is_file() {
        return Some(venv);
    }
    std::env::split_paths(path_var?)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
}

/// Whether `path` is a regular file with the executable bit set — i.e. a
/// compiled binary or script croft can run directly (`./file`) and debug via
/// lldb-dap without a build step. The honest signal for "directly executable",
/// since such files usually have no extension.
pub(super) fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Append variable rows to the debug tree, recursing into any expanded
/// (`variablesReference` in `expanded`) container whose children are loaded.
/// Pulled out as a free function so it can borrow the session immutably while
/// the caller mutates the row vector.
pub(super) fn push_variable_rows(
    rows: &mut Vec<crate::widgets::run_debug::DebugRow>,
    session: &crate::dap::session::DapSession,
    expanded: &std::collections::HashSet<i64>,
    vars: &[crate::dap::session::Variable],
    indent: u8,
) {
    use crate::widgets::run_debug::{DebugRow, DebugRowKind};
    for v in vars {
        let expandable = v.variables_ref > 0;
        let is_open = expandable && expanded.contains(&v.variables_ref);
        rows.push(DebugRow {
            indent,
            kind: DebugRowKind::Variable {
                reference: v.variables_ref,
                expandable,
                expanded: is_open,
                name: v.name.clone(),
                value: v.value.clone(),
                type_name: v.type_name.clone(),
                evaluate_name: v.evaluate_name.clone(),
            },
        });
        if is_open && let Some(children) = session.variables.get(&v.variables_ref) {
            push_variable_rows(rows, session, expanded, children, indent.saturating_add(1));
        }
    }
}

/// Grace before the post-teardown js-debug orphan sweep runs, letting a
/// just-killed server's detached watchdog reparent to init so the sweep can
/// recognise it as an orphan. See [`crate::dap::reaper`].
pub(super) const REAPER_TEARDOWN_GRACE_MS: u64 = 250;

/// The end-of-session feedback line. When breakpoints were set but the program
/// exited without ever stopping, say so explicitly — that is the common "I set a
/// breakpoint and nothing happened" confusion (e.g. running a library module
/// whose breakpointed code is never called). Otherwise a plain end message.
/// What a parked launch does once its task exits 0.
///
/// Two shapes, because a compound's task and a config's task are different
/// steps (#318). A config's own task is the LAST thing before the adapter
/// starts, so the config is already resolved. A compound's task runs BEFORE
/// its member is considered at all, and the member may carry a task of its
/// own - VS Code runs both - so that continuation re-enters the normal launch
/// path rather than skipping to the adapter.
pub(super) enum PendingLaunchStep {
    /// The config's own task just finished: start it. Boxed like the other
    /// variant so neither dominates the enum's size - a resolved config is
    /// several hundred bytes, and this type is held in an `Option` on `App`.
    Resolved(Box<crate::dap::configs::ResolvedConfig>),
    /// A compound's task just finished: launch its member from the top, which
    /// parks again if the member has a `preLaunchTask` too.
    Member(Box<crate::dap::configs::DebugConfig>),
    /// A MULTI-member compound's task just finished: launch every member
    /// (#310). Separate from `Member` because the set has to be started
    /// together - running them through the single-member path would stop each
    /// previous one.
    /// Boxed like its siblings: the doc above says they are boxed so none
    /// dominates the enum's size, and a list plus a name inline would have
    /// dominated it six times over.
    Members(Box<CompoundLaunch>),
}

/// What became of one compound member (#310).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum MemberOutcome {
    /// Started, and now in the running set.
    Launched,
    /// Its `cargo` build is running (#1392); it joins the set when the
    /// build lands.
    Building,
    /// Not started: it declares a `preLaunchTask` of its own, and croft's
    /// parking is one-at-a-time, so the task cannot be run as part of a set.
    /// Launching regardless would debug whatever the last build left behind.
    TaskUnrun,
    /// Tried and failed; the reason was already reported against its name.
    Failed,
}

/// Whether a launch replaces the running set or joins it (#310).
///
/// Starting a configuration has always terminated whatever was running, and
/// that stays true; a compound MEMBER is the one caller that must not, or the
/// second member would kill the first.
#[derive(Clone, Copy)]
pub(super) enum LaunchSlot {
    Replace,
    Add,
}

/// Where the session at `index` sits once every index in `removed` has been
/// taken out of the set: `None` if it was removed itself, otherwise shifted
/// down by one for each removal below it (#567).
pub(super) fn index_after_removals(index: usize, removed: &[usize]) -> Option<usize> {
    (!removed.contains(&index)).then(|| index - removed.iter().filter(|&&r| r < index).count())
}

/// A session is ending: mirror to OUTPUT's "Debug Console" channel each line
/// its console streams leave open (#932), as `console` shows it, since no
/// newline will come to finish it. `dropped` is how many lines have left the
/// front of `console`, which the streams' positions count from.
pub(super) fn mirror_open_console_lines(
    streams: &mut [crate::dap::session::ConsoleStream],
    console: &[crate::ansi_text::AnsiLine],
    dropped: usize,
) {
    for stream in streams {
        let Some(line) = stream.ansi.finish() else {
            continue;
        };
        let shown = stream
            .shown_at
            .take()
            .and_then(|at| at.checked_sub(dropped))
            .and_then(|i| console.get(i));
        crate::output::push(
            crate::output::CHANNEL_DEBUG_CONSOLE,
            crate::output::OutputLevel::Info,
            &shown.unwrap_or(&line).text,
        );
    }
}

/// The BREAKPOINTS section of the debug tree (#250): a header and one row per
/// breakpoint, or nothing when there are none.
pub(super) fn breakpoint_section_rows(count: usize) -> Vec<crate::widgets::run_debug::DebugRow> {
    use crate::widgets::run_debug::{DebugRow, DebugRowKind};
    if count == 0 {
        return Vec::new();
    }
    let mut rows = vec![DebugRow {
        indent: 0,
        kind: DebugRowKind::Header {
            title: String::from("BREAKPOINTS"),
        },
    }];
    rows.extend((0..count).map(|index| DebugRow {
        indent: 1,
        kind: DebugRowKind::Breakpoint { index },
    }));
    rows
}

/// The current user's processes, croft's own excluded, for the attach
/// picker (#250). Empty when `ps` is unavailable.
pub(super) fn list_user_processes() -> Vec<(i64, String)> {
    let out = std::process::Command::new("ps")
        .args(["-x", "-o", "pid=,comm="])
        .output();
    match out {
        Ok(o) if o.status.success() => crate::dap::configs::parse_ps_processes(
            &String::from_utf8_lossy(&o.stdout),
            i64::from(std::process::id()),
        ),
        _ => Vec::new(),
    }
}

/// What an [`InputRun`] does once every question is answered (#1424).
pub(super) enum InputThen {
    Launch(Box<crate::dap::configs::DebugConfig>),
    Compound(Box<crate::dap::configs::Compound>),
    Task(Box<crate::tasks::Task>),
}

/// How a launch waits on its `preLaunchTask` (#1541), as VS Code does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreLaunchWait {
    /// An ordinary task: until it exits, and only exit 0 launches.
    Exit,
    /// A background task with a watch matcher: until its first build
    /// cycle ends, and only a cycle with no errors launches.
    FirstBuild,
    /// A background task with no matcher to say when a build ends: it is
    /// never going to exit, so the launch goes ahead at once.
    Untracked,
}

impl PreLaunchWait {
    pub(super) fn of(task: &crate::tasks::Task) -> Self {
        if !task.is_background {
            return Self::Exit;
        }
        let watches = task
            .problem_matcher
            .as_ref()
            .and_then(crate::problem_matchers::from_tasks_json)
            .is_some_and(|m| m.background.is_some());
        if watches {
            Self::FirstBuild
        } else {
            Self::Untracked
        }
    }
}

/// Whether a finished pane command settles the pending preLaunchTask: same
/// pane, and the captured command line is the task's (the shell may report
/// it with surrounding whitespace). Pure, for tests.
pub(super) fn finished_settles_pending(
    pane: u64,
    cmd: &str,
    pending_pane: u64,
    pending_cmd: &str,
) -> bool {
    pane == pending_pane && cmd.trim() == pending_cmd.trim()
}

/// How long a parked launch waits before concluding the pane will never
/// report a command mark. Only consulted while the pane's foreground is back
/// to a plain shell — a long build keeps a non-shell foreground and waits
/// indefinitely, exactly like the FinishedCommand path.
pub(super) const PRELAUNCH_MARK_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

/// An lldb launch's `program`, resolved against the session's cwd (VS Code's
/// contract for relative programs, and NOT croft's own working directory) and
/// checked for existence, so the validation and the adapter can never read a
/// relative path differently. Returns the absolute path to hand the adapter.
pub(super) fn resolve_lldb_program(program: &str, cwd: &Path) -> Result<PathBuf, String> {
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

/// The FinishedCommand match is the settling signal for a parked launch, but
/// it depends on OSC 133 shell-integration marks. This is the fallback for
/// panes that will never produce one: the pane vanished, or its foreground
/// returned to a plain shell (the task is over) and the grace period passed
/// without a mark. Returns the abort reason, or `None` to keep waiting. Pure.
pub(super) fn pending_abort_reason(
    pane_exists: bool,
    foreground_is_shell: bool,
    waited: std::time::Duration,
) -> Option<&'static str> {
    if !pane_exists {
        return Some("its terminal pane closed");
    }
    if foreground_is_shell && waited > PRELAUNCH_MARK_GRACE {
        return Some("the task produced no command mark (shell integration missing in that pane?)");
    }
    None
}

pub(super) fn debug_end_message(had_breakpoints: bool, ever_stopped: bool) -> &'static str {
    if had_breakpoints && !ever_stopped {
        "Program exited without hitting a breakpoint"
    } else {
        "Debug session ended"
    }
}

impl App {
    pub(super) fn handle_run_debug_key(&mut self, key: KeyEvent) {
        // While a session is live the panel is a REPL: text edits the prompt and
        // Enter evaluates it. Esc always leaves the view.
        if self.run_debug.debug_active {
            match key.code {
                KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
                KeyCode::Enter => {
                    self.debug_repl_submit();
                    self.refresh_debug_panel();
                }
                KeyCode::Backspace => {
                    self.run_debug.repl_input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.run_debug.repl_input.push(c);
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.set_sidebar_view(SidebarView::Explorer),
            KeyCode::Enter => self.run_debug_button_action(),
            _ => {}
        }
    }

    /// Debug: Install Go Debugger (delve) (#264). Runs `go install` in a
    /// terminal pane, where the user sees the command and its output: that
    /// visible run is the consent, as for a coverage tool. Without Go there
    /// is nothing to run it with, so it says that instead.
    pub(super) fn install_delve(&mut self) {
        if !crate::dap::install::go_on_path() {
            self.status = crate::dap::install::dlv_missing_message(false);
            return;
        }
        match crate::widgets::terminal::PtyTerminal::new(self.roots.primary()) {
            Ok(mut term) => {
                term.set_manual_name(Some(String::from("delve install")));
                term.write_input(format!("{}\r", crate::dap::install::DLV_INSTALL).as_bytes());
                self.insert_terminal(term);
                self.show_terminal = true;
                self.focus_pane(Pane::Terminal);
                self.status = String::from(
                    "Installing delve into ~/.croft/servers/go; start debugging again once it finishes",
                );
            }
            Err(e) => self.status = format!("Could not open a terminal: {e}"),
        }
    }

    /// Debug the test the editor caret sits in (Cmd+K Shift+Enter, palette).
    pub(super) fn debug_test_at_cursor(&mut self) {
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
    pub(super) fn test_runner_for_focused_file(
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
    pub(super) fn debug_named_test(&mut self, name: String) {
        if !self.debug_sessions.is_empty() {
            self.status = String::from("A debug session is already running (Shift+F5 stops it)");
            return;
        }
        self.launch_with_unsaved_saved(|app| app.debug_named_test_now(name));
    }

    pub(super) fn debug_named_test_now(&mut self, name: String) {
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
    pub(super) fn start_test_binary_build(
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

    /// Run a launch's `cargo` build on a background thread (#1392); the
    /// drain launches the config once cargo names the executable.
    pub(super) fn start_cargo_program_build(
        &mut self,
        rc: crate::dap::configs::ResolvedConfig,
        slot: LaunchSlot,
        spec: crate::dap::configs::CargoSpec,
    ) {
        let cwd = rc
            .cwd
            .clone()
            .unwrap_or_else(|| self.active_workspace_root());
        let line = format!("cargo {}", spec.args.join(" "));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::dap::configs::build_cargo_program(&spec, &cwd));
        });
        self.run_debug.feedback = Some(format!("Building with {line}\u{2026}"));
        self.run_debug.feedback_is_error = false;
        self.status = format!(
            "config \"{}\": building with {line} \u{2014} debug starts when it succeeds",
            rc.name
        );
        let root = self.tree.root.clone();
        self.pending_cargo_launches
            .push((rx, Box::new(rc), slot, root));
    }

    /// Launch every `cargo` config whose build finished, or report why it
    /// did not. True when anything landed.
    pub(super) fn drain_pending_cargo_launch(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let mut landed = false;
        let mut i = 0;
        while i < self.pending_cargo_launches.len() {
            let result = match self.pending_cargo_launches[i].0.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(String::from("the build thread died")),
            };
            let (_, mut rc, slot, build_root) = self.pending_cargo_launches.remove(i);
            landed = true;
            // Re-rooting mid-build would otherwise launch the old
            // workspace's binary in the new one.
            if build_root != self.tree.root {
                self.status = format!(
                    "config \"{}\": cargo finished, but the workspace changed — launch it again from its workspace",
                    rc.name
                );
                continue;
            }
            match result {
                Ok(program) => {
                    rc.program = Some(program.to_string_lossy().into_owned());
                    self.launch_resolved_in(*rc, slot);
                }
                Err(e) => self.debug_error(format!("config \"{}\": {e}", rc.name)),
            }
        }
        landed
    }

    /// Drain the background test-binary build: on success launch lldb-dap
    /// (with the editor's breakpoints as they are NOW — the user may have set
    /// more while the build ran), on failure surface the error. Returns true
    /// when anything landed, for the redraw aggregate.
    pub(super) fn drain_pending_test_debug(&mut self) -> bool {
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
    pub(super) fn launch_lldb_test_debug(&mut self, binary: &Path, name: &str) {
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

    pub(super) fn refresh_run_debug(&mut self) {
        let path = self.editor.path.clone();
        // Decide the button: scripts and executables are "Run" (terminal);
        // compiled source (no script runner, but a debug adapter) is "Debug"
        // (build + lldb). Files that are neither (e.g. `.git/config`) get no
        // button. run_spec_for already covers executables (the +x bit), so a
        // compiled binary is "Run" — run it directly.
        let (runnable, is_debug) = match path.as_deref() {
            Some(p) => {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if Self::run_spec_for(
                    p,
                    self.roots
                        .owning_root(p)
                        .unwrap_or_else(|| self.roots.primary()),
                )
                .is_some()
                {
                    (true, false)
                } else if crate::dap::registry::adapter_for_extension(&ext).is_some() {
                    (true, true)
                } else {
                    (false, false)
                }
            }
            None => (false, false),
        };
        self.run_debug.runnable = runnable;
        self.run_debug.run_is_debug = is_debug;
        self.run_debug.set_active_file(path);
        self.run_debug.feedback = None;
        self.run_debug.feedback_is_error = false;
        // The launch.json config row (#250): count + F5 selection. Discovery
        // re-reads the same two small files once per helper — four reads — and
        // this only runs on active-file changes.
        // Compounds count too — the row is the documented route to the picker,
        // and the picker lists them, so counting only configurations makes the
        // row vanish for a compounds-only workspace and undercount otherwise.
        //
        // A HIDDEN compound is not listed (#318), so it must not be counted
        // either: the row and the picker have to agree, or a workspace whose
        // only entry is hidden advertises "1 debug entry" over a picker that
        // offers nothing. Same filter as `open_debug_config_picker`.
        let root = self.active_workspace_root();
        // The same filter the picker applies, so the badge and the picker
        // cannot disagree: a `presentation.hidden` entry of EITHER kind is
        // absent from both (#318).
        self.run_debug.config_count = crate::dap::configs::discover_configs(&root)
            .iter()
            .filter(|c| !c.hidden())
            .count()
            + crate::dap::configs::discover_compounds(&root)
                .iter()
                .filter(|c| !c.hidden())
                .count();
        self.run_debug.selected_config = self
            .selected_debug_compound
            .clone()
            .or_else(|| self.selected_debug_config.clone());
    }

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
    pub(super) fn run_spec_for(file: &Path, workspace_root: &Path) -> Option<RunSpec> {
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

    /// Spawn a fresh PTY at the resolved working directory, seed it with the
    /// runner command for the active editor tab's file, and focus the
    /// terminal pane so the user sees output immediately.
    /// Drain the debug session each frame. Returns true if anything changed
    /// (so the frame loop redraws). Mirrors the paused location into the
    /// editor's stop-line, surfaces program output + stop reasons, and tears
    /// the session down when the debuggee exits.
    pub fn poll_dap(&mut self) -> bool {
        use crate::dap::session::{DapEvent, SessionPhase};
        if self.debug_sessions.is_empty() {
            return false;
        }
        let mut changed_background = false;
        // EVERY session is polled, not just the focused one (#310). `poll` is
        // the protocol state machine, not a passive read: it is where
        // `initialized` triggers the breakpoint push and `configurationDone`,
        // and where js-debug's reverse requests are answered. An unpolled
        // member never leaves the gate - its debuggee never runs, its
        // breakpoints never bind, and a js-debug parent blocks forever on a
        // reverse request nobody answered. It would be started, named in the
        // status, and dead.
        //
        // Only the FOCUSED session's events drive the UI. A background
        // member's output still reaches the console, tagged with its name,
        // and everything else it reports is queued on it and replayed when it
        // is focused (#567): dropping its `stopped` left a breakpoint hit that
        // nothing showed, so the member looked hung.
        const BACKLOG_CAP: usize = 256;
        let focused = self.debug_sessions.focused_index();
        // A session focused since the last poll first replays what it did
        // while in the background.
        let mut events = self.debug_sessions.take_focused_backlog();
        let mut background_ended: Vec<usize> = Vec::new();
        // (session index, category, text) of each background output event.
        let mut background_output: Vec<(usize, String, String)> = Vec::new();
        // By index, not name: a compound can list one configuration twice,
        // so two members can share a name.
        let mut background_stopped: Option<usize> = None;
        for (index, named) in self.debug_sessions.iter_named_mut_indexed() {
            let drained = named.session.poll();
            if index == focused {
                events.extend(drained);
                continue;
            }
            changed_background |= !drained.is_empty();
            for ev in drained {
                match ev {
                    // `exited` and `terminated` both end a member and often
                    // arrive together: name it once, or the removal below
                    // takes a live sibling for the second copy.
                    DapEvent::Terminated => {
                        if background_ended.last() != Some(&index) {
                            background_ended.push(index);
                        }
                    }
                    DapEvent::Output { category, text }
                        if crate::dap::session::output_is_user_visible(&category) =>
                    {
                        background_output.push((index, category, text));
                    }
                    DapEvent::Output { .. } => {}
                    other => {
                        if matches!(other, DapEvent::Stopped { .. }) {
                            background_stopped.get_or_insert(index);
                        }
                        named.backlog.push(other);
                        if named.backlog.len() > BACKLOG_CAP {
                            let overflow = named.backlog.len() - BACKLOG_CAP;
                            named.backlog.drain(..overflow);
                        }
                    }
                }
            }
        }
        for (index, category, text) in background_output {
            self.debug_console_push_output(index, &category, &text, true);
        }
        // Highest index first, so removing one does not move the next.
        let mut ended_names = Vec::new();
        // Where the stopped member sits once the ended ones are gone: gone
        // itself if it also ended, else shifted down past every removal below.
        let background_stopped =
            background_stopped.and_then(|i| index_after_removals(i, &background_ended));
        for index in background_ended.into_iter().rev() {
            if let Some(mut gone) = self.debug_sessions.remove(index) {
                mirror_open_console_lines(
                    &mut gone.console,
                    &self.debug_console,
                    self.debug_console_dropped,
                );
                gone.session.disconnect();
                ended_names.push(gone.name);
            }
        }
        // `stopAll: true` (#567): a background member ending takes the set
        // down with it, the same as the focused one ending does below.
        if self.debug_stop_all && !ended_names.is_empty() && !self.debug_sessions.is_empty() {
            self.debug_stop();
            self.status = format!(
                "{} ended — stopAll stopped the rest of the compound",
                ended_names.join(", ")
            );
            self.run_post_debug_tasks();
            return true;
        }
        if !ended_names.is_empty() && !self.debug_sessions.is_empty() {
            self.report_member_ended(&ended_names.join(", "));
        }
        // A background member stopped (a breakpoint, an exception, a pause).
        // If the member being looked at is still running, show the stopped
        // one, as VS Code does; otherwise say so rather than steal the view
        // from a stop the user is already inspecting.
        if let Some(index) = background_stopped
            && let Some(name) = self.debug_sessions.names().get(index).cloned()
        {
            let focused_is_stopped = self
                .debug_sessions
                .focused()
                .is_some_and(|s| s.phase == SessionPhase::Stopped)
                || events.iter().any(|e| matches!(e, DapEvent::Stopped { .. }));
            // A focused member that ended this tick is torn down below, and
            // the teardown reads the FOCUSED member: moving the view first
            // would carry the ending into its backlog and leave it running.
            let focused_ended = self
                .debug_sessions
                .focused()
                .is_some_and(|s| s.phase == SessionPhase::Terminated)
                || events.iter().any(|e| matches!(e, DapEvent::Terminated));
            if focused_is_stopped || focused_ended {
                self.status =
                    format!("{name} stopped in the background · Cmd+Opt+Shift+G switches to it");
            } else {
                self.refocus_debug_session(index, &mut events);
                self.status = format!("{name} stopped · now showing it");
            }
        }
        let mut changed = !events.is_empty() || changed_background;
        for ev in events {
            match ev {
                // Route program output to the debug console (a line per
                // newline, #932), but drop debugpy's own `telemetry` banner
                // (the `ptvsd` / `debugpy` noise).
                DapEvent::Output { category, text }
                    if crate::dap::session::output_is_user_visible(&category) =>
                {
                    let focused = self.debug_sessions.focused_index();
                    self.debug_console_push_output(focused, &category, &text, false);
                }
                // REPL submissions echo "❯ expr" then the result; watch/hover
                // results are consumed elsewhere.
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                    ..
                } if context == "repl" => {
                    self.debug_console_push(format!("❯ {expression}"));
                    // A console line each (#932): a traceback is many lines,
                    // and as one entry only its first was ever visible.
                    for line in result.lines() {
                        self.debug_console_push(line.to_string());
                    }
                }
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                    success,
                } if context == "watch" => {
                    self.watch_vals.insert(expression, (result, success));
                }
                // Copy Value's whole value (#1546), or, when the adapter
                // could not give it, the value the row showed.
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                    success,
                } if context == "clipboard" => {
                    if self
                        .debug_copy_pending
                        .as_ref()
                        .is_some_and(|(expr, _)| *expr == expression)
                        && let Some((_, fallback)) = self.debug_copy_pending.take()
                    {
                        let text = if success { result } else { fallback };
                        self.status = self.copy_where_pasted(&text).status("value");
                    }
                }
                // Fresh stack + frame selection just landed: this is the
                // moment each watch expression re-evaluates against the new
                // top frame (#112).
                DapEvent::InspectionUpdated => {
                    self.refresh_watches();
                    self.refresh_inline_values();
                    changed = true;
                }
                DapEvent::Stopped { reason, .. } => {
                    self.run_debug.feedback = Some(format!("Paused ({reason})"));
                    self.run_debug.feedback_is_error = false;
                    self.watch_baseline_pending = true;
                    self.debug_pause_reason = Some(reason);
                }
                // What the exception stop threw (#864): the panel's pill and
                // the status line, which has the width the pill lacks.
                // Step Into Target (#1543): one call steps straight in, several
                // are offered by label, and a line with none steps in plainly.
                DapEvent::StepInTargets { targets } => {
                    self.offer_step_in_targets(targets);
                }
                DapEvent::ExceptionInfo { summary } => {
                    self.run_debug.feedback = Some(format!("Paused (exception): {summary}"));
                    self.status = format!("Paused on exception: {summary}");
                }
                DapEvent::BreakpointsUpdated => {
                    // Mirror the adapter's unverified set into the editor so the
                    // gutter can hollow out inert breakpoints, and warn once if
                    // any failed to bind (the "I set a breakpoint and it just
                    // ran" case).
                    let unverified: std::collections::HashMap<
                        PathBuf,
                        std::collections::BTreeSet<usize>,
                    > = self
                        .debug_sessions
                        .focused()
                        .map(|s| s.unverified_breakpoints.clone().into_iter().collect())
                        .unwrap_or_default();
                    let total: usize = unverified.values().map(|s| s.len()).sum();
                    self.editor.unverified_breakpoints = unverified;
                    if total > 0 {
                        self.status = format!(
                            "{total} breakpoint(s) could not bind (no executable code on that line)"
                        );
                    }
                }
                _ => {}
            }
        }
        let phase = self.debug_sessions.focused().map(|s| s.phase);
        match phase {
            Some(SessionPhase::Stopped) => {
                self.debug_ever_stopped = true;
                let loc = self
                    .debug_sessions
                    .focused()
                    .and_then(|s| s.current_location.clone());
                // Said once this stop's stack trace has placed it, so the
                // announcement names the file and line (#1547). The frames
                // are cleared at each stop and the location is not, so an
                // empty stack means the location is still the last stop's,
                // and so does a top frame without a source: that stop is
                // read without a file and line rather than with stale ones.
                let top = self
                    .debug_sessions
                    .focused()
                    .and_then(|s| s.stack_frames.first())
                    .map(|f| (f.name.clone(), f.path.is_some()));
                if let Some((frame, has_source)) = top
                    && let Some(reason) = self.debug_pause_reason.take()
                {
                    let at = if has_source { loc.as_ref() } else { None };
                    let said = self.debug_pause_announcement(&reason, at, &frame);
                    self.announce_event(said);
                    changed = true;
                }
                if let Some(loc) = loc
                    && self.editor.stop_line.as_ref() != Some(&loc)
                {
                    self.editor.stop_line = Some(loc);
                    // A re-stop at the same location can re-arm the arrow
                    // without a fresh InspectionUpdated on this tick; rebuild
                    // the trailers here too so arrow and values never split
                    // (#136 review). Idempotent over already-fetched data.
                    self.refresh_inline_values();
                    changed = true;
                }
            }
            Some(SessionPhase::Terminated) => {
                self.debug_pause_reason = None;
                self.editor.stop_line = None;
                self.editor.unverified_breakpoints.clear();
                // A session that ends on its own must take croft's own
                // breakpoint with it, exactly as Shift+F5 does (#373):
                // this is the path a passing test takes, so it is the
                // common one, not the exceptional one.
                self.disarm_failure_breakpoint();
                self.reset_watch_runtime();
                self.clear_inline_values();
                // Reply to the adapter's own `terminated`/`exited` with the
                // graceful `disconnect` handshake before dropping it. js-debug
                // only retires its detached watchdog on this handshake, never on
                // a SIGKILL of the server, so the natural-exit path used to leak
                // the watchdog by dropping straight to a kill.
                // The session that ENDED, not the whole set (#310). Dropping
                // the set reaches its siblings as a SIGKILL through the
                // transport's drop - exactly the kill this handshake exists to
                // avoid - and would make every compound behave as
                // `stopAll: true`, which is neither VS Code's default nor what
                // the file asked for.
                let ended = self.debug_sessions.focused_index();
                let ended_name = self
                    .debug_sessions
                    .focused_name()
                    .unwrap_or("the session")
                    .to_string();
                // The adapter refused to start the program (#264): its reason
                // is the message, not "Debug session ended".
                let launch_error = self
                    .debug_sessions
                    .focused()
                    .and_then(|s| s.launch_error.clone());
                if let Some(mut gone) = self.debug_sessions.remove(ended) {
                    mirror_open_console_lines(
                        &mut gone.console,
                        &self.debug_console,
                        self.debug_console_dropped,
                    );
                    gone.session.disconnect();
                }
                // `stopAll: true` (#567): the siblings go too, and the rest
                // of this arm reports the ended run as for a lone session.
                let stopped_all = self.debug_stop_all && !self.debug_sessions.is_empty();
                if stopped_all {
                    self.debug_stop();
                    self.status =
                        format!("{ended_name} ended — stopAll stopped the rest of the compound");
                }
                if !self.debug_sessions.is_empty() {
                    // Siblings are still running, so the debugger is not torn
                    // down: the view moves to one of them.
                    self.report_member_ended(&ended_name);
                    return true;
                }
                // The watchdog reparents to init the moment the server dies;
                // sweep it (and any tree a crashed prior croft left behind)
                // after a short grace so it has settled into the orphan state.
                crate::dap::reaper::reap_orphaned_js_debug_async(std::time::Duration::from_millis(
                    REAPER_TEARDOWN_GRACE_MS,
                ));
                let had_breakpoints = !self.editor.breakpoints.is_empty();
                match launch_error {
                    Some(reason) => {
                        let msg = format!("Could not start debugging: {reason}");
                        self.status = msg.clone();
                        self.run_debug.feedback = Some(msg);
                        self.run_debug.feedback_is_error = true;
                    }
                    None => {
                        let msg =
                            debug_end_message(had_breakpoints, self.debug_ever_stopped).to_string();
                        // The status bar still read "Debugging x — F5
                        // continue · F10 step over" after the end (#867).
                        // stopAll's own explanation above is left standing,
                        // and a postDebugTask below may report over this.
                        if !stopped_all {
                            self.status = msg.clone();
                        }
                        self.run_debug.feedback = Some(msg);
                        self.run_debug.feedback_is_error = false;
                    }
                }
                // Keep `debug_console` so its output stays visible after the run;
                // the panel renders it until the next session starts.
                self.run_debug.session_ended = true;
                self.run_post_debug_tasks();
                changed = true;
            }
            _ => {
                if self.editor.stop_line.take().is_some() {
                    // The trailers clear wherever the stop arrow clears
                    // (#136 review): this arm catches every phase change the
                    // four explicit resume sites don't, guarded the same way
                    // so ordinary Running ticks pay nothing.
                    self.clear_inline_values();
                    changed = true;
                }
            }
        }
        // Rebuild the Call Stack / Variables panel from the (now-updated) session.
        self.refresh_debug_panel();
        changed
    }

    /// Rebuild the Run & Debug panel's paused-state tree from the session, or
    /// deactivate it when not paused. Cheap; called after each `poll_dap`.
    pub(super) fn refresh_debug_panel(&mut self) {
        let (active, status, rows) = self.build_debug_view();
        self.run_debug.debug_active = active;
        self.run_debug.debug_paused = self
            .debug_sessions
            .focused()
            .is_some_and(|s| s.phase == crate::dap::session::SessionPhase::Stopped);
        if active {
            self.run_debug.debug_status = status;
            self.run_debug.debug_rows = rows;
            let max_scroll = self.run_debug.debug_rows.len().saturating_sub(1);
            if self.run_debug.debug_scroll > max_scroll {
                self.run_debug.debug_scroll = max_scroll;
            }
            // Show the console tail (most recent output / REPL lines).
            let n = self.debug_console.len();
            self.run_debug.console_tail = self.debug_console[n.saturating_sub(50)..].to_vec();
        } else {
            self.run_debug.debug_rows.clear();
            self.run_debug.debug_scroll = 0;
            self.run_debug.repl_input.clear();
            // Retain the just-ended session's console output until the next run;
            // otherwise the idle Run panel shows nothing.
            if self.run_debug.session_ended && !self.debug_console.is_empty() {
                let n = self.debug_console.len();
                self.run_debug.console_tail = self.debug_console[n.saturating_sub(50)..].to_vec();
            } else {
                self.run_debug.console_tail.clear();
            }
        }
    }

    /// Build the (active, status, rows) tuple for the debug panel from the
    /// session. `&self` so it can borrow the session and expansion set without
    /// conflicting with the `&mut self` assignment in `refresh_debug_panel`.
    pub(super) fn build_debug_view(
        &self,
    ) -> (bool, String, Vec<crate::widgets::run_debug::DebugRow>) {
        use crate::dap::session::SessionPhase;
        use crate::widgets::run_debug::{DebugRow, DebugRowKind};
        let Some(session) = self.debug_sessions.focused() else {
            return (false, String::new(), Vec::new());
        };
        // Active for any live session: while Running the tree is empty but the
        // console still shows output; while Stopped the full tree is built.
        if session.phase == SessionPhase::Terminated {
            return (false, String::new(), Vec::new());
        }
        if session.phase != SessionPhase::Stopped {
            let status = self
                .run_debug
                .feedback
                .clone()
                .unwrap_or_else(|| String::from("Running"));
            return (
                true,
                status,
                breakpoint_section_rows(self.breakpoint_items().len()),
            );
        }
        let mut rows = Vec::new();
        rows.push(DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: String::from("CALL STACK"),
            },
        });
        // With more than one thread (goroutines, under delve), each gets a
        // row and the selected one's frames sit under it, as in VS Code
        // (#264). A single thread keeps the plain frame list.
        let threaded = session.threads.len() > 1;
        let mut frames_at = rows.len();
        if threaded {
            for (id, name) in &session.threads {
                let selected = session.stopped_thread == Some(*id);
                rows.push(DebugRow {
                    indent: 1,
                    kind: DebugRowKind::Thread {
                        id: *id,
                        name: name.clone(),
                        selected,
                    },
                });
                if selected {
                    frames_at = rows.len();
                }
            }
        }
        let mut frame_rows = Vec::new();
        for f in &session.stack_frames {
            let loc = f
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            frame_rows.push(DebugRow {
                indent: if threaded { 2 } else { 1 },
                kind: DebugRowKind::Frame {
                    id: f.id,
                    selected: Some(f.id) == session.selected_frame,
                    name: f.name.clone(),
                    location: format!("{loc}:{}", f.line),
                },
            });
        }
        rows.splice(frames_at..frames_at, frame_rows);
        rows.push(DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: String::from("VARIABLES"),
            },
        });
        for (index, scope) in session.scopes.iter().enumerate() {
            let open = session.scope_open(index);
            rows.push(DebugRow {
                indent: 1,
                kind: DebugRowKind::Scope {
                    name: scope.name.clone(),
                    index,
                    open: scope.expensive.then_some(open),
                },
            });
            if !open {
                continue;
            }
            if let Some(vars) = session.variables.get(&scope.variables_ref) {
                push_variable_rows(&mut rows, session, &self.debug_expanded, vars, 2);
            }
        }
        rows.push(DebugRow {
            indent: 0,
            kind: DebugRowKind::Header {
                title: String::from("WATCH"),
            },
        });
        for (widx, expr) in self.watch_exprs.iter().enumerate() {
            let (value, available, pending) = match self.watch_vals.get(expr) {
                Some((v, ok)) => (v.clone(), *ok, false),
                None => (String::new(), true, true),
            };
            let changed = available
                && !pending
                && self.watch_prev.get(expr).is_some_and(|prev| prev != &value);
            rows.push(DebugRow {
                indent: 1,
                kind: DebugRowKind::Watch {
                    index: widx,
                    expression: expr.clone(),
                    value,
                    available,
                    changed,
                    pending,
                },
            });
        }
        rows.push(DebugRow {
            indent: 1,
            kind: DebugRowKind::WatchAdd,
        });
        // Last, so `sync_breakpoint_list` can swap it without a rebuild.
        rows.extend(breakpoint_section_rows(self.breakpoint_items().len()));
        let status = self
            .run_debug
            .feedback
            .clone()
            .unwrap_or_else(|| String::from("Paused"));
        (true, status, rows)
    }

    /// Re-evaluate every watch expression against the current stop. The
    /// current values become the previous stop's baseline first, so the
    /// changed-value highlight compares stop-to-stop (#112).
    pub(super) fn refresh_watches(&mut self) {
        if self.watch_exprs.is_empty() {
            return;
        }
        if std::mem::take(&mut self.watch_baseline_pending) {
            self.watch_prev = self
                .watch_vals
                .drain()
                .filter(|(_, (_, ok))| *ok)
                .map(|(k, (v, _))| (k, v))
                .collect();
        }
        if let Some(session) = self
            .debug_sessions
            .focused_mut()
            .filter(|s| s.phase == crate::dap::session::SessionPhase::Stopped)
        {
            for expr in &self.watch_exprs {
                session.evaluate(expr, "watch");
            }
        }
    }

    /// Append a watch expression (palette or the panel's add row) and
    /// evaluate it immediately when a session is paused.
    pub(super) fn add_watch_expression(&mut self, expr: String) {
        let expr = expr.trim().to_string();
        if expr.is_empty() {
            self.status = String::from("Enter an expression to watch");
            return;
        }
        if self.watch_exprs.contains(&expr) {
            self.status = format!("Already watching {expr}");
            return;
        }
        self.watch_exprs.push(expr.clone());
        if let Some(session) = self
            .debug_sessions
            .focused_mut()
            .filter(|s| s.phase == crate::dap::session::SessionPhase::Stopped)
        {
            session.evaluate(&expr, "watch");
        }
        self.status = format!("Watching {expr}");
    }

    pub(super) fn remove_watch_expression(&mut self, idx: usize) {
        if idx < self.watch_exprs.len() {
            let expr = self.watch_exprs.remove(idx);
            self.watch_vals.remove(&expr);
            self.watch_prev.remove(&expr);
            self.status = format!("Stopped watching {expr}");
        }
    }

    /// Append a line to the debug console, capping the backlog so a chatty
    /// program can't grow it without bound. The line is mirrored to OUTPUT's
    /// "Debug Console" channel (#867), where it reads at full width and can
    /// be searched and copied; that channel keeps its own cap.
    pub(super) fn debug_console_push(&mut self, line: String) {
        self.debug_console_push_line(crate::ansi_text::AnsiLine::plain(line));
    }

    /// Append an output event of the session at `index` to the debug
    /// console, with its colour sequences turned into spans and every other
    /// escape dropped (#1501). The event is a chunk of that session's
    /// `category` stream, not a line (#932), and is parsed with the stream's
    /// own state: a sequence the event ends in the middle of waits for the
    /// next event, a colour it leaves on carries only into its own lines, and
    /// a line ends at its newline, blank ones included. The line a chunk
    /// leaves open is shown as it reads so far and redrawn in place as later
    /// chunks extend it, so `print: i =` and ` 0` read as one line and a
    /// progress bar's `\r` redraws it rather than piling up copies. `tagged`
    /// puts the session's name in front of each line, for a background member.
    pub(super) fn debug_console_push_output(
        &mut self,
        index: usize,
        category: &str,
        text: &str,
        tagged: bool,
    ) {
        let Some((_, named)) = self
            .debug_sessions
            .iter_named_mut_indexed()
            .find(|(i, _)| *i == index)
        else {
            let mut alone = crate::ansi_text::AnsiStream::default();
            let mut lines = alone.feed(text);
            lines.extend(alone.finish());
            for line in lines {
                self.debug_console_push_line(line);
            }
            return;
        };
        let prefix = tagged.then(|| format!("[{}] ", named.name));
        let stream = named.console_stream(category);
        let ended = stream.ansi.feed(text);
        let open = stream.ansi.open_line().cloned();
        let mut slot = stream
            .shown_at
            .take()
            .and_then(|at| at.checked_sub(self.debug_console_dropped))
            .filter(|&i| i < self.debug_console.len());
        let tag = |mut line: crate::ansi_text::AnsiLine| {
            if let Some(prefix) = &prefix {
                line.prefix(prefix);
            }
            line
        };
        // The first line the chunk ends is the one shown open, if any:
        // finish it where it stands.
        for line in ended.into_iter().map(tag) {
            match slot.take() {
                Some(i) => {
                    crate::output::push(
                        crate::output::CHANNEL_DEBUG_CONSOLE,
                        crate::output::OutputLevel::Info,
                        &line.text,
                    );
                    self.debug_console[i] = line;
                }
                None => self.debug_console_push_line(line),
            }
        }
        let shown_at = open.map(tag).map(|line| match slot {
            Some(i) => {
                self.debug_console[i] = line;
                self.debug_console_dropped + i
            }
            None => {
                self.debug_console_append(line);
                self.debug_console_dropped + self.debug_console.len() - 1
            }
        });
        if let Some((_, named)) = self
            .debug_sessions
            .iter_named_mut_indexed()
            .find(|(i, _)| *i == index)
        {
            named.console_stream(category).shown_at = shown_at;
        }
    }

    /// Append a finished line to the debug console and mirror it to
    /// OUTPUT's "Debug Console" channel.
    pub(super) fn debug_console_push_line(&mut self, line: crate::ansi_text::AnsiLine) {
        crate::output::push(
            crate::output::CHANNEL_DEBUG_CONSOLE,
            crate::output::OutputLevel::Info,
            &line.text,
        );
        self.debug_console_append(line);
    }

    /// Append a line to the debug console without mirroring it, capping the
    /// backlog and counting the lines the cap drops off its front.
    pub(super) fn debug_console_append(&mut self, line: crate::ansi_text::AnsiLine) {
        const CAP: usize = 1000;
        self.debug_console.push(line);
        if self.debug_console.len() > CAP {
            let overflow = self.debug_console.len() - CAP;
            self.debug_console.drain(..overflow);
            self.debug_console_dropped += overflow;
        }
    }

    /// Submit the Run & Debug panel's REPL input: echo + evaluate it at the
    /// selected frame. No-op on empty input or with no session.
    pub(super) fn debug_repl_submit(&mut self) {
        let expr = self.run_debug.repl_input.trim().to_string();
        self.run_debug.repl_input.clear();
        if expr.is_empty() {
            return;
        }
        match self.debug_sessions.focused_mut() {
            Some(session) => session.evaluate(&expr, "repl"),
            None => self.debug_console_push(String::from("(no debug session)")),
        }
    }

    /// Hover-to-evaluate: while a debug session is paused, return a popup string
    /// for the identifier at `(line, c)` if it's a loaded local/global, else
    /// None (so the normal LSP hover proceeds).
    pub(super) fn debug_hover_value(&self, line: usize, c: usize) -> Option<String> {
        use crate::dap::session::SessionPhase;
        let session = self.debug_sessions.focused()?;
        if session.phase != SessionPhase::Stopped {
            return None;
        }
        let word = self.editor.word_string_at(line, c)?;
        let var = session.lookup_local(&word)?;
        let ty = if var.type_name.is_empty() {
            String::new()
        } else {
            format!(": {}", var.type_name)
        };
        Some(format!("{} = {}{ty}", var.name, var.value))
    }

    /// Act on a click of debug-panel row `idx`: select a call-stack frame (load
    /// its scopes/variables) or toggle an expandable variable open/closed.
    pub(super) fn debug_panel_click(&mut self, idx: usize) {
        use crate::widgets::run_debug::DebugRowKind;
        let Some(row) = self.run_debug.debug_rows.get(idx) else {
            return;
        };
        match row.kind.clone() {
            DebugRowKind::Thread { id, .. } => {
                if let Some(session) = self.debug_sessions.focused_mut() {
                    session.select_thread(id);
                }
                self.debug_expanded.clear();
            }
            DebugRowKind::Frame { id, .. } => {
                if let Some(session) = self.debug_sessions.focused_mut() {
                    session.load_frame(id);
                }
                // Collapse expansions: variable refs are frame-scoped.
                self.debug_expanded.clear();
            }
            DebugRowKind::Variable {
                reference,
                expandable: true,
                ..
            } => {
                if self.debug_expanded.contains(&reference) {
                    self.debug_expanded.remove(&reference);
                } else {
                    self.debug_expanded.insert(reference);
                    if let Some(session) = self.debug_sessions.focused_mut() {
                        session.expand_variable(reference);
                    }
                }
            }
            DebugRowKind::WatchAdd => {
                self.open_add_watch_prompt();
            }
            // An expensive scope opens (and is fetched) on click (#1421).
            DebugRowKind::Scope {
                index,
                open: Some(_),
                ..
            } => {
                if let Some(session) = self.debug_sessions.focused_mut() {
                    session.toggle_expensive_scope(index);
                }
            }
            _ => {}
        }
        self.refresh_debug_panel();
    }

    /// The right-click menu of debug-panel row `idx` (#1546), VS Code's:
    /// a variable's Copy Value, Copy as Expression and Add to Watch, a
    /// watch's Copy Value, Copy Expression and Remove, and a frame's Copy
    /// Call Stack. Empty for a row that has none (a header, a scope).
    pub(super) fn debug_row_menu(&self, idx: usize) -> Vec<(String, MenuAction)> {
        use crate::widgets::run_debug::DebugRowKind;
        let Some(row) = self.run_debug.debug_rows.get(idx) else {
            return Vec::new();
        };
        match &row.kind {
            DebugRowKind::Variable {
                value,
                evaluate_name,
                ..
            } => {
                let mut items = vec![(
                    String::from("Copy Value"),
                    MenuAction::CopyDebugValue {
                        expression: evaluate_name.clone(),
                        value: value.clone(),
                    },
                )];
                if let Some(expr) = evaluate_name {
                    items.push((
                        String::from("Copy as Expression"),
                        MenuAction::CopyDebugExpression(expr.clone()),
                    ));
                    items.push((
                        String::from("Add to Watch"),
                        MenuAction::AddDebugWatch(expr.clone()),
                    ));
                }
                items
            }
            DebugRowKind::Watch {
                index,
                expression,
                value,
                ..
            } => vec![
                (
                    String::from("Copy Value"),
                    MenuAction::CopyDebugValue {
                        expression: Some(expression.clone()),
                        value: value.clone(),
                    },
                ),
                (
                    String::from("Copy Expression"),
                    MenuAction::CopyDebugExpression(expression.clone()),
                ),
                (
                    String::from("Remove Expression"),
                    MenuAction::RemoveDebugWatch(*index),
                ),
            ],
            DebugRowKind::Frame { .. } => {
                vec![(String::from("Copy Call Stack"), MenuAction::CopyCallStack)]
            }
            _ => Vec::new(),
        }
    }

    /// Copy Value (#1546). The row's `value` can be cut short by the
    /// adapter (debugpy elides a long string), so while paused, with an
    /// expression and an adapter that offers it, the value is asked for
    /// again in the `clipboard` context, which is not cut, and copied when
    /// it lands (`poll_dap`). Otherwise the row's value is copied at once.
    pub(super) fn copy_debug_value(&mut self, expression: Option<String>, value: String) {
        use crate::dap::session::SessionPhase;
        if let Some(expr) = expression
            && let Some(session) = self
                .debug_sessions
                .focused_mut()
                .filter(|s| s.phase == SessionPhase::Stopped)
            && session.supports("supportsClipboardContext")
        {
            session.evaluate(&expr, "clipboard");
            self.debug_copy_pending = Some((expr, value));
            self.status = String::from("Copying value…");
            return;
        }
        self.status = self.copy_where_pasted(&value).status("value");
    }

    /// Copy Call Stack (#1546): every frame of the paused thread, innermost
    /// first, as `name (path:line)`, VS Code's form.
    pub(super) fn copy_call_stack(&mut self) {
        let Some(session) = self.debug_sessions.focused() else {
            return;
        };
        let text = session
            .stack_frames
            .iter()
            .map(|f| match &f.path {
                Some(p) => format!("{} ({}:{})", f.name, p.display(), f.line),
                None => f.name.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.status = self.copy_where_pasted(&text).status("call stack");
    }

    /// The "+ Add Expression" affordance (panel row or palette): a single-line
    /// input popup, per croft's inputs-in-popups rule.
    pub(super) fn open_add_watch_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::AddWatch,
            "Add Watch Expression",
            "expression",
        ));
    }

    /// The Run/Debug panel button action: debug compiled source (build + lldb),
    /// or run scripts/executables in a terminal — matching the button's verb.
    pub(super) fn run_debug_button_action(&mut self) {
        if self.run_debug.run_is_debug {
            self.debug_start_or_continue();
        } else {
            self.run_active_file();
        }
    }

    /// Open and focus the Run and Debug view so the call stack, variables,
    /// console, and REPL are visible the instant a session starts (VS Code
    /// reveals its debug view on launch the same way).
    pub(super) fn reveal_debug_view(&mut self) {
        // A fresh session: drop the previous run's retained console + stop
        // tracking so the "exited without hitting a breakpoint" hint and the
        // console reflect only this run.
        self.debug_console_dropped += self.debug_console.len();
        self.debug_console.clear();
        self.debug_ever_stopped = false;
        self.run_debug.session_ended = false;
        self.show_tree = true;
        self.set_sidebar_view(SidebarView::RunDebug);
        self.focus_pane(Pane::Tree);
    }

    /// Surface a debug-start failure on BOTH the styled Run/Debug panel
    /// feedback line AND the always-visible status bar. The panel line only
    /// renders while the Run/Debug sidebar is the active view (`reveal_debug_view`
    /// is reached only on success), so without the status-bar mirror F5 fails
    /// silently whenever the user is looking at any other sidebar — which read
    /// as "F5 does nothing" on a remote with no debug adapter installed.
    pub(super) fn debug_error(&mut self, msg: String) {
        // A launch that never happened leaves no session to clean up after,
        // so any breakpoint croft armed for it comes out here (#373) —
        // otherwise it survives in the editor as if the user had set it.
        self.disarm_failure_breakpoint();
        self.run_debug.feedback = Some(msg.clone());
        self.run_debug.feedback_is_error = true;
        self.status = msg;
    }

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
    pub(super) fn launch_with_unsaved_saved(&mut self, launch: impl FnOnce(&mut Self)) {
        self.run_with_unsaved_saved("the launch", launch);
    }

    /// [`Self::launch_with_unsaved_saved`] for anything that runs the
    /// project's files: a task, a Testing run (#1639). VS Code saves before
    /// a task by default (`task.saveBeforeRun`); without it a build or test
    /// run reported on code the editor had already changed. `what` names
    /// the run in the warning.
    pub(super) fn run_with_unsaved_saved(&mut self, what: &str, launch: impl FnOnce(&mut Self)) {
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
    /// tab first: what [`Self::launch_with_unsaved_saved`] warns about.
    pub(super) fn unsaved_editor_names(&self) -> Vec<String> {
        let mut seen: Vec<&std::path::Path> = Vec::new();
        let mut names = Vec::new();
        let groups = std::iter::once(&self.editor).chain(self.editor_layout.inactive_groups());
        let active = self.editor.editors.get(self.editor.active_index());
        let tabs = active
            .into_iter()
            .chain(groups.flat_map(|g| g.editors.iter()));
        for ed in tabs.filter(|e| e.dirty) {
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
    pub(super) fn start_selected_debug(&mut self) {
        self.launch_with_unsaved_saved(Self::start_selected_debug_now);
    }

    pub(super) fn start_selected_debug_now(&mut self) {
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

    /// Launch a launch.json compound: resolve its members against the
    /// un-deduped config list, refuse what croft cannot honour, then start
    /// one or several sessions. Shared by the picker and by F5/restart once
    /// the compound is the selected target (#567).
    pub(super) fn launch_compound(&mut self, compound: &crate::dap::configs::Compound) {
        // Resolve first: a compound naming a configuration no
        // launch.json declares is a config error worth
        // reporting now, separately from the unbuilt feature.
        // The un-deduped list: `debug_configs` is the picker's
        // display list and drops a lower-precedence duplicate by
        // name, which is the very entry a `.vscode` compound
        // naming that name has to bind.
        let all = crate::dap::configs::discover_configs_all(&self.active_workspace_root());
        let folders = self.workspace_folders();
        // Every `${input:…}` the members and the compound's task use is asked
        // once, up front, and shared (#1424).
        if let Ok(members) = crate::dap::configs::resolve_compound(compound, &all, &folders) {
            let mut refs = Vec::new();
            let mut defs = Vec::new();
            for m in &members {
                for r in m.input_refs() {
                    if !refs.contains(&r) {
                        refs.push(r);
                    }
                }
                defs.extend(m.inputs.iter().cloned());
            }
            if let Some(label) = &compound.pre_launch_task {
                self.task_inputs(label, &mut refs, &mut defs);
            }
            let then = InputThen::Compound(Box::new(compound.clone()));
            match self.ask_inputs(&refs, &defs, &compound.answers, then) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => {
                    self.debug_error(format!("compound \"{}\": {e}", compound.name));
                    return;
                }
            }
        }
        // #310 defers compounds that need SEVERAL sessions at
        // once. A compound naming ONE configuration needs one,
        // which croft has run since #250 — so the arity has to
        // be READ here rather than assumed. Reporting the
        // multi-session limit for a one-member compound cites
        // a limitation that does not apply and tells the user
        // something false about their own launch.json.
        match crate::dap::configs::resolve_compound(compound, &all, &folders) {
            Err(e) => self.debug_error(e),
            // A one-member compound carrying keys croft does
            // not honour is REFUSED rather than launched.
            // Since #318 completed, only VALUES croft cannot
            // READ remain: a `presentation` that is not an
            // object, or a `hidden`/`group`/`order` that is
            // not a bool/string/number. Every key that says
            // something croft can act on is honoured -
            // `preLaunchTask` by the branch below, `hidden`,
            // `group` and `order` by the picker's sort.
            // Launching while ignoring what the compound asked
            // for is the "silently debug something other than
            // what was asked for" outcome every guard here
            // exists to prevent.
            // Asked of BOTH arities (#310). It sat on the
            // one-member arm because the other refused anyway;
            // now that both launch, a malformed key would be
            // refused at one member and ignored at two - the
            // "silently debug something other than what was
            // asked for" outcome this guard exists to prevent.
            Ok(members) if !compound.unsupported_keys.is_empty() => {
                // #318, NOT #310: this compound needs ONE
                // session, which croft runs. What it cannot do
                // is honour the key. Citing the multi-session
                // limit here would be the same false claim the
                // launching branch below exists to stop making.
                //
                // The remedy names both ways out: fix the
                // value, or run the member directly. Running
                // it directly DOES skip the compound's own
                // preLaunchTask, which the message leaves
                // unsaid on purpose - a test pins that the
                // status never mentions preLaunchTask, since
                // naming it here would read as advice to
                // accept the skip.
                // Up to three keys can be listed now, so
                // "a and b and c" would read as one run-on;
                // the last pair keeps the "and".
                let keys = match compound.unsupported_keys.split_last() {
                    Some((last, [])) => (*last).to_string(),
                    Some((last, rest)) => {
                        format!("{} and {last}", rest.join(", "))
                    }
                    None => String::new(),
                };
                let plural = if compound.unsupported_keys.len() > 1 {
                    "those keys"
                } else {
                    "that key"
                };
                // Name EVERY member, not `members[0]`. The
                // guard now covers both arities, and telling
                // the user of a three-member compound to run
                // the first one directly is advice to launch
                // a SUBSET - the precise outcome this arm
                // refuses to produce itself.
                let all = members
                    .iter()
                    .map(|m| format!("\"{}\"", m.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let run_them = if members.len() > 1 {
                    format!("or run {all} directly, in that order")
                } else {
                    format!("or run {all} directly")
                };
                self.debug_error(format!(
                    "compound \"{}\" sets {}, which croft cannot read — fix {} to launch {} through the compound, {}",
                    compound.name, keys, plural, all, run_them
                ));
            }
            Ok(members) if members.len() == 1 => {
                let mut cfg = members[0].clone();
                cfg.answers.extend(compound.answers.clone());
                // One member has no siblings to stop, so its `stopAll` is set
                // false rather than inherited from whatever ran before.
                self.debug_stop_all = false;
                match compound.pre_launch_task.clone() {
                    // #318: the compound's own task runs
                    // first, and the member starts only if it
                    // exits 0 - the same contract a config's
                    // own task already gets. Chained rather
                    // than merged: the member may declare a
                    // task too, and VS Code runs both.
                    Some(task) => self.launch_after_compound_task(&task, cfg),
                    None => self.launch_debug_config(&cfg),
                }
            }
            Ok(members) => {
                // Several sessions at once: what #310 built
                // the set for. Members start in DECLARATION
                // order, which is the order the file asked
                // for and the order a client-then-server
                // compound depends on.
                let label = compound.name.clone();
                // Each member carries the answers asked once for
                // the whole compound (#1424).
                let members: Vec<_> = members
                    .into_iter()
                    .map(|mut m| {
                        m.answers.extend(compound.answers.clone());
                        m
                    })
                    .collect();
                match compound.pre_launch_task.clone() {
                    Some(task) => self.launch_after_compound_task_all(
                        &task,
                        members,
                        label,
                        compound.stop_all,
                    ),
                    None => self.launch_compound_members(members, label, compound.stop_all),
                }
            }
        }
    }

    /// Debug: Add Configuration… (#250): the native form for a launch.json
    /// entry, one step at a time, written to `.croft/launch.json` at the end.
    /// Starts with the adapter.
    pub fn open_add_debug_config(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        self.debug_config_draft = None;
        let rows = crate::dap::configs::DRAFT_TYPES
            .iter()
            .map(|(id, label)| ListRow {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(
                ListPurpose::DebugConfigType,
                "Add Configuration: Debugger",
                rows,
            ),
            "",
        );
    }

    /// Ask for `field` of the draft, seeded with `retry` (a value just
    /// refused) or the field's default; `None` means the draft is complete,
    /// so write it.
    pub(super) fn prompt_debug_config_field(
        &mut self,
        field: Option<crate::dap::configs::DraftField>,
        retry: Option<&str>,
    ) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some(draft) = self.debug_config_draft.as_ref() else {
            return;
        };
        let Some(field) = field else {
            self.finish_debug_config_draft();
            return;
        };
        let (title, hint, seed) = draft.prompt(field);
        let mut prompt = InputPrompt::new(InputPurpose::DebugConfigField { field }, title, hint)
            .with_value(retry.map_or(seed, str::to_string));
        if crate::dap::configs::ConfigDraft::optional(field) {
            prompt = prompt.allowing_blank();
        }
        self.open_input_prompt(prompt);
    }

    /// Take a field's value: on to the next field, or back to this one with
    /// the reason when it does not fit.
    pub(super) fn submit_debug_config_field(
        &mut self,
        field: crate::dap::configs::DraftField,
        value: &str,
    ) {
        let Some(draft) = self.debug_config_draft.as_mut() else {
            return;
        };
        match draft.set(field, value) {
            Ok(()) => {
                let next = draft.next_field(Some(field));
                self.prompt_debug_config_field(next, None);
            }
            Err(why) => {
                self.status = why;
                self.prompt_debug_config_field(Some(field), Some(value));
            }
        }
    }

    /// Write the finished draft to `.croft/launch.json` and pick up the file
    /// again, so the new configuration is in the picker and F5 at once.
    pub(super) fn finish_debug_config_draft(&mut self) {
        let Some(draft) = self.debug_config_draft.take() else {
            return;
        };
        let root = self.active_workspace_root();
        match crate::dap::configs::add_to_croft_launch_json(&root, &draft) {
            Ok(_) => {
                self.debug_configs = crate::dap::configs::discover_configs(&root);
                self.selected_debug_config = Some(draft.name.clone());
                self.selected_debug_compound = None;
                self.run_debug.selected_config = Some(draft.name.clone());
                self.status = format!(
                    "Added \"{}\" to .croft/launch.json and selected it; F5 starts it",
                    draft.name
                );
            }
            Err(why) => self.status = why,
        }
    }

    /// Debug: Select and Start Debugging — the launch.json configurations
    /// plus the synthesized zero-config entry.
    pub fn open_debug_config_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let root = self.active_workspace_root();
        self.debug_configs = crate::dap::configs::discover_configs(&root);
        self.debug_compounds = crate::dap::configs::discover_compounds(&root);
        let mut rows = vec![ListRow {
            id: String::from("active"),
            label: String::from("Debug active file — no configuration"),
        }];
        // Configurations and compounds are ONE list to sort, which is what
        // VS Code does: a config and a compound can name the same
        // `presentation.group`, and sorting them separately would split that
        // group in two no matter what the file asked for (#318).
        //
        // Every id is built BEFORE the sort, from `enumerate` over the full
        // unsorted list, so `compound:{i}` and `{i}` keep naming the same
        // entries however the rows move. Indexing the sorted sequence would
        // launch a different configuration than the row says as soon as any
        // entry declares a `presentation`.
        let entries: Vec<(ListRow, Option<crate::dap::configs::Presentation>)> = self
            .debug_configs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    ListRow {
                        id: i.to_string(),
                        label: format!("{} — {} · {}", c.name, c.type_name, c.source),
                    },
                    c.presentation.clone(),
                )
            })
            // Compounds are listed so a workspace's own launch.json is
            // reflected honestly. Selecting one LAUNCHES it when it names a
            // single configuration and carries no key croft cannot honour;
            // otherwise it reports what is missing (#310) rather than
            // launching a subset, which would debug something other than what
            // was asked for.
            .chain(self.debug_compounds.iter().enumerate().map(|(i, c)| {
                (
                    ListRow {
                        id: format!("compound:{i}"),
                        label: format!(
                            "{} — compound of {}",
                            c.name,
                            c.configurations.join(", ")
                        ),
                    },
                    c.presentation.clone(),
                )
            }))
            .collect();
        rows.extend(
            crate::dap::configs::visible_and_sorted(entries, |(_, p)| p.as_ref())
                .into_iter()
                .map(|(row, _)| row),
        );
        rows.push(ListRow {
            id: String::from("add"),
            label: String::from("Add Configuration…"),
        });
        self.open_list_picker(
            ListPicker::new(ListPurpose::DebugConfig, "Debug Configuration", rows),
            "No debug configurations (.croft/launch.json or .vscode/launch.json)",
        );
    }

    /// Launch one member INTO the running set, leaving its siblings alone.
    ///
    /// `launch_debug_config` opens by stopping whatever is running, which is
    /// right for a configuration the user picked and fatal for a compound's
    /// second member. This is the same path with that first step removed and
    /// the session added rather than substituted.
    ///
    /// A member with its own `preLaunchTask` is NOT parked here: parking is a
    /// one-at-a-time mechanism, and a compound whose members each park would
    /// interleave unpredictably. Its task is skipped with a status saying so,
    /// which is honest, and #310 is where the general form belongs.
    pub(super) fn launch_member_into_set(
        &mut self,
        cfg: &crate::dap::configs::DebugConfig,
    ) -> MemberOutcome {
        // A member from another workspace folder resolves there (#1425).
        let root = cfg
            .root
            .clone()
            .unwrap_or_else(|| self.active_workspace_root());
        let ctx = crate::dap::configs::SubstCtx {
            workspace_folder: root.clone(),
            file: self.editor.path.clone(),
            inputs: cfg.answers.clone(),
            folders: self.workspace_folders(),
        };
        let mut rc = match crate::dap::configs::resolve(cfg, &ctx) {
            Ok(rc) => rc,
            Err(e) => {
                self.debug_error(format!("config \"{}\": {e}", cfg.name));
                return MemberOutcome::Failed;
            }
        };
        // js-debug's source-map defaults follow the folder the member was
        // resolved against (#1414), not the one the active file is in.
        if rc.kind == crate::dap::session::AdapterKind::JsDebug {
            rc.extra
                .entry("__workspaceFolder")
                .or_insert_with(|| serde_json::json!(root.to_string_lossy()));
        }
        self.record_post_debug_task(&rc);
        if rc.pre_launch_task.is_some() {
            // NOT launched. Running it anyway would debug whatever the last
            // build left behind, which is exactly what the single-member path
            // refuses to do for the same hazard - and a status message would
            // not have helped, since the caller overwrites it before a frame
            // is drawn. The caller collects the name and says so.
            return MemberOutcome::TaskUnrun;
        }
        // Ask the SET whether a session appeared, rather than trusting the
        // call to have made one: `launch_resolved_into_set` returns `()` on
        // every validation, spawn and attach failure alike, so reporting
        // `Launched` unconditionally counted members that never started and
        // made "N of M started" name an N the user does not have.
        let before = self.debug_sessions.len();
        let building = self.pending_cargo_launches.len();
        self.launch_resolved_into_set(rc);
        if self.debug_sessions.len() > before {
            MemberOutcome::Launched
        } else if self.pending_cargo_launches.len() > building {
            MemberOutcome::Building
        } else {
            MemberOutcome::Failed
        }
    }

    /// Park a whole compound behind its own `preLaunchTask` (#310).
    ///
    /// The single-member form parks one config; this parks the list, because
    /// the members have to start together once the task exits 0. Running them
    /// through the single-member path in turn would stop each previous one.
    pub(super) fn launch_after_compound_task_all(
        &mut self,
        task_label: &str,
        members: Vec<crate::dap::configs::DebugConfig>,
        compound: String,
        stop_all: bool,
    ) {
        self.debug_stop();
        self.pending_debug_launch = None;
        let root = self.active_workspace_root();
        let tasks = crate::tasks::discover_tasks(&root);
        let task = match crate::tasks::find_task(tasks, task_label) {
            Ok(task) => task,
            Err(near) => {
                self.debug_error(format!(
                    "compound preLaunchTask \"{task_label}\" not found{} — Tasks: Run Task lists what the workspace declares",
                    did_you_mean(near)
                ));
                return;
            }
        };
        let answers = members
            .first()
            .map(|m| m.answers.clone())
            .unwrap_or_default();
        let task = with_answers(task, &answers);
        let command = self
            .task_command_line(&task)
            .unwrap_or_else(|_| task.command.clone());
        let step = PendingLaunchStep::Members(Box::new(CompoundLaunch {
            members,
            compound,
            stop_all,
        }));
        let folder = self.active_workspace_root();
        self.park_launch_behind(
            task,
            folder,
            command,
            step,
            "compound preLaunchTask",
            "its sessions start",
        );
    }

    /// Run `task` in `folder` and park `step` until it is done, as its [`PreLaunchWait`]
    /// says (#250, #1541). `what` names the task in the status ("preLaunchTask",
    /// "compound preLaunchTask") and `starts` says what follows it.
    pub(super) fn park_launch_behind(
        &mut self,
        task: crate::tasks::Task,
        folder: PathBuf,
        command: String,
        step: PendingLaunchStep,
        what: &str,
        starts: &str,
    ) {
        let task_label = task.label.clone();
        let wait = PreLaunchWait::of(&task);
        let Some(pane) = self.run_project_task_in(task, folder) else {
            // run_project_task already reported why the pane failed.
            return;
        };
        if wait == PreLaunchWait::Untracked {
            // VS Code warns that such a task cannot be tracked; waiting for
            // an exit that never comes is the hang this avoids.
            crate::output::push(
                "Tasks",
                crate::output::OutputLevel::Warn,
                &format!(
                    "{what} \"{task_label}\" is a background task with no problem matcher that marks when its build ends, so debugging started without waiting for it"
                ),
            );
            self.resume_pending_launch(Some(0), step);
            return;
        }
        self.pending_debug_launch = Some(PendingDebugLaunch {
            pane,
            command,
            step,
            started: std::time::Instant::now(),
            watch: wait == PreLaunchWait::FirstBuild,
        });
        self.run_debug.feedback = Some(format!("{what} \"{task_label}\" running…"));
        self.run_debug.feedback_is_error = false;
        let when = if wait == PreLaunchWait::FirstBuild {
            "when its first build ends"
        } else {
            "when it exits 0"
        };
        self.status = format!("{what} \"{task_label}\" running — {starts} {when}");
    }

    /// Launch every member of a compound, in declaration order (#310).
    ///
    /// The first member REPLACES whatever was running, matching what starting
    /// any configuration has always done; the rest are pushed beside it. That
    /// asymmetry is the point - `launch_debug_config` stops the world before
    /// it starts, so calling it per member would leave only the last one
    /// alive, which is the bug this path exists to avoid.
    ///
    /// A member that fails to start does not abort the ones already running:
    /// they are real processes, and killing them because a later sibling
    /// could not resolve would be a second failure caused by the first. The
    /// error names the member, and the survivors stay.
    pub(super) fn launch_compound_members(
        &mut self,
        members: Vec<crate::dap::configs::DebugConfig>,
        compound: String,
        stop_all: bool,
    ) {
        self.debug_stop();
        self.debug_post_tasks.clear();
        // Set only here, once the old set is gone: a compound refused before
        // this point leaves the running set untouched, and must leave its
        // `stopAll` out of it too (#567).
        self.debug_stop_all = stop_all;
        // A launch supersedes any older one still parked behind its task:
        // left in place, that task's exit would launch its config and stop
        // this set (#567). The task-gated path reaches here via `take()`, so
        // this only ever drops a STALE parking.
        self.pending_debug_launch = None;
        self.debug_build = None;
        let total = members.len();
        let mut started = 0usize;
        let mut building: Vec<String> = Vec::new();
        let mut unrun: Vec<String> = Vec::new();
        for cfg in members {
            match self.launch_member_into_set(&cfg) {
                MemberOutcome::Launched => started += 1,
                MemberOutcome::Building => building.push(cfg.name.clone()),
                MemberOutcome::TaskUnrun => unrun.push(cfg.name.clone()),
                MemberOutcome::Failed => {}
            }
        }
        if started == 0 && !building.is_empty() {
            // Every member that could start is still building with cargo
            // (#1392): each joins the set when its build lands.
            self.debug_compound = Some(compound.clone());
            self.run_debug.feedback = Some(format!(
                "compound {compound}: building {} \u{2014} sessions start when cargo finishes",
                building.join(", ")
            ));
            self.run_debug.feedback_is_error = building.len() != total;
            self.status = format!(
                "compound \"{compound}\": building {} \u{2014} sessions start when cargo finishes",
                building.join(", ")
            );
            return;
        }
        if started == 0 {
            // Nothing started. Each member has already said why, so this names
            // the compound and, if that is the reason, the members croft
            // declined to launch against a build it could not run.
            let why = if unrun.is_empty() {
                String::new()
            } else {
                format!(
                    " — {} declares a preLaunchTask croft cannot run as part of a set, and starting it anyway would debug the last build",
                    unrun.join(", ")
                )
            };
            self.debug_error(format!("compound \"{compound}\" started no sessions{why}"));
            return;
        }
        // The FIRST member takes focus, not the last: a compound is usually
        // written server-first, and landing the user on whichever happened to
        // start last would put them in the wrong one.
        self.debug_sessions.focus(0);
        self.debug_compound = Some(compound.clone());
        // Reveal FIRST, then write the feedback: `reveal_debug_view` runs
        // `refresh_run_debug`, which clears `feedback` unconditionally, so
        // doing it afterwards wiped every line built below - including the
        // only place a skipped member is ever named - before it was drawn.
        //
        // Once for the compound, not once per member: each member's launch
        // reveals the view too, and doing it per member clears the console
        // and re-focuses the pane for every one of them.
        self.reveal_debug_view();
        // Name them. With one session "the debugger" was unambiguous; with
        // several the user has to be told which are running and which one the
        // call stack and variables belong to, or the view describes a session
        // they cannot identify.
        let mut running = self.debug_sessions.names().join(", ");
        if !building.is_empty() {
            // Not failed: they join the set when their cargo builds land.
            running = format!("{running}; building {}", building.join(", "));
        }
        let started = started + building.len();
        self.run_debug.feedback = Some(if started == total {
            format!("Debugging compound {compound}: {running}")
        } else if unrun.is_empty() {
            format!("Debugging compound {compound}: {running} ({started} of {total} started)")
        } else {
            // Named, not silent: the member is absent because croft would
            // otherwise debug an unbuilt binary, and the user has to know
            // which one to run themselves.
            format!(
                "Debugging compound {compound}: {running} ({started} of {total}; {} needs its preLaunchTask run first)",
                unrun.join(", ")
            )
        });
        self.run_debug.feedback_is_error = started != total;
        let focused = self
            .debug_sessions
            .focused_name()
            .unwrap_or("?")
            .to_string();
        self.status = format!(
            "Debugging compound {compound} — {started} session(s), showing {focused} · Shift+F5 stops all"
        );
    }

    /// Continue a parked launch whose `preLaunchTask` has finished.
    ///
    /// Split from the terminal sweep so it can be driven directly: reaching it
    /// through the sweep needs a pane emitting OSC 133 marks, and the branch
    /// that matters most - a compound's task completing, then its MEMBER
    /// starting (#318) - is invisible to every test that only checks the
    /// launch parked.
    pub(super) fn resume_pending_launch(&mut self, exit: Option<i32>, step: PendingLaunchStep) {
        if exit != Some(0) {
            let code = exit.map_or_else(|| String::from("?"), |c| c.to_string());
            self.debug_error(format!(
                "preLaunchTask exited {code} — debug launch aborted (its pane has the output)"
            ));
            return;
        }
        match step {
            PendingLaunchStep::Resolved(rc) => self.launch_resolved_config(*rc),
            // Back through the front door: the member may have a
            // `preLaunchTask` of its own, and VS Code runs the compound's AND
            // the member's. Calling the resolved-launch path here would
            // silently skip the member's.
            PendingLaunchStep::Member(cfg) => self.launch_debug_config(&cfg),
            // NOT back through the front door, unlike the single member above:
            // `launch_debug_config` stops the world first, so the second
            // member would kill the first (#310). The members' own
            // `preLaunchTask`s are skipped, with a status saying so per
            // member - parking is one-at-a-time and a set of parked members
            // would interleave.
            PendingLaunchStep::Members(launch) => {
                self.launch_compound_members(launch.members, launch.compound, launch.stop_all)
            }
        }
    }
}
