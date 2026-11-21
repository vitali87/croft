//! Language servers: sync, messages and progress, decorations, go-to and peek,
//! hierarchies, rename, code actions, formatting, linked editing, smart select,
//! highlights, symbol pickers, code lens, outline, breadcrumbs, sticky scroll, restart.

use super::*;

mod breadcrumbs;
mod capabilities;
mod decorations;
mod hierarchy;
mod inlay_hints;
mod messages;
mod navigation;
mod outline;
mod peek;
mod rename;
mod rust_analyzer;
mod selection;
mod semantic;
mod sync;
mod typing;
mod workspace_edits;
#[cfg(test)]
pub(super) use breadcrumbs::breadcrumb_symbol_chain;
#[cfg(test)]
pub(super) use breadcrumbs::sibling_symbols;
pub(super) use outline::syntax_kind_of;
#[cfg(test)]
pub(super) use peek::peek_excerpt;
#[cfg(test)]
pub(super) use semantic::semantic_reply_is_current;
pub(super) use sync::just_installed_covers_path;
pub(super) use sync::store_diagnostics_update;
pub(super) use sync::tool_on_path;
use sync::*;
pub(super) use typing::is_signature_help_trigger;
pub(super) use typing::snippet_completion_item;

/// What to do with one on-type formatting reply (#254). `NotOurs` leaves the
/// armed request alone — a superseded reply must not disarm the newer one
/// still in flight — while `Stale` and `Apply` both consume it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OnTypeReply {
    NotOurs,
    Stale,
    Apply,
}

/// What a breadcrumb crumb stands for (#1608).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CrumbTarget {
    Folder(PathBuf),
    File(PathBuf),
    /// An index into the outline's symbols.
    Symbol(usize),
}

/// The file the OUTLINE shows and the edit seq it was synced at.
pub(super) type OutlineKey = (PathBuf, Option<u64>);

/// Where an off-thread outline parse delivers its symbols (#1150).
pub(super) type OutlineParse = std::sync::mpsc::Receiver<Vec<crate::lsp::manager::OutlineSymbol>>;

/// A place Peek References can show: path, 0-based line, UTF-16 column.
pub(super) type PeekLocation = (PathBuf, u32, u32);

