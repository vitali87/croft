//! Go to Symbol in Editor (Cmd/Ctrl+Shift+O): opening it on the active file's outline,
//! its keys, mouse and drawing, and jumping to the chosen symbol.

use super::*;

impl App {
    /// Open "Go to Symbol in Editor" (Cmd/Ctrl+Shift+O). Snapshots the active
    /// file's symbols from the outline (tree-sitter is populated synchronously
    /// each tick; LSP refines it). A leading `:` in the query switches to Go to
    /// Line. ponytail: snapshot at open — symbols are present for any synced
    /// file; if a slow LSP reply ever needs to land into an open picker, feed
    /// it live from `drain_lsp_document_symbols`.
    pub(in crate::app) fn open_go_to_symbol(&mut self) {
        if self.go_to_symbol.is_some() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Go to Symbol: no file open");
            return;
        };
        let symbols = self.outline.symbols().to_vec();
        let count = symbols.len();
        self.go_to_symbol = Some(crate::widgets::symbol_picker::SymbolPicker::new(
            path, symbols,
        ));
        self.overlays.command_palette_clear.request();
        self.status = format!("Go to Symbol: {count} symbols — Esc to close");
    }

    pub(in crate::app) fn close_go_to_symbol(&mut self) {
        if self.go_to_symbol.take().is_some() {
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(in crate::app) fn handle_go_to_symbol_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.go_to_symbol.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_go_to_symbol(),
            (KeyCode::Enter, _) => {
                if picker.is_line_mode() {
                    // Resolve the target before closing (close drops the picker).
                    let target = picker.line_target().map(|n| (n as u32 - 1, 0));
                    let path = picker.path.clone();
                    self.close_go_to_symbol();
                    if let Some((line, col)) = target {
                        self.go_to_definition(path, line, col);
                    }
                } else {
                    self.jump_to_go_to_symbol_selection();
                }
            }
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
            (KeyCode::Backspace, _) => picker.pop_char(),
            (KeyCode::Delete, _) => picker.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                picker.push_char(c);
            }
            _ => {}
        }
    }

    /// Jump to the highlighted symbol and dismiss the picker: Enter's action
    /// outside line mode, shared with a mouse click on a result row (a click
    /// names a concrete row, so it always means the symbol, never a `:N`
    /// line query).
    pub(in crate::app) fn jump_to_go_to_symbol_selection(&mut self) {
        let Some(picker) = self.go_to_symbol.as_ref() else {
            return;
        };
        // Resolve the target before closing (close drops the picker).
        let target = picker.selected_target();
        let path = picker.path.clone();
        self.close_go_to_symbol();
        if let Some((line, col)) = target {
            self.go_to_definition(path, line, col);
        }
    }

    pub(in crate::app) fn handle_go_to_symbol_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.go_to_symbol.as_mut() else {
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
                self.close_go_to_symbol();
            }
            // Click a row to jump to it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = picker.row_index_at(m.row) {
                    picker.selected = idx;
                    self.jump_to_go_to_symbol_selection();
                }
            }
            _ => {}
        }
    }

    pub(in crate::app) fn render_go_to_symbol(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let center = self.quick_input_position == QuickInputPosition::Center;
        let Some(picker) = self.go_to_symbol.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::symbol_picker::render_symbol_picker(
            picker,
            area,
            frame.buffer_mut(),
            theme,
            center,
        );
    }
}
