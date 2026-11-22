//! Call and type hierarchies: asking for the callers, callees, supertypes or subtypes at
//! the caret, and the picker their answer opens.

use super::*;

impl App {
    /// Apply a call-hierarchy reply: a picker of callers/callees labelled
    /// `name — path:line`, jumping on select. Invoking the command again at a
    /// picked site walks the next level of the hierarchy.
    pub fn drain_lsp_call_hierarchy(&mut self) -> bool {
        let mut result = None;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_call_hierarchy() {
                if Some(r.request_id) == self.call_hierarchy_request_id {
                    result = Some(r);
                }
            }
        }
        let Some(result) = result else {
            return false;
        };
        let noun = result.kind.noun();
        if result.unsupported {
            let what = match result.kind {
                crate::lsp::manager::HierarchyKind::Supertypes
                | crate::lsp::manager::HierarchyKind::Subtypes => "Type hierarchy",
                _ => "Call hierarchy",
            };
            self.status = format!("{what}: not supported by this file's language server");
            return true;
        }
        if result.sites.is_empty() {
            self.status = format!("No {noun}");
            return true;
        }
        let root = self.tree.root.clone();
        let items: Vec<(String, MenuAction)> = result
            .sites
            .into_iter()
            .map(|s| {
                let label = format!(
                    "{} — {}:{}",
                    s.name,
                    s.path.strip_prefix(&root).unwrap_or(&s.path).display(),
                    s.line + 1
                );
                (
                    label,
                    MenuAction::GoToLocation {
                        path: s.path,
                        line: s.line,
                        col: s.character,
                    },
                )
            })
            .collect();
        self.present_call_hierarchy_menu(items, noun);
        true
    }

    /// Show a call-hierarchy reply as a picker at the caret — unless another
    /// menu is already open: rust-analyzer can answer seconds later, and a
    /// late reply must not replace what the user is looking at (or yank
    /// focus to the editor under them).
    pub(in crate::app) fn present_call_hierarchy_menu(
        &mut self,
        items: Vec<(String, MenuAction)>,
        noun: &str,
    ) {
        if self.context_menu.is_some() {
            self.status = String::from("Call hierarchy ready, but another menu is open");
            return;
        }
        self.status = format!("{} {noun}", items.len());
        let origin = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        // A late server reply is not the user asking to leave the sidebar.
        // The `context_menu` guard cannot cover this: it is set on the NEXT
        // line, so auto-hide would already have fired.
        self.without_auto_hide(|app| app.focus_pane(Pane::Editor));
        let root = self.tree.root.clone();
        self.context_menu = Some(ContextMenu::flat(origin, items, root));
    }

    /// Request the callers (`incoming`) or callees of the symbol at the
    /// cursor. Cmd+K H / Cmd+K Shift+H and the right-click menu rows.
    pub(in crate::app) fn request_call_hierarchy_at_cursor(&mut self, incoming: bool) {
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("Call hierarchy: no language server for this file");
            return;
        };
        let id = lsp.request_call_hierarchy(path, line, character, incoming);
        self.call_hierarchy_request_id = Some(id);
    }

    /// Request the supertypes or subtypes of the type at the cursor (#613).
    /// Cmd+K Shift+U / Cmd+K Shift+D and the right-click menu rows.
    pub(in crate::app) fn request_type_hierarchy_at_cursor(&mut self, supertypes: bool) {
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("Type hierarchy: no language server for this file");
            return;
        };
        let id = lsp.request_type_hierarchy(path, line, character, supertypes);
        self.call_hierarchy_request_id = Some(id);
    }
}
