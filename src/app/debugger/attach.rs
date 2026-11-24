//! Attaching: the process picker for a `${command:pickProcess}` attach and for Debug:
//! Attach to Python Process, its keys, mouse and drawing, and attaching to the chosen
//! process.

use super::*;

/// The current user's processes, croft's own excluded, for the attach
/// picker (#250). Empty when `ps` is unavailable.
pub(in crate::app) fn list_user_processes() -> Vec<(i64, String)> {
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

impl App {
    /// Open the process picker for a `${command:pickProcess}` attach (#250),
    /// parking the resolved config until a process is chosen.
    pub(in crate::app) fn open_attach_process_picker(
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
    pub(in crate::app) fn take_pending_attach(
        &mut self,
        pid: i64,
    ) -> Option<(crate::dap::configs::ResolvedConfig, LaunchSlot)> {
        let (mut rc, slot) = self.pending_attach.take()?;
        rc.process_id = Some(pid);
        Some((rc, slot))
    }

    /// Open the "Debug: Attach to Python Process" picker. Enumerates running
    /// CPython 3.14+ processes (those with PEP 768 `sys.remote_exec`); shows a
    /// status message and opens nothing when there are none.
    pub(in crate::app) fn open_attach_python_picker(&mut self) {
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
        self.overlays.popup_clear.request();
        self.status = format!("Attach: {count} Python process(es) — Esc to close, Enter to attach");
    }

    pub(in crate::app) fn close_process_picker(&mut self) {
        if self.process_picker.take().is_some() {
            self.overlays.popup_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(in crate::app) fn handle_process_picker_key(&mut self, key: KeyEvent) {
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
    pub(in crate::app) fn attach_process_picker_selection(&mut self) {
        let Some(picker) = self.process_picker.as_ref() else {
            return;
        };
        let target = picker.selected_target().cloned();
        self.close_process_picker();
        if let Some(target) = target {
            self.attach_to_python_target(target);
        }
    }

    pub(in crate::app) fn handle_process_picker_mouse(&mut self, m: MouseEvent) {
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

    pub(in crate::app) fn render_process_picker(&mut self, frame: &mut ratatui::Frame) {
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
    pub(in crate::app) fn attach_to_python_target(
        &mut self,
        target: crate::dap::discovery::PyTarget,
    ) {
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
}
