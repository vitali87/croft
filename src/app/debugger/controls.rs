//! The debug toolbar's controls: continue, pause, step over, into and out, Step Into
//! Target, Run to Cursor, restart and stop.

use super::*;

impl App {
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

    /// A click on the Run and Debug panel's debug toolbar (#1545): the same
    /// action as the button's key.
    pub(in crate::app) fn run_debug_control(
        &mut self,
        control: crate::widgets::run_debug::DebugControl,
    ) {
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
    pub(in crate::app) fn debug_continue(&mut self) {
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
    pub(in crate::app) fn debug_step_into_target(&mut self) {
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
    pub(in crate::app) fn offer_step_in_targets(&mut self, targets: Vec<(i64, String)>) {
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
    pub(in crate::app) fn debug_step_into(&mut self, target: Option<i64>) {
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

    /// Shift+F5 and Debug: Stop Debugging. The user ending the run is when a
    /// `postDebugTask` runs (#250); the internal `debug_stop` a relaunch uses
    /// to replace the set is not, so it lives here rather than in there.
    pub(in crate::app) fn debug_stop_by_user(&mut self) {
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
}