/// How long a buffer must sit unedited before its code lenses are asked
/// for (#608).
pub(super) const CODE_LENS_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Maximum pinned sticky-scroll header rows (VS Code's default is 5).
pub(super) const STICKY_SCROLL_MAX: usize = 5;

impl App {
    /// VS Code "Format Document" (Cmd+Opt+Shift+F): ask the language server to
    /// reformat the active buffer. No-op on read-only diff / sheet / image
    /// tabs. The reply is applied in [`drain_lsp_format`].
    pub(super) fn start_format_document(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        // A Prettier project formats with its own Prettier and config, as
        // VS Code's Prettier extension does; tsserver's formatter knows
        // neither (#1255).
        if let Some(bin) = crate::prettier::for_file(&path) {
            let sent = self.editor.document_text();
            self.prettier_job = Some(crate::prettier::Job::spawn(bin, path, sent));
            self.status = String::from("Formatting document with Prettier");
            return;
        }
        if self.lsp.is_none() {
            self.status = String::from("No language server for this file");
            return;
        }
        // Mirror the editor's indentation preference; servers that carry their
        // own config (rustfmt, ruff) ignore these but the fields are
        // required by the LSP request.
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let seq = self.format_target_seq(&path);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_formatting(path, tab_size, insert_spaces);
        self.format_request_id = Some(id);
        self.format_request_seq = seq;
        self.format_request_selection = false;
        self.status = String::from("Formatting document");
    }

    /// The `edit_seq` of the tab a format reply for `path` lands in (the
    /// one `apply_rename_to_open_tab` picks), or `None` when no tab holds it.
    pub(super) fn format_target_seq(&self, path: &std::path::Path) -> Option<u64> {
        let idx = self.editor.find_buffer_tab(path)?;
        Some(self.editor.editors[idx].edit_seq)
    }

    /// "Format Selection" (#254): `textDocument/rangeFormatting` over the
    /// primary selection, applied through the same reply path as
    /// whole-document formatting.
    pub(super) fn format_selection(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(sel) = self.editor.selection.filter(|s| s.has_area()) else {
            self.status = String::from("Select something to format");
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let ((sr, sc), (er, ec)) = sel.normalised();
        let start = self.editor.pos_to_utf16(sr, sc);
        let end = self.editor.pos_to_utf16(er, ec);
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let seq = self.format_target_seq(&path);
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_range_formatting(path, start, end, tab_size, insert_spaces);
        self.format_request_id = Some(id);
        self.format_request_seq = seq;
        self.format_request_selection = true;
        self.status = String::from("Formatting selection");
    }

    /// Apply a finished Prettier run to the tab it formatted, then let the
    /// save it held go ahead. A buffer edited while Prettier ran keeps the
    /// edit: the result only fits the text that was sent.
    pub fn drain_prettier_format(&mut self) -> bool {
        let Some(reply) = self
            .prettier_job
            .as_ref()
            .and_then(|job| job.poll(std::time::Instant::now()))
        else {
            return false;
        };
        let Some(job) = self.prettier_job.take() else {
            return false;
        };
        let formatted = match reply {
            Ok(formatted) => formatted,
            Err(e) => {
                // Said after the save, which would otherwise cover it with
                // "Saved".
                let saving = self.save_after_format.is_some();
                self.complete_pending_save();
                self.status = match saving {
                    true => format!("Prettier failed: {e}; saved without formatting"),
                    false => format!("Prettier failed: {e}"),
                };
                return true;
            }
        };
        let tab = self.editor.find_buffer_tab(&job.path);
        let buffer = tab
            .map(|i| &self.editor.editors[i])
            .map(|e| (e.document_text(), e.lines.clone(), e.has_final_newline()));
        self.status = match buffer {
            Some((text, lines, final_newline)) if text == job.sent => {
                if formatted == job.sent {
                    String::from("Document already formatted")
                } else {
                    // The buffer keeps no line for a final newline it already
                    // has; one Prettier adds becomes the empty last line that
                    // writes it.
                    let body = match final_newline {
                        true => formatted.strip_suffix('\n').unwrap_or(&formatted),
                        false => &formatted,
                    };
                    let edits = crate::prettier::edits_for(&lines, body);
                    self.editor.apply_rename_to_open_tab(&job.path, &edits);
                    self.editor.clamp_cursor();
                    String::from("Formatted document with Prettier")
                }
            }
            Some(_) => String::from("Prettier result dropped: the file changed while it ran"),
            None => String::from("Format failed: document not open"),
        };
        self.complete_pending_save();
        true
    }

    pub fn drain_lsp_format(&mut self) -> bool {
        self.drain_lsp_format_at(std::time::Instant::now())
    }

    /// Apply a formatter reply that has arrived, or, once a format-on-save
    /// has waited `FORMAT_ON_SAVE_TIMEOUT` with none, write its file
    /// unformatted: a busy, hung or unreachable server left it unsaved for
    /// good (#1491). Returns true when the next frame should redraw.
    pub(super) fn drain_lsp_format_at(&mut self, now: std::time::Instant) -> bool {
        let mut arrived = false;
        let mut edits: Option<Vec<crate::widgets::editor::TextSpanEdit>> = None;
        let mut unsupported = false;
        let mut path: Option<PathBuf> = None;
        let mut server: Option<String> = None;
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(result) = lsp.drain_formatting() {
                if Some(result.request_id) != self.format_request_id {
                    continue;
                }
                arrived = true;
                edits = result.edits;
                unsupported = result.unsupported;
                path = Some(result.path);
                server = result.server;
            }
        }
        if !arrived {
            let overdue = self.save_after_format.is_some()
                && self
                    .format_save_deadline
                    .is_some_and(|deadline| now >= deadline);
            if overdue {
                let secs = FORMAT_ON_SAVE_TIMEOUT.as_secs();
                self.save_without_formatting(&format!("the formatter did not answer in {secs} s"));
            }
            return overdue;
        }
        self.format_request_id = None;
        self.land_format(edits, unsupported, path, server.as_deref());
        true
    }

    /// Write a format-on-save's file now, unformatted, and drop the reply
    /// it was waiting on, which would otherwise land on the saved text.
    pub(super) fn save_without_formatting(&mut self, reason: &str) {
        let Some(path) = self.save_after_format.clone() else {
            return;
        };
        self.format_request_id = None;
        self.format_request_seq = None;
        self.format_request_selection = false;
        self.complete_pending_save();
        // A refused write (a disk conflict, a lossy encoding) or a skipped
        // one (the tab was closed meanwhile) keeps the status it set.
        if self.path_is_saved(&path) {
            self.status = format!(
                "Saved {} without formatting: {reason}",
                self.status_path(&path)
            );
        }
    }

    /// Whether `path` is open in some tab, in any split, and none of its
    /// tabs has unsaved edits. A closed file was not written, so it is not
    /// saved either.
    pub(super) fn path_is_saved(&self, path: &Path) -> bool {
        let mut tabs = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| e.path.as_deref() == Some(path))
            .peekable();
        tabs.peek().is_some() && tabs.all(|e| !e.dirty)
    }

    /// Apply a formatter's reply to the request in flight, then finish a
    /// format-on-save. Split from [`Self::drain_lsp_format`] so tests can
    /// deliver a reply without a server.
    pub(super) fn land_format(
        &mut self,
        edits: Option<Vec<crate::widgets::editor::TextSpanEdit>>,
        unsupported: bool,
        path: Option<PathBuf>,
        server: Option<&str>,
    ) {
        let selection = std::mem::take(&mut self.format_request_selection);
        let requested_at = self.format_request_seq.take();
        // bash-language-server formats only through shfmt; without it, it
        // answers every request with no edits (#1511), which is not "already
        // formatted". Another formatter's empty reply is its own answer.
        let shfmt_missing = server == Some(BASH_SERVER)
            && path.as_deref().is_some_and(is_shell_script)
            && !tool_on_path("shfmt", self.tool_search_path.as_deref());
        if unsupported {
            self.status = if selection {
                String::from("No range formatter available for this file")
            } else {
                String::from("No formatter available for this file")
            };
            self.complete_pending_save();
            return;
        }
        match (edits, path) {
            // The edits are positions in the text the server was sent; typed
            // into since, they land on the wrong lines (#1127). Drop them, as
            // VS Code does, and let a format-on-save write what is there.
            (Some(edits), Some(path))
                if !edits.is_empty() && self.format_target_seq(&path) != requested_at =>
            {
                let file = self.status_path(&path);
                let saving = self.save_after_format.is_some();
                self.complete_pending_save();
                self.status = if saving {
                    format!(
                        "Saved {file} without formatting: it changed while the formatter was running"
                    )
                } else {
                    format!(
                        "Format skipped: {file} changed while the formatter was running; format again"
                    )
                };
                return;
            }
            (Some(edits), Some(path)) if !edits.is_empty() => {
                match self.editor.apply_rename_to_open_tab(&path, &edits) {
                    Some(_) => {
                        self.editor.clamp_cursor();
                        self.status = if selection {
                            String::from("Formatted selection")
                        } else {
                            String::from("Formatted document")
                        };
                    }
                    None => self.status = String::from("Format failed: document not open"),
                }
            }
            _ if shfmt_missing => self.status = String::from(SHFMT_MISSING),
            _ => {
                self.status = if selection {
                    String::from("Selection already formatted")
                } else {
                    String::from("Document already formatted")
                }
            }
        }
        // A format-on-save write is deferred until now so the disk file gets the
        // formatted text in one go.
        self.complete_pending_save();
    }

    /// The range a Quick Fix asks the server about, as UTF-16 `(line, col)`
    /// start and end, with the diagnostics on its lines as context: the
    /// selection when there is one (VS Code sends it, so selection-scoped
    /// refactors like Extract Function are offered), else the caret point.
    #[allow(clippy::type_complexity)]
    pub(super) fn code_action_request(
        &self,
    ) -> ((u32, u32), (u32, u32), Vec<crate::lsp::manager::Diagnostic>) {
        let ((sr, sc), (er, ec)) = match self.editor.selection.filter(|s| s.has_area()) {
            Some(sel) => sel.normalised(),
            None => {
                let caret = (self.editor.cursor_row, self.editor.cursor_col);
                (caret, caret)
            }
        };
        let start = self.editor.pos_to_utf16(sr, sc);
        let end = self.editor.pos_to_utf16(er, ec);
        // Rows are plain line numbers, unaffected by the UTF-16 columns.
        let diagnostics = self.editor.diagnostics_in_line_range(start.0, end.0);
        (start, end, diagnostics)
    }

    /// VS Code "Quick Fix" (`Cmd+.`): ask the language server for the code
    /// actions available at the cursor (with the diagnostics on that line as
    /// context) and present them in a picker. No-op on read-only preview tabs.
    /// The reply is handled in [`Self::drain_lsp_code_actions`].
    pub(super) fn start_code_action(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        // A merge conflict at the cursor takes the Quick Fix over LSP code
        // actions, like VS Code's Accept Current / Incoming / Both lenses:
        // resolving markers is what the user wants there, and it needs no
        // server round-trip.
        let cursor_row = self.editor.cursor_row;
        if crate::merge::conflict_containing(self.editor.conflicts(), cursor_row).is_some() {
            self.open_merge_conflict_picker();
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let ((sr, sc), (er, ec), diagnostics) = self.code_action_request();
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_code_action(path, sr, sc, er, ec, diagnostics);
        self.code_action_request_id = Some(id);
        self.code_action_pending_resolve = false;
        self.code_action_baseline = self.edit_seqs_in_every_group();
        self.status = String::from("Finding quick fixes");
    }

    pub fn drain_lsp_code_actions(&mut self) -> bool {
        let mut arrived = false;
        let mut items: Option<Vec<crate::lsp::manager::CodeActionItem>> = None;
        let mut unsupported = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_code_action() {
                if Some(result.request_id) != self.code_action_request_id {
                    continue;
                }
                arrived = true;
                items = result.items;
                unsupported = result.unsupported;
            }
        }
        if !arrived {
            return false;
        }
        self.code_action_request_id = None;
        let was_resolve = std::mem::take(&mut self.code_action_pending_resolve);
        if unsupported {
            // No code-action server, but a seated navigator can still take
            // the diagnostic under the caret (#374).
            if self.navigator_fix_candidate().is_some() {
                self.open_code_action_picker(Vec::new());
            } else {
                self.status = String::from("No quick fixes: no code-action server for this file");
            }
            return true;
        }
        // A resolve reply carries the one action the user already chose, now with
        // its edit filled in: apply it directly instead of re-opening a menu.
        if was_resolve {
            match items.and_then(|mut v| v.pop()) {
                Some(item) => self.apply_resolved_code_action(item),
                None => self.status = String::from("Quick fix could not be resolved"),
            }
            return true;
        }
        match items {
            Some(items) if items.len() == 1 && items[0].needs_resolve => {
                // A single resolve-only action (e.g. a deferred auto-import):
                // resolve it straight away rather than popping a one-row menu.
                self.resolve_and_apply_code_action(items.into_iter().next().unwrap());
            }
            Some(items) if !items.is_empty() => self.open_code_action_picker(items),
            // Nothing from the server: the navigator entry alone, when a
            // diagnostic sits under the caret (#374).
            _ if self.navigator_fix_candidate().is_some() => {
                self.open_code_action_picker(Vec::new());
            }
            _ => self.status = String::from("No quick fixes available here"),
        }
        true
    }

    /// Present the available code actions in the shared list picker, tagging
    /// each row with its index into `pending_code_actions`. When a diagnostic
    /// sits under the caret, one more row follows the server's: Fix with
    /// Navigator (#374), at index `items.len()`, offered whether or not a
    /// navigator is seated (the action says so if none is).
    pub(super) fn open_code_action_picker(
        &mut self,
        items: Vec<crate::lsp::manager::CodeActionItem>,
    ) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let mut rows: Vec<ListRow> = items
            .iter()
            .enumerate()
            .map(|(i, item)| ListRow {
                id: i.to_string(),
                label: if item.is_preferred {
                    format!("{} (preferred)", item.title)
                } else {
                    item.title.clone()
                },
            })
            .collect();
        if self.navigator_fix_candidate().is_some() {
            rows.push(ListRow {
                id: String::from("navigator"),
                label: String::from("Fix with Navigator"),
            });
        }
        self.pending_code_actions = items;
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeAction, "Quick Fix", rows),
            "No quick fixes available here",
        );
    }

    /// Apply the chosen code action: edits first (as one undo step, like rename),
    /// then any command. A resolve-only action is sent for resolution first; its
    /// resolved edit/command arrives back through [`Self::drain_lsp_code_actions`].
    pub(super) fn apply_code_action(&mut self, item: crate::lsp::manager::CodeActionItem) {
        if item.needs_resolve {
            self.resolve_and_apply_code_action(item);
            return;
        }
        self.apply_resolved_code_action(item);
    }

    /// Apply an action that already carries its edit / command (the leaf of both
    /// the direct and the post-resolve paths). Never re-resolves, so a server
    /// that returns a still-edit-less action after resolve can't loop.
    pub(super) fn apply_resolved_code_action(&mut self, item: crate::lsp::manager::CodeActionItem) {
        let mut applied = false;
        if let Some(file) = self.stale_edit_target(&self.code_action_baseline, &item.edits) {
            self.status = format!(
                "Quick fix cancelled: {file} changed while the server was working; try it again"
            );
            return;
        }
        if !item.edits.is_empty() {
            match self.apply_rename_edits(&item.edits) {
                Ok((files, occ)) => {
                    applied = true;
                    self.status = format!(
                        "{}: changed {occ} edit(s) across {files} file(s)",
                        item.title
                    );
                }
                Err(e) => {
                    self.status = format!("Quick fix failed: {e}");
                    return;
                }
            }
        }
        if let Some(cmd) = item.command
            && let (Some(path), Some(lsp)) = (self.editor.path.clone(), self.lsp.as_mut())
        {
            lsp.execute_command(path, cmd.command, cmd.arguments);
            // A command-only action (gopls's "Browse documentation") does
            // its work in the server, and anything it has to show arrives
            // as a message; claiming a fix was applied was wrong (#1599).
            if !applied {
                self.status = format!("Ran {}", item.title);
            }
            applied = true;
        }
        if !applied {
            self.status = format!("Quick fix had no effect: {}", item.title);
        }
    }

    /// Send a resolve-only action for `codeAction/resolve`; the resolved item
    /// comes back as a single-item reply that the drain applies.
    pub(super) fn resolve_and_apply_code_action(
        &mut self,
        item: crate::lsp::manager::CodeActionItem,
    ) {
        let Some(action) = item.resolve_action else {
            self.status = format!("Quick fix cannot be resolved: {}", item.title);
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_code_action_resolve(path, item.server, action);
        self.code_action_request_id = Some(id);
        self.code_action_pending_resolve = true;
        self.code_action_baseline = self.edit_seqs_in_every_group();
        self.status = String::from("Resolving quick fix");
    }

    /// Open "Go to Symbol in Editor" (Cmd/Ctrl+Shift+O). Snapshots the active
    /// file's symbols from the outline (tree-sitter is populated synchronously
    /// each tick; LSP refines it). A leading `:` in the query switches to Go to
    /// Line. ponytail: snapshot at open — symbols are present for any synced
    /// file; if a slow LSP reply ever needs to land into an open picker, feed
    /// it live from `drain_lsp_document_symbols`.
    pub(super) fn open_go_to_symbol(&mut self) {
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

    pub(super) fn close_go_to_symbol(&mut self) {
        if self.go_to_symbol.take().is_some() {
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(super) fn handle_go_to_symbol_key(&mut self, key: KeyEvent) {
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
    pub(super) fn jump_to_go_to_symbol_selection(&mut self) {
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

    pub(super) fn handle_go_to_symbol_mouse(&mut self, m: MouseEvent) {
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

    pub(super) fn render_go_to_symbol(&mut self, frame: &mut ratatui::Frame) {
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

    /// Open "Go to Symbol in Workspace" seeded with `initial` (the text
    /// after Quick Open's `#`). The query goes to every running language
    /// server; results stream in via [`Self::drain_lsp_workspace_symbols`].
    pub(super) fn open_workspace_symbols(&mut self, initial: &str) {
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

    pub(super) fn close_workspace_symbols(&mut self) {
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
    pub(super) fn workspace_symbols_requery(&mut self) {
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
    pub(super) fn workspace_symbols_dispatch_due(&mut self) {
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

    pub(super) fn handle_workspace_symbols_key(&mut self, key: KeyEvent) {
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
    pub(super) fn jump_to_workspace_symbol_selection(&mut self) {
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

    pub(super) fn handle_workspace_symbols_mouse(&mut self, m: MouseEvent) {
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

    pub(super) fn render_workspace_symbols(&mut self, frame: &mut ratatui::Frame) {
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

    /// Ask for the active file's code lenses once its edits have settled
    /// (#608). Lenses cost a server real work (rust-analyzer resolves each
    /// one), so this waits out [`CODE_LENS_SETTLE`] rather than riding every
    /// edit the way inlay hints do.
    pub fn tick_code_lens(&mut self) {
        if !self.code_lens_enabled {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(&seq) = self.lsp_last_seen.get(&path) else {
            return;
        };
        if self.code_lens_requested.get(&path) == Some(&seq)
            || self
                .editor
                .last_edit_at
                .is_some_and(|t| t.elapsed() < CODE_LENS_SETTLE)
        {
            return;
        }
        if let Some(lsp) = self.lsp.as_ref() {
            lsp.request_code_lens(path.clone(), seq);
            self.code_lens_requested.insert(path, seq);
        }
    }

    /// Land code-lens replies on every tab showing the file (#608). A reply
    /// for an older seq is dropped; the tab keeps its lenses, each shown only
    /// while its own line is unchanged.
    pub fn drain_lsp_code_lens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_code_lens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if !self.code_lens_enabled || self.lsp_last_seen.get(&u.path) != Some(&u.seq) {
                continue;
            }
            self.for_each_tab_of(&u.path, |tab| {
                tab.code_lenses = u
                    .lenses
                    .iter()
                    .filter_map(|l| {
                        let line = l.line as usize;
                        Some(crate::code_lens::EditorLens {
                            line,
                            line_text: tab.lines.get(line)?.clone(),
                            title: l.title.clone(),
                            command: l.command.clone(),
                            arguments: l.arguments.clone(),
                            server_side: l.server_side,
                        })
                    })
                    .collect();
            });
            changed = true;
        }
        changed
    }

    /// Run the active editor's code lens at `idx` (#608).
    pub(crate) fn run_code_lens(&mut self, idx: usize) {
        let Some(lens) = self.editor.code_lenses.get(idx).cloned() else {
            return;
        };
        match crate::code_lens::action_for(&lens) {
            crate::code_lens::LensAction::ShowLocations(targets) => {
                if targets.len() == 1 {
                    let (path, line, col) = targets.into_iter().next().expect("len 1");
                    self.go_to_definition(path, line, col);
                } else {
                    self.open_location_picker(targets, "references");
                }
            }
            crate::code_lens::LensAction::RunInTerminal { label, command } => {
                self.start_project_task(crate::tasks::Task {
                    label,
                    command,
                    source: String::from("code lens"),
                    is_build: false,
                    is_default: false,
                    problem_matcher: None,
                    is_background: false,
                    vscode: None,
                });
            }
            crate::code_lens::LensAction::DebugAt(line) => {
                self.editor.cursor_row = line.min(self.editor.lines.len().saturating_sub(1));
                self.editor.cursor_col = 0;
                self.debug_test_at_cursor();
            }
            crate::code_lens::LensAction::ServerCommand { command, arguments } => {
                if let (Some(lsp), Some(path)) = (self.lsp.as_mut(), self.editor.path.clone()) {
                    lsp.execute_command(path, command, arguments);
                    self.status = format!("Ran \u{201c}{}\u{201d}", lens.title);
                }
            }
            crate::code_lens::LensAction::Unsupported(why) => {
                self.status = format!("Code lens: {why}");
            }
        }
    }

    /// The caret line's code lenses from the keyboard (Cmd+K Shift+E): one
    /// runs at once, several open a menu to pick from (#608).
    pub(super) fn run_code_lens_at_cursor(&mut self) {
        let lenses = self.editor.lenses_on_line(self.editor.cursor_row);
        match lenses.as_slice() {
            [] => self.status = String::from("No code lens on this line"),
            [one] => self.run_code_lens(*one),
            many => {
                let items: Vec<(String, MenuAction)> = many
                    .iter()
                    .map(|&i| {
                        (
                            self.editor.code_lenses[i].title.clone(),
                            MenuAction::RunCodeLens(i),
                        )
                    })
                    .collect();
                let origin = self.editor.cursor_screen_pos().unwrap_or((
                    self.editor.last_full_area.x + 1,
                    self.editor.last_full_area.y + 1,
                ));
                let root = self.tree.root.clone();
                self.context_menu = Some(ContextMenu::flat(origin, items, root));
            }
        }
    }

    /// Editor: Toggle CodeLens (#608).
    pub(super) fn toggle_code_lens(&mut self) {
        self.code_lens_enabled = !self.code_lens_enabled;
        self.code_lens_requested.clear();
        if !self.code_lens_enabled {
            for tab in self.editor.editors.iter_mut() {
                tab.code_lenses.clear();
            }
            for group in self.editor_layout.inactive_groups_mut() {
                for tab in group.editors.iter_mut() {
                    tab.code_lenses.clear();
                }
            }
        }
        self.status = format!(
            "CodeLens: {}",
            if self.code_lens_enabled { "on" } else { "off" }
        );
    }
}

/// The bundled shell language server, the one that needs shfmt and
/// shellcheck (#1511).
pub(super) const BASH_SERVER: &str = "bash-language-server";

/// What Format Document says for a shell script when shfmt is missing (#1511).
pub(super) const SHFMT_MISSING: &str =
    "Not formatted: bash-language-server formats shell scripts with shfmt, which is not on PATH";

/// The one-time notice when a shell script opens and shellcheck is missing
/// (#1511).
pub(super) const SHELLCHECK_MISSING: &str = "Shell scripts are not linted: bash-language-server lints with shellcheck, which is not on PATH";

/// What the restart commands say while an update build has the language
/// servers stopped (#1621): the build's end starts them again.
pub(super) const LSP_PAUSED_FOR_BUILD: &str =
    "Language servers are stopped while croft builds its update; they restart when it finishes";

/// The largest buffer whose syntax outline is parsed on the UI thread
/// (#1150). A from-scratch parse costs about a quarter of a millisecond per
/// kilobyte, so this keeps it within a frame; larger buffers go to
/// [`App::start_outline_parse`].
pub(super) const OUTLINE_INLINE_MAX_BYTES: usize = 64 * 1024;

/// Divider drawn between the diagnostic block and the language hover content,
/// mirroring the horizontal rule VS Code renders between hover sections.
pub(super) const HOVER_DIVIDER: &str = "────────";

/// What Show or Focus Hover says when the caret has nothing to describe
/// (#1518): a keyboard command that does nothing must say why.
pub(super) const NO_HOVER_AT_CARET: &str = "No hover information at the caret";

/// Render the diagnostics under the hovered point into a single labelled text
/// block, errors first (so the most severe message leads), each line prefixed
/// with its severity. `None` when there are no diagnostics. The severity word
/// answers the user's "what type of error is it" directly, since a TUI can't
/// show VS Code's coloured marker icon.
pub(super) fn format_diagnostics(
    diags: &[(crate::lsp::manager::DiagnosticSeverity, String)],
) -> Option<String> {
    use crate::lsp::manager::DiagnosticSeverity;
    if diags.is_empty() {
        return None;
    }
    let rank = |s: &DiagnosticSeverity| match s {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    };
    let label = |s: &DiagnosticSeverity| match s {
        DiagnosticSeverity::Error => "Error",
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Information => "Info",
        DiagnosticSeverity::Hint => "Hint",
    };
    let mut ordered: Vec<&(DiagnosticSeverity, String)> = diags.iter().collect();
    ordered.sort_by_key(|(s, _)| rank(s));
    Some(
        ordered
            .iter()
            .map(|(s, m)| format!("{}: {}", label(s), m))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Stack the diagnostic block above the language hover content, divided by a
/// rule, matching VS Code (PR microsoft/vscode#166560 put marker hovers on
/// top). Either side may be absent; `None` when both are.
pub(super) fn compose_hover(diagnostic: Option<&str>, hover: Option<&str>) -> Option<String> {
    match (diagnostic, hover) {
        (Some(d), Some(h)) => Some(format!("{d}\n{HOVER_DIVIDER}\n{h}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(h)) => Some(h.to_string()),
        (None, None) => None,
    }
}
