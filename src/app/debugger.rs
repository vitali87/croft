//! Run and Debug: launch configs and Add Configuration, compounds and preLaunchTask waits,
//! adapters, debug builds, attach pickers, the paused view, watches, the console,
//! stepping, every kind of breakpoint, Debug Test and inline values.

use super::*;

mod active_file;
mod adapters;
mod builds;
mod compounds;
mod configs;
mod console;
mod debug_test;
mod events;
mod launch;
mod python;
mod view;
mod watches;
pub(super) use adapters::lldb_dap_missing_message;
pub(super) use adapters::lldb_dap_path;
#[cfg(test)]
pub(super) use adapters::lldb_dap_rank;
use adapters::*;
pub(super) use compounds::finished_settles_pending;
pub(super) use compounds::pending_abort_reason;
use console::*;
#[cfg(test)]
pub(super) use events::debug_end_message;
#[cfg(test)]
pub(super) use events::index_after_removals;
#[cfg(test)]
pub(crate) use launch::no_display;
#[cfg(test)]
pub(super) use launch::resolve_lldb_program;
pub(super) use python::config_project_python;
pub(super) use python::debug_test_python;
pub(super) use python::debuggee_python;
pub(super) use python::debuggee_python_note;
pub(super) use python::launch_request_python;
pub(super) use python::project_python_for;
use python::*;

/// Grace before the post-teardown js-debug orphan sweep runs, letting a
/// just-killed server's detached watchdog reparent to init so the sweep can
/// recognise it as an orphan. See [`crate::dap::reaper`].
pub(super) const REAPER_TEARDOWN_GRACE_MS: u64 = 250;

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

/// How long a parked launch waits before concluding the pane will never
/// report a command mark. Only consulted while the pane's foreground is back
/// to a plain shell — a long build keeps a non-shell foreground and waits
/// indefinitely, exactly like the FinishedCommand path.
pub(super) const PRELAUNCH_MARK_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

impl App {
    /// The editor's breakpoints in launch shape, shared by every session
    /// starter (zero-config and launch.json alike).
    pub(super) fn collect_editor_breakpoints(
        &self,
    ) -> std::collections::BTreeMap<PathBuf, Vec<crate::dap::session::SourceBreakpoint>> {
        self.editor
            .breakpoints
            .iter()
            .map(|(p, lines)| (p.clone(), self.editor.source_breakpoints(p, lines)))
            .collect()
    }

    /// F9: toggle a breakpoint on the cursor line, pushing it live if a session
    /// is running.
    pub fn debug_toggle_breakpoint(&mut self) {
        self.debug_toggle_breakpoint_line(self.editor.cursor_row + 1);
    }

    /// Toggle a breakpoint on an explicit 1-based `line`, then push the file's
    /// breakpoints to a live session. Shared by F9 (cursor line) and the gutter
    /// right-click menu (clicked line).
    pub(super) fn debug_toggle_breakpoint_line(&mut self, line: usize) {
        match self.editor.toggle_breakpoint_line(line) {
            Some((_, line, now_set)) => {
                self.status = if now_set {
                    format!("Breakpoint set at line {line}")
                } else {
                    format!("Breakpoint removed from line {line}")
                };
            }
            None => {
                self.status = String::from("Open a file to set a breakpoint");
                return;
            }
        }
        if let Some(path) = self.editor.path.clone() {
            let specs = self
                .editor
                .breakpoints
                .get(&path)
                .map(|lines| self.editor.source_breakpoints(&path, lines))
                .unwrap_or_default();
            // Every session, not the focused one (#310): breakpoints are
            // global editor state, handed to every session at launch, so a
            // breakpoint set mid-run in a shared file must bind in all of
            // them or it binds in the server and not the client.
            for session in self.debug_sessions.iter_mut() {
                session.update_breakpoints(&path, &specs);
            }
        }
    }

    /// Open the breakpoint-condition editor for the cursor line, pre-filled with
    /// any existing condition.
    pub fn debug_edit_condition(&mut self) {
        self.debug_edit_condition_line(self.editor.cursor_row + 1);
    }

