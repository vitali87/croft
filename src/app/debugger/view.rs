//! The Run and Debug view: its keys, the Run or Debug button, the paused-state tree (call
//! stack, variables), clicks and the row menu with Copy Value and Copy Call Stack,
//! revealing the view, and how a launch failure shows.

use super::*;

/// Append variable rows to the debug tree, recursing into any expanded
/// (`variablesReference` in `expanded`) container whose children are loaded.
/// Pulled out as a free function so it can borrow the session immutably while
/// the caller mutates the row vector.
pub(in crate::app) fn push_variable_rows(
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

impl App {
    pub(in crate::app) fn handle_run_debug_key(&mut self, key: KeyEvent) {
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

    pub(in crate::app) fn refresh_run_debug(&mut self) {
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

    /// Rebuild the Run & Debug panel's paused-state tree from the session, or
    /// deactivate it when not paused. Cheap; called after each `poll_dap`.
    pub(in crate::app) fn refresh_debug_panel(&mut self) {
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
    pub(in crate::app) fn build_debug_view(
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

    /// Act on a click of debug-panel row `idx`: select a call-stack frame (load
    /// its scopes/variables) or toggle an expandable variable open/closed.
    pub(in crate::app) fn debug_panel_click(&mut self, idx: usize) {
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
    pub(in crate::app) fn debug_row_menu(&self, idx: usize) -> Vec<(String, MenuAction)> {
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
    pub(in crate::app) fn copy_debug_value(&mut self, expression: Option<String>, value: String) {
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
    pub(in crate::app) fn copy_call_stack(&mut self) {
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

    /// The Run/Debug panel button action: debug compiled source (build + lldb),
    /// or run scripts/executables in a terminal — matching the button's verb.
    pub(in crate::app) fn run_debug_button_action(&mut self) {
        if self.run_debug.run_is_debug {
            self.debug_start_or_continue();
        } else {
            self.run_active_file();
        }
    }

    /// Open and focus the Run and Debug view so the call stack, variables,
    /// console, and REPL are visible the instant a session starts (VS Code
    /// reveals its debug view on launch the same way).
    pub(in crate::app) fn reveal_debug_view(&mut self) {
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
    pub(in crate::app) fn debug_error(&mut self, msg: String) {
        // A launch that never happened leaves no session to clean up after,
        // so any breakpoint croft armed for it comes out here (#373) —
        // otherwise it survives in the editor as if the user had set it.
        self.disarm_failure_breakpoint();
        self.run_debug.feedback = Some(msg.clone());
        self.run_debug.feedback_is_error = true;
        self.status = msg;
    }
}
