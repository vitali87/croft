//! Quick Fix (Cmd+.): the range and diagnostics it asks about, the picker of actions, and
//! applying the chosen one's edit and command, resolving it first when the server sent it
//! bare.

use super::*;

impl App {
    /// The range a Quick Fix asks the server about, as UTF-16 `(line, col)`
    /// start and end, with the diagnostics on its lines as context: the
    /// selection when there is one (VS Code sends it, so selection-scoped
    /// refactors like Extract Function are offered), else the caret point.
    #[allow(clippy::type_complexity)]
    pub(in crate::app) fn code_action_request(
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
    pub(in crate::app) fn start_code_action(&mut self) {
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
    pub(in crate::app) fn open_code_action_picker(
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
    pub(in crate::app) fn apply_code_action(&mut self, item: crate::lsp::manager::CodeActionItem) {
        if item.needs_resolve {
            self.resolve_and_apply_code_action(item);
            return;
        }
        self.apply_resolved_code_action(item);
    }

    /// Apply an action that already carries its edit / command (the leaf of both
    /// the direct and the post-resolve paths). Never re-resolves, so a server
    /// that returns a still-edit-less action after resolve can't loop.
    pub(in crate::app) fn apply_resolved_code_action(
        &mut self,
        item: crate::lsp::manager::CodeActionItem,
    ) {
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
    pub(in crate::app) fn resolve_and_apply_code_action(
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
}
