//! Go to Symbol in Workspace (`#` in Quick Open): the debounced query to every running
//! server, its replies, the picker's keys, mouse and drawing, and jumping to the chosen
//! symbol.

use super::*;

impl App {
    /// Open "Go to Symbol in Workspace" seeded with `initial` (the text
    /// after Quick Open's `#`). The query goes to every running language
    /// server; results stream in via [`Self::drain_lsp_workspace_symbols`].
    pub(in crate::app) fn open_workspace_symbols(&mut self, initial: &str) {
        if self.workspace_symbols.is_some() {
            return;
        }
        self.workspace_symbols = Some(
            crate::widgets::workspace_symbols::WorkspaceSymbolPicker::new(
                self.tree.root.clone(),
                initial,
            ),
        );
        self.overlays.command_palette_clear.request();
        self.workspace_symbols_requery();
        self.status = String::from("Go to Symbol in Workspace: type to search — Esc to close");
    }

    pub(in crate::app) fn close_workspace_symbols(&mut self) {
        if self.workspace_symbols.take().is_some() {
            self.ws_symbols_request_id = None;
            self.ws_symbols_pending = None;
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Note a query change and arm the debounce; the actual LSP request
    /// fires from [`Self::workspace_symbols_dispatch_due`] once the query
    /// has been still for the window. Firing per keystroke serialized a
    /// round trip per character behind the per-client lock, starving every
    /// other LSP feature on that server for the whole burst.
    pub(in crate::app) fn workspace_symbols_requery(&mut self) {
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let query = picker.query.trim().to_string();
        if query.is_empty() {
            picker.set_results(Vec::new());
            // A stale notice from the previous reply reads as an error;
            // an empty query should show the type-to-search hint.
            picker.unsupported = false;
            self.ws_symbols_request_id = None;
            self.ws_symbols_pending = None;
            return;
        }
        picker.loading = true;
        // Invalidate any in-flight request: its reply answers a query the
        // user has typed past and would render stale results until the
        // debounced request lands.
        self.ws_symbols_request_id = None;
        self.ws_symbols_pending = Some(std::time::Instant::now());
    }

    /// Debounced dispatch for [`Self::workspace_symbols_requery`]: called
    /// each tick from the drain loop, sends the newest query once it has
    /// been still for the window. Only the newest reply applies via
    /// `ws_symbols_request_id`.
    pub(in crate::app) fn workspace_symbols_dispatch_due(&mut self) {
        const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
        let Some(armed) = self.ws_symbols_pending else {
            return;
        };
        if armed.elapsed() < DEBOUNCE {
            return;
        }
        self.ws_symbols_pending = None;
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let query = picker.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(lsp) = self.lsp.as_mut() else {
            if let Some(p) = self.workspace_symbols.as_mut() {
                p.loading = false;
                p.unsupported = true;
            }
            return;
        };
        self.ws_symbols_request_id = Some(lsp.request_workspace_symbols(query));
    }

    /// Install a `workspace/symbol` reply into the open picker. Only the
    /// newest request's reply applies; anything else is a stale answer to a
    /// query the user has already typed past. Returns true when applied.
    pub fn apply_workspace_symbols(
        &mut self,
        request_id: u64,
        symbols: Vec<crate::lsp::manager::WorkspaceSymbolItem>,
        unsupported: bool,
    ) -> bool {
        if self.ws_symbols_request_id != Some(request_id) {
            return false;
        }
        self.ws_symbols_request_id = None;
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return false;
        };
        picker.unsupported = unsupported;
        picker.set_results(symbols);
        true
    }

    pub fn drain_lsp_workspace_symbols(&mut self) -> bool {
        let mut results = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_workspace_symbols() {
                results.push(r);
            }
        }
        let mut changed = false;
        for r in results {
            changed |= self.apply_workspace_symbols(r.request_id, r.symbols, r.unsupported);
        }
        changed
    }

    pub(in crate::app) fn handle_workspace_symbols_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_workspace_symbols(),
            // Alt+Enter: go, then view that symbol in a tab of its own (#369).
            (KeyCode::Enter, m) if m.contains(KeyModifiers::ALT) => {
                let target = picker.selected_item().map(|i| i.path.clone());
                self.jump_to_workspace_symbol_selection();
                // The server's path may be spelled differently from the
                // tab's (a symlink, `..`): a landed jump is the same file.
                let landed = match (target, self.editor.path.as_ref()) {
                    (Some(t), Some(p)) => {
                        &t == p
                            || std::fs::canonicalize(&t)
                                .is_ok_and(|t| std::fs::canonicalize(p).is_ok_and(|p| p == t))
                    }
                    _ => false,
                };
                if landed {
                    self.open_symbol_tab();
                }
            }
            (KeyCode::Enter, _) => self.jump_to_workspace_symbol_selection(),
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
            (KeyCode::Left, _) => picker.move_cursor_left(),
            (KeyCode::Right, _) => picker.move_cursor_right(),
            (KeyCode::Home, _) => picker.move_cursor_home(),
            (KeyCode::End, _) => picker.move_cursor_end(),
            (KeyCode::Backspace, _) => {
                picker.pop_char();
                self.workspace_symbols_requery();
            }
            (KeyCode::Delete, _) => {
                picker.delete_char();
                self.workspace_symbols_requery();
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                picker.push_char(c);
                self.workspace_symbols_requery();
            }
            _ => {}
        }
    }

    /// Jump to the highlighted workspace symbol and dismiss the picker:
    /// Enter's action, shared with a mouse click on a result row.
    pub(in crate::app) fn jump_to_workspace_symbol_selection(&mut self) {
        let Some(picker) = self.workspace_symbols.as_ref() else {
            return;
        };
        let target = picker
            .selected_item()
            .map(|i| (i.path.clone(), i.line, i.character));
        self.close_workspace_symbols();
        if let Some((path, line, character)) = target {
            self.go_to_definition(path, line, character);
        }
    }

    pub(in crate::app) fn handle_workspace_symbols_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.workspace_symbols.as_mut() else {
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
                self.close_workspace_symbols();
            }
            // Click a row to jump to it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = picker.row_index_at(m.row) {
                    picker.selected = idx;
                    self.jump_to_workspace_symbol_selection();
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn render_workspace_symbols(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let center = self.quick_input_position == QuickInputPosition::Center;
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::workspace_symbols::render_workspace_symbols(
            picker,
            area,
            frame.buffer_mut(),
            theme,
            center,
        );
    }
}