    /// Open the breakpoint-condition editor for an explicit 1-based `line` as a
    /// centered popup (`PromptKind::BreakpointCondition`), pre-filled with any
    /// existing condition. Shared by the command-palette entry (cursor line)
    /// and the gutter right-click menu (clicked line). The popup, not the
    /// status line, captures the input; commit lives in [`App::commit_prompt`].
    pub(super) fn debug_edit_condition_line(&mut self, line: usize) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open a file to set a conditional breakpoint");
            return;
        };
        let existing = self
            .editor
            .breakpoint_conditions
            .get(&path)
            .and_then(|c| c.get(&line))
            .cloned()
            .unwrap_or_default();
        let label = if existing.is_empty() {
            format!("Add Conditional Breakpoint · line {line}")
        } else {
            format!("Edit Condition · line {line}")
        };
        let target_dir = self.tree.root.clone();
        self.prompt = Some(Prompt {
            label,
            buffer: existing,
            caret: usize::MAX,
            kind: PromptKind::BreakpointCondition { path, line },
            target_dir,
            error: None,
        });
    }

    /// Commit the breakpoint-condition popup: ensure a breakpoint exists on the
    /// line, attach the typed expression (or clear it when blank), and push the
    /// file's breakpoints to a live session.
    pub(super) fn commit_breakpoint_condition(&mut self, path: PathBuf, line: usize, expr: &str) {
        self.editor
            .breakpoints
            .entry(path.clone())
            .or_default()
            .insert(line);
        let cond = expr.trim();
        if cond.is_empty() {
            if let Some(c) = self.editor.breakpoint_conditions.get_mut(&path) {
                c.remove(&line);
            }
            self.status = format!("Plain breakpoint at line {line}");
        } else {
            self.editor
                .breakpoint_conditions
                .entry(path.clone())
                .or_default()
                .insert(line, cond.to_string());
            self.status = format!("Conditional breakpoint at line {line}");
        }
        let specs = self
            .editor
            .breakpoints
            .get(&path)
            .map(|l| self.editor.source_breakpoints(&path, l))
            .unwrap_or_default();
        // Every session (#310); see `debug_toggle_breakpoint_line`.
        for session in self.debug_sessions.iter_mut() {
            session.update_breakpoints(&path, &specs);
        }
    }

    /// A one-line prompt with no pre-fill, for the #611 breakpoint kinds.
    pub(super) fn open_debug_prompt(&mut self, label: String, buffer: String, kind: PromptKind) {
        let target_dir = self.tree.root.clone();
        self.prompt = Some(Prompt {
            label,
            buffer,
            caret: usize::MAX,
            kind,
            target_dir,
            error: None,
        });
    }

    /// Debug: Add Hit Count Breakpoint (#611), on the cursor line.
    pub fn debug_edit_hit_condition(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open a file to set a hit count breakpoint");
            return;
        };
        let line = self.editor.cursor_row + 1;
        let existing = self
            .editor
            .breakpoint_hit_conditions
            .get(&path)
            .and_then(|h| h.get(&line))
            .cloned()
            .unwrap_or_default();
        self.open_debug_prompt(
            format!("Pause after how many hits? · line {line}"),
            existing,
            PromptKind::HitCondition { path, line },
        );
    }

    /// Commit the hit-count popup: ensure a breakpoint on the line, attach
    /// the count (or clear it when blank), and push the file's set to every
    /// session.
    pub(super) fn commit_hit_condition(&mut self, path: PathBuf, line: usize, hits: &str) {
        self.editor
            .breakpoints
            .entry(path.clone())
            .or_default()
            .insert(line);
        let hits = hits.trim();
        if hits.is_empty() {
            if let Some(h) = self.editor.breakpoint_hit_conditions.get_mut(&path) {
                h.remove(&line);
            }
            self.status = format!("Plain breakpoint at line {line}");
        } else {
            self.editor
                .breakpoint_hit_conditions
                .entry(path.clone())
                .or_default()
                .insert(line, hits.to_string());
            self.status = format!("Breakpoint at line {line} pauses on hit {hits}");
        }
        let specs = self
            .editor
            .breakpoints
            .get(&path)
            .map(|l| self.editor.source_breakpoints(&path, l))
            .unwrap_or_default();
        for session in self.debug_sessions.iter_mut() {
            session.update_breakpoints(&path, &specs);
        }
    }

    /// Debug: Add Function Breakpoint (#611).
    pub fn debug_add_function_breakpoint(&mut self) {
        self.open_debug_prompt(
            String::from("Break on entry to function"),
            String::new(),
            PromptKind::FunctionBreakpoint,
        );
    }

    pub(super) fn commit_function_breakpoint(&mut self, name: String) {
        if name.is_empty() || self.function_breakpoints.contains(&name) {
            return;
        }
        self.function_breakpoints.push(name.clone());
        self.push_function_breakpoints();
        self.status = format!("Function breakpoint: {name}");
    }

    /// Debug: Remove All Function Breakpoints (#611).
    pub fn debug_clear_function_breakpoints(&mut self) {
        self.function_breakpoints.clear();
        self.push_function_breakpoints();
        self.status = String::from("Function breakpoints removed");
    }

    pub(super) fn push_function_breakpoints(&mut self) {
        let names = self.function_breakpoints.clone();
        let mut refused = false;
        for session in self.debug_sessions.iter_mut() {
            if session.supports("supportsFunctionBreakpoints") {
                session.set_function_breakpoints(names.clone());
            } else {
                refused = true;
            }
        }
        if refused && !names.is_empty() {
            self.status = String::from("This debugger does not support function breakpoints");
        }
    }

    /// Debug: Run to Cursor (#611, Ctrl+F10): resume and pause once at the
    /// caret line, without leaving a breakpoint behind.
    pub fn debug_run_to_cursor(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let line = self.editor.cursor_row as u32 + 1;
        match self.debug_sessions.focused_mut() {
            Some(session) if session.stopped_thread.is_some() => {
                session.run_to_cursor(&path, line);
                self.status = format!("Running to line {line}");
            }
            Some(_) => self.status = String::from("Run to Cursor needs the debugger paused"),
            None => self.status = String::from("Run to Cursor: no debug session"),
        }
    }

    /// Debug: Break When Value Changes (#611): prompt for a variable of the
    /// paused frame.
    pub fn debug_break_on_value_change(&mut self) {
        match self.debug_sessions.focused() {
            Some(s) if s.stopped_thread.is_none() => {
                self.status = String::from("Break When Value Changes needs the debugger paused");
            }
            Some(s) if !s.supports("supportsDataBreakpoints") => {
                self.status = String::from("This debugger does not support data breakpoints");
            }
            Some(_) => self.open_debug_prompt(
                String::from("Break when this variable changes"),
                String::new(),
                PromptKind::DataBreakpoint,
            ),
            None => self.status = String::from("Break When Value Changes: no debug session"),
        }
    }

    pub(super) fn commit_data_breakpoint(&mut self, name: &str) {
        let Some(session) = self.debug_sessions.focused_mut() else {
            return;
        };
        // The variable's container: the scope (or expanded variable) that
        // lists it, which is what `dataBreakpointInfo` is asked against.
        let container = session.scopes.iter().map(|s| s.variables_ref).find(|r| {
            session
                .variables
                .get(r)
                .is_some_and(|vs| vs.iter().any(|v| v.name == name))
        });
        match container {
            Some(reference) => {
                session.break_on_change(reference, name);
                self.status = format!("Pausing when {name} changes");
            }
            None => self.status = format!("No variable named {name} in the paused frame"),
        }
    }

    /// Debug: Remove All Data Breakpoints (#611).
    pub fn debug_clear_data_breakpoints(&mut self) {
        for session in self.debug_sessions.iter_mut() {
            session.clear_data_breakpoints();
        }
        self.status = String::from("Data breakpoints removed");
    }

    /// Open the logpoint editor for the cursor line, pre-filled with any
    /// existing message.
    pub fn debug_edit_logpoint(&mut self) {
        self.debug_edit_logpoint_line(self.editor.cursor_row + 1);
    }

    /// Open the logpoint editor for an explicit 1-based `line` as a centered
    /// popup (`PromptKind::Logpoint`). Shared by the palette entry (cursor
    /// line), Shift+Alt+F9, and the gutter right-click menu (clicked line).
    pub(super) fn debug_edit_logpoint_line(&mut self, line: usize) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open a file to set a logpoint");
            return;
        };
        let existing = self
            .editor
            .breakpoint_logs
            .get(&path)
            .and_then(|m| m.get(&line))
            .cloned()
            .unwrap_or_default();
        let label = if existing.is_empty() {
            format!("Add Logpoint · line {line}")
        } else {
            format!("Edit Logpoint · line {line}")
        };
        let target_dir = self.tree.root.clone();
        self.prompt = Some(Prompt {
            label,
            buffer: existing,
            caret: usize::MAX,
            kind: PromptKind::Logpoint { path, line },
            target_dir,
            error: None,
        });
    }

    /// Commit the logpoint popup: ensure a breakpoint exists on the line,
    /// attach the typed message (or clear it when blank), and push the file's
    /// breakpoints to a live session.
    pub(super) fn commit_logpoint(&mut self, path: PathBuf, line: usize, message: &str) {
        self.editor
            .breakpoints
            .entry(path.clone())
            .or_default()
            .insert(line);
        let msg = message.trim();
        if msg.is_empty() {
            if let Some(m) = self.editor.breakpoint_logs.get_mut(&path) {
                m.remove(&line);
            }
            self.status = format!("Plain breakpoint at line {line}");
        } else {
            self.editor
                .breakpoint_logs
                .entry(path.clone())
                .or_default()
                .insert(line, msg.to_string());
            self.status = format!("Logpoint at line {line}");
        }
        let specs = self
            .editor
            .breakpoints
            .get(&path)
            .map(|l| self.editor.source_breakpoints(&path, l))
            .unwrap_or_default();
        // Every session (#310); see `debug_toggle_breakpoint_line`.
        for session in self.debug_sessions.iter_mut() {
            session.update_breakpoints(&path, &specs);
        }
    }

    /// A click on the Run and Debug panel's debug toolbar (#1545): the same
    /// action as the button's key.
    pub(super) fn run_debug_control(&mut self, control: crate::widgets::run_debug::DebugControl) {
        use crate::widgets::run_debug::DebugControl;
        match control {
            DebugControl::Continue => self.debug_start_or_continue(),
            DebugControl::Pause => self.debug_pause(),
            DebugControl::StepOver => self.debug_step("next"),
            DebugControl::StepInto => self.debug_step("stepIn"),
            DebugControl::StepOut => self.debug_step("stepOut"),
            DebugControl::Restart => self.debug_restart(),
            DebugControl::Stop => self.debug_stop_by_user(),
        }
        self.refresh_debug_panel();
    }

    /// Debug: Continue (#1543): resume a paused session. Unlike F5 it never
    /// starts one, so a binding for it is inert with nothing to continue.
    pub(super) fn debug_continue(&mut self) {
        use crate::dap::session::SessionPhase;
        match self.debug_sessions.focused().map(|s| s.phase) {
            Some(SessionPhase::Stopped) => self.debug_start_or_continue(),
            Some(_) => self.status = String::from("The program is already running"),
            None => self.status = String::from("No debug session to continue"),
        }
    }

    /// Debug: Step Into Target (#1543): ask the adapter which calls the
    /// paused line makes; the answer arrives as `StepInTargets`. An adapter
    /// that cannot say steps in plainly, as F11 does.
    pub(super) fn debug_step_into_target(&mut self) {
        use crate::dap::session::{SessionPhase, StepInTargetsAsk};
        let Some(session) = self.debug_sessions.focused_mut() else {
            self.status = String::from("No debug session to step");
            return;
        };
        if session.phase != SessionPhase::Stopped {
            self.status = String::from("Step Into Target needs a paused program");
            return;
        }
        match session.request_step_in_targets() {
            StepInTargetsAsk::Sent => {}
            StepInTargetsAsk::NoFrame => {
                self.status =
                    String::from("The paused frame is still loading; try Step Into Target again");
            }
            StepInTargetsAsk::Unsupported => {
                self.status = String::from(
                    "This debugger cannot list step-in targets; stepping into the first call",
                );
                self.debug_step("stepIn");
            }
        }
    }

    /// The adapter's answer to Step Into Target: nothing to choose between
    /// steps in plainly, one target steps into it, several open a picker.
    pub(super) fn offer_step_in_targets(&mut self, targets: Vec<(i64, String)>) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        match targets.as_slice() {
            [] => self.debug_step_into(None),
            [(id, _)] => self.debug_step_into(Some(*id)),
            _ => {
                let rows = targets
                    .into_iter()
                    .map(|(id, label)| ListRow {
                        id: id.to_string(),
                        label,
                    })
                    .collect();
                self.open_list_picker(
                    ListPicker::new(ListPurpose::StepInTarget, "Step Into Target", rows),
                    "",
                );
            }
        }
    }

    /// Step into `target` on the paused line, or the first call to run.
    pub(super) fn debug_step_into(&mut self, target: Option<i64>) {
        match target {
            Some(id) => {
                if let Some(session) = self.debug_sessions.focused_mut() {
                    session.step_in_target(id);
                    self.editor.stop_line = None;
                    self.clear_inline_values();
                }
            }
            None => self.debug_step("stepIn"),
        }
    }

    /// F10/F11/Shift+F11: step the paused thread (`next` / `stepIn` / `stepOut`).
    pub fn debug_step(&mut self, command: &str) {
        if let Some(session) = self.debug_sessions.focused_mut() {
            session.step(command);
            self.editor.stop_line = None;
            self.clear_inline_values();
        }
    }

    /// Toggle "break on raised exceptions" for the active session, with the
    /// filter id this adapter uses for it (#1426). `uncaught` stays on so
    /// unhandled exceptions still pause. No-op without a session.
    pub fn debug_toggle_raised_exceptions(&mut self) {
        let Some(session) = self.debug_sessions.focused_mut() else {
            self.status = String::from("Start debugging first to set exception breakpoints");
            return;
        };
        let Some(on) = session.toggle_raised_exceptions() else {
            self.status = String::from(
                "This debugger has no filter for raised exceptions; exception breakpoints are unchanged",
            );
            return;
        };
        let uncaught = session.has_exception_filter("uncaught");
        self.status = String::from(match (on, uncaught) {
            (true, true) => "Breaking on raised exceptions (and uncaught)",
            (true, false) => "Breaking on raised exceptions",
            (false, true) => "Breaking on uncaught exceptions only",
            (false, false) => "No longer breaking on raised exceptions",
        });
    }

    /// F6: interrupt a running program so it stops at the current line.
    pub fn debug_pause(&mut self) {
        match self.debug_sessions.focused_mut() {
            Some(session) => {
                session.pause();
                self.status = String::from("Pausing");
            }
            None => self.status = String::from("No debug session to pause"),
        }
    }

    /// Restart: tear down the current session and relaunch the active file with
    /// the same breakpoints.
    pub fn debug_restart(&mut self) {
        if !self.debug_sessions.is_empty() {
            self.debug_stop();
        }
        self.start_selected_debug();
    }

    /// Every breakpoint for the BREAKPOINTS list (#250), by file then line,
    /// with its condition or log message.
    pub(super) fn breakpoint_items(&self) -> Vec<crate::widgets::run_debug::BreakpointItem> {
        let mut paths: Vec<&PathBuf> = self.editor.breakpoints.keys().collect();
        paths.sort();
        let mut items = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            for &line in &self.editor.breakpoints[path] {
                let condition = self
                    .editor
                    .breakpoint_conditions
                    .get(path)
                    .and_then(|m| m.get(&line));
                let log = self
                    .editor
                    .breakpoint_logs
                    .get(path)
                    .and_then(|m| m.get(&line));
                let detail = match (log, condition) {
                    (Some(msg), _) => Some(format!("log {msg}")),
                    (None, Some(cond)) => Some(format!("if {cond}")),
                    (None, None) => None,
                };
                items.push(crate::widgets::run_debug::BreakpointItem {
                    path: path.clone(),
                    line,
                    label: format!("{name}:{line}"),
                    detail,
                });
            }
        }
        items
    }

    /// Bring the panel's BREAKPOINTS list up to date, and the tree's section
    /// with it while a session is shown. A no-op when nothing changed.
    pub(super) fn sync_breakpoint_list(&mut self) {
        use crate::widgets::run_debug::DebugRowKind;
        let items = self.breakpoint_items();
        if items == self.run_debug.breakpoints {
            return;
        }
        let n = items.len();
        self.run_debug.breakpoints = items;
        if self.run_debug.debug_active {
            let rows = &mut self.run_debug.debug_rows;
            if let Some(pos) = rows.iter().position(
                |r| matches!(&r.kind, DebugRowKind::Header { title } if title == "BREAKPOINTS"),
            ) {
                rows.truncate(pos);
            }
            rows.extend(breakpoint_section_rows(n));
        }
    }

    /// A click in the BREAKPOINTS list: jump to the breakpoint, or remove it
    /// when the click landed on its `✕`.
    pub(super) fn breakpoint_list_click(&mut self, index: usize, remove: bool) {
        let Some(bp) = self.run_debug.breakpoints.get(index).cloned() else {
            return;
        };
        if remove {
            self.remove_breakpoint_at(&bp.path, bp.line);
        } else if let Err(e) = self.open_at(&bp.path, bp.line.saturating_sub(1), 0) {
            self.status = format!("Could not open {}: {e}", bp.path.display());
        }
        self.sync_breakpoint_list();
    }

    /// Remove the breakpoint at `path:line` (1-based) with its condition or
    /// log message, and push the file's breakpoints to every live session so
    /// it stops binding mid-run too, as F9 does.
    pub(super) fn remove_breakpoint_at(&mut self, path: &Path, line: usize) {
        if let Some(lines) = self.editor.breakpoints.get_mut(path) {
            lines.remove(&line);
            if lines.is_empty() {
                self.editor.breakpoints.remove(path);
            }
        }
        if let Some(m) = self.editor.breakpoint_conditions.get_mut(path) {
            m.remove(&line);
        }
        if let Some(m) = self.editor.breakpoint_logs.get_mut(path) {
            m.remove(&line);
        }
        let specs = self
            .editor
            .breakpoints
            .get(path)
            .map(|lines| self.editor.source_breakpoints(path, lines))
            .unwrap_or_default();
        let owned = path.to_path_buf();
        for session in self.debug_sessions.iter_mut() {
            session.update_breakpoints(&owned, &specs);
        }
        self.status = format!(
            "Breakpoint removed from {}:{line}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
    }

    /// Open the process picker for a `${command:pickProcess}` attach (#250),
    /// parking the resolved config until a process is chosen.
    pub(super) fn open_attach_process_picker(
        &mut self,
        rc: crate::dap::configs::ResolvedConfig,
        slot: LaunchSlot,
    ) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let processes = list_user_processes();
        if processes.is_empty() {
            self.debug_error(format!(
                "config \"{}\": no processes found to attach to",
                rc.name
            ));
            return;
        }
        let rows = processes
            .into_iter()
            .map(|(pid, comm)| ListRow {
                id: pid.to_string(),
                label: format!("{pid:>7}  {comm}"),
            })
            .collect();
        let title = format!("Attach \"{}\" to process", rc.name);
        self.pending_attach = Some((rc, slot));
        self.list_picker = Some(ListPicker::new(ListPurpose::AttachProcess, title, rows));
    }

    /// The parked attach with its `processId` set to the chosen `pid`, taken
    /// out so a second confirm cannot launch it twice.
    pub(super) fn take_pending_attach(
        &mut self,
        pid: i64,
    ) -> Option<(crate::dap::configs::ResolvedConfig, LaunchSlot)> {
        let (mut rc, slot) = self.pending_attach.take()?;
        rc.process_id = Some(pid);
        Some((rc, slot))
    }

    /// Shift+F5 and Debug: Stop Debugging. The user ending the run is when a
    /// `postDebugTask` runs (#250); the internal `debug_stop` a relaunch uses
    /// to replace the set is not, so it lives here rather than in there.
    pub(super) fn debug_stop_by_user(&mut self) {
        let was_running = !self.debug_sessions.is_empty();
        self.debug_stop();
        if was_running {
            self.run_post_debug_tasks();
        } else {
            self.debug_post_tasks.clear();
        }
    }

    /// Shift+F5: stop debugging and tear the session down.
    pub fn debug_stop(&mut self) {
        // EVERY session, not the focused one (#310). A compound launches
        // several; disconnecting only the one the user happens to be looking
        // at would leave its siblings running with nothing on screen owning
        // them, which is the orphaning the issue names.
        for (_, named) in self.debug_sessions.iter_named_mut_indexed() {
            mirror_open_console_lines(
                &mut named.console,
                &self.debug_console,
                self.debug_console_dropped,
            );
            named.session.disconnect();
        }
        self.debug_sessions.clear();
        // A `cargo` build still running for a launch (#1392) must not start
        // its debuggee after the user stopped debugging.
        self.pending_cargo_launches.clear();
        self.debug_compound = None;
        // A C/C++ build still running must not launch once it finishes
        // (#1359): Stop means stop.
        self.debug_build = None;
        self.reset_watch_runtime();
        // Sweep js-debug's detached watchdog (and any leftover tree) once it has
        // reparented to init after the server dies. See the Terminated arm in
        // `poll_dap` and `crate::dap::reaper`.
        crate::dap::reaper::reap_orphaned_js_debug_async(std::time::Duration::from_millis(
            REAPER_TEARDOWN_GRACE_MS,
        ));
        self.editor.stop_line = None;
        self.editor.unverified_breakpoints.clear();
        // The breakpoint croft set at the failing assertion goes with the
        // session that asked for it (#373).
        self.disarm_failure_breakpoint();
        self.clear_inline_values();
        self.status = String::from("Debug session stopped");
        // Clear the paused-state feedback ("Paused (breakpoint)") so the
        // empty-state panel doesn't keep showing it under the Run button.
        self.run_debug.feedback = None;
        self.run_debug.feedback_is_error = false;
        // Refresh now: `poll_dap` early-returns once the session is gone, so it
        // would never deactivate the panel on its own — without this the Run &
        // Debug panel keeps showing the stale "Paused" tree until the next event.
        self.refresh_debug_panel();
    }

    pub fn consume_process_picker_image_clear(&mut self) -> bool {
        self.overlays.process_picker_clear.consume()
    }

    /// Open the "Debug: Attach to Python Process" picker. Enumerates running
    /// CPython 3.14+ processes (those with PEP 768 `sys.remote_exec`); shows a
    /// status message and opens nothing when there are none.
    pub(super) fn open_attach_python_picker(&mut self) {
        if self.process_picker.is_some() {
            return;
        }
        let targets = crate::dap::discovery::attachable_python_targets();
        if targets.is_empty() {
            self.run_debug.feedback = Some(String::from(
                "No attachable Python 3.14+ processes found (PEP 768 needs CPython 3.14+)",
            ));
            self.run_debug.feedback_is_error = true;
            self.status = String::from("Attach: no Python 3.14+ processes running");
            return;
        }
        let count = targets.len();
        self.process_picker = Some(crate::widgets::process_picker::ProcessPicker::new(targets));
        self.overlays.process_picker_clear.request();
        self.status = format!("Attach: {count} Python process(es) — Esc to close, Enter to attach");
    }

    pub(super) fn close_process_picker(&mut self) {
        if self.process_picker.take().is_some() {
            self.overlays.process_picker_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(super) fn handle_process_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.process_picker.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_process_picker(),
            (KeyCode::Enter, _) => self.attach_process_picker_selection(),
            (KeyCode::Up, _) => picker.select_prev(),
            (KeyCode::Down, _) => picker.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    picker.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    picker.select_next();
                }
            }
            _ => {}
        }
    }

    /// Attach to the highlighted process and dismiss the picker: Enter's
    /// action, shared with a mouse click on a result row.
    pub(super) fn attach_process_picker_selection(&mut self) {
        let Some(picker) = self.process_picker.as_ref() else {
            return;
        };
        let target = picker.selected_target().cloned();
        self.close_process_picker();
        if let Some(target) = target {
            self.attach_to_python_target(target);
        }
    }

    pub(super) fn handle_process_picker_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.process_picker.as_mut() else {
            return;
        };
        let inside = rect_contains(picker.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    picker.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    picker.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_process_picker();
            }
            // Click a row to attach to it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = picker.row_index_at(m.row) {
                    picker.selected = idx;
                    self.attach_process_picker_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_process_picker(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let Some(picker) = self.process_picker.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::process_picker::render_process_picker(
            picker,
            area,
            frame.buffer_mut(),
            theme,
        );
    }

    /// Spawn `pdb -p <pid>` for the chosen target in a new PTY terminal,
    /// wrapping in `sudo` when the platform requires elevation (always on
    /// macOS; on Linux unless Yama `ptrace_scope` is relaxed). The PTY hosts
    /// both the interactive pdb prompt and any `sudo` password prompt — the
    /// edge a GUI debugger lacks.
    pub(super) fn attach_to_python_target(&mut self, target: crate::dap::discovery::PyTarget) {
        use crate::dap::remote_attach::{Platform, plan_attach, read_yama_ptrace_scope};
        let pid = target.pid;
        let plan = match plan_attach(
            target.version,
            target.exe,
            pid,
            Platform::current(),
            read_yama_ptrace_scope(),
        ) {
            Ok(plan) => plan,
            Err(e) => {
                self.run_debug.feedback = Some(format!("Cannot attach to PID {pid}: {e:?}"));
                self.run_debug.feedback_is_error = true;
                return;
            }
        };
        let (program, args) = plan.command_line();
        match PtyTerminal::new_running(&program, &args, self.workspace_root()) {
            Ok(term) => {
                let slot = self.active_terminal + 1;
                self.terminals.insert(slot, term);
                self.active_terminal = slot;
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.focus_pane(Pane::Terminal);
                self.run_debug.feedback = Some(format!("Attaching debugger to PID {pid}"));
                self.run_debug.feedback_is_error = false;
            }
            Err(e) => {
                self.run_debug.feedback = Some(format!("Failed to spawn terminal: {e}"));
                self.run_debug.feedback_is_error = true;
            }
        }
    }

    /// Rebuild the inline-value trailers (#135) from the current stop's
    /// already-fetched VARIABLES data, on every editor showing the stopped
    /// file (inactive split groups render too).
    pub(super) fn refresh_inline_values(&mut self) {
        use crate::dap::session::SessionPhase;
        if !self.inline_values_enabled {
            return;
        }
        let Some(session) = self.debug_sessions.focused() else {
            return;
        };
        if session.phase != SessionPhase::Stopped {
            return;
        }
        let Some((path, line)) = session.current_location.clone() else {
            return;
        };
        let locals = crate::dap::session::inline_locals(&session.scopes, &session.variables);
        for ed in self.editor.editors.iter_mut() {
            if ed.path.as_deref() == Some(path.as_path()) {
                ed.set_inline_values_from_locals(line, &locals);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                if ed.path.as_deref() == Some(path.as_path()) {
                    ed.set_inline_values_from_locals(line, &locals);
                }
            }
        }
    }

    /// Drop every inline-value trailer, in every editor of every group —
    /// called wherever the stop arrow clears (resume, step, terminate).
    pub(super) fn clear_inline_values(&mut self) {
        for ed in self.editor.editors.iter_mut() {
            ed.inline_values.clear();
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                ed.inline_values.clear();
            }
        }
    }

    pub(super) fn toggle_inline_values(&mut self) {
        self.inline_values_enabled = !self.inline_values_enabled;
        if self.inline_values_enabled {
            self.refresh_inline_values();
        } else {
            self.clear_inline_values();
        }
        self.status = format!(
            "Debug Inline Values: {}",
            if self.inline_values_enabled {
                "on"
            } else {
                "off"
            }
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_inline_values(
                self.inline_values_enabled,
            ));
        }
    }

    /// Send `path`'s current breakpoints to every debug session.
    pub(super) fn resend_breakpoints(&mut self, path: &Path) {
        if self.debug_sessions.is_empty() {
            return;
        }
        let specs = self
            .editor
            .breakpoints
            .get(path)
            .map(|lines| self.editor.source_breakpoints(path, lines))
            .unwrap_or_default();
        for session in self.debug_sessions.iter_mut() {
            session.update_breakpoints(path, &specs);
        }
    }
}
