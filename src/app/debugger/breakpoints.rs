//! Every kind of breakpoint: line breakpoints with conditions, hit counts and logpoints,
//! function and data breakpoints, break on raised exceptions, and sending a file's
//! breakpoints to the running sessions.

use super::*;

impl App {
    /// The editor's breakpoints in launch shape, shared by every session
    /// starter (zero-config and launch.json alike).
    pub(in crate::app) fn collect_editor_breakpoints(
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
    pub(in crate::app) fn debug_toggle_breakpoint_line(&mut self, line: usize) {
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
    pub(in crate::app) fn debug_edit_condition_line(&mut self, line: usize) {
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
    pub(in crate::app) fn commit_breakpoint_condition(
        &mut self,
        path: PathBuf,
        line: usize,
        expr: &str,
    ) {
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
    pub(in crate::app) fn open_debug_prompt(
        &mut self,
        label: String,
        buffer: String,
        kind: PromptKind,
    ) {
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
    pub(in crate::app) fn commit_hit_condition(&mut self, path: PathBuf, line: usize, hits: &str) {
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

    pub(in crate::app) fn commit_function_breakpoint(&mut self, name: String) {
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

    pub(in crate::app) fn push_function_breakpoints(&mut self) {
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

    pub(in crate::app) fn commit_data_breakpoint(&mut self, name: &str) {
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
    pub(in crate::app) fn debug_edit_logpoint_line(&mut self, line: usize) {
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
    pub(in crate::app) fn commit_logpoint(&mut self, path: PathBuf, line: usize, message: &str) {
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

    /// Send `path`'s current breakpoints to every debug session.
    pub(in crate::app) fn resend_breakpoints(&mut self, path: &Path) {
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
