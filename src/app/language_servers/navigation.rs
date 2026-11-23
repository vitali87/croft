//! Go to Definition, Declaration, Type Definition, Implementations and References: the
//! request at the caret or under a Cmd-click, jumping to the answer with navigation
//! history, and the picker when there are several.

use super::*;

impl App {
    pub(in crate::app) fn go_to_definition(&mut self, path: PathBuf, line: u32, col: u32) {
        self.try_go_to_definition(path, line, col);
    }

    /// `go_to_definition`, returning whether the file opened.
    pub(in crate::app) fn try_go_to_definition(
        &mut self,
        path: PathBuf,
        line: u32,
        col: u32,
    ) -> bool {
        let pinned = self.keep_preview_open_for_navigation_to(&path);
        if let Some(from) = self.editor.path.clone() {
            self.nav.record(NavLoc {
                path: from,
                row: self.editor.cursor_row,
                col: self.editor.cursor_col,
            });
        }
        // `col` is the server's UTF-16 column, not a character column.
        match self.open_at_utf16(&path, line, col) {
            Ok(()) => {
                self.status = format!("Jumped to {} line {}", self.status_path(&path), line + 1);
                true
            }
            Err(e) => {
                self.restore_preview_after_failed_navigation(pinned);
                self.status = format!("Go to definition failed: {e}");
                false
            }
        }
    }

    pub fn drain_lsp_definition(&mut self) -> bool {
        let mut target = None;
        let mut peek = None;
        let mut empty_reply = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            let mut peek_replied = false;
            let mut replied = false;
            while let Some(result) = lsp.drain_definition() {
                if Some(result.request_id) == self.peek_definition_request_id {
                    peek_replied = true;
                    peek = result.target;
                    continue;
                }
                if Some(result.request_id) != self.definition_request_id {
                    continue;
                }
                replied = true;
                target = result.target;
            }
            if peek_replied && peek.is_none() {
                // The reply arrived empty: without this the "Peeking
                // definition" status hung forever with no popup.
                self.peek_definition_request_id = None;
                self.status = String::from("No definition found");
            }
            if replied {
                self.definition_request_id = None;
                // An empty F12 reply said nothing (#1302): a slow server, a
                // missing one and "nothing here" all looked alike.
                if target.is_none() {
                    self.status = String::from("No definition found");
                    empty_reply = true;
                }
            }
        }
        let mut changed = empty_reply;
        if let Some((path, line, col)) = peek {
            self.peek_definition_request_id = None;
            self.open_peek_popup(path, line, col);
            changed = true;
        }
        if let Some((path, line, col)) = target {
            self.go_to_definition(path, line, col);
            changed = true;
        }
        changed
    }

    pub fn drain_lsp_declaration(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let Some(reply) = take_reply(
            &mut self.declaration_request_id,
            || lsp.drain_declaration(),
            |r| r.request_id,
        ) else {
            return false;
        };
        self.finish_jump(
            reply.target,
            reply.unsupported,
            "Go to Declaration",
            "declaration",
        );
        true
    }

    pub fn drain_lsp_type_definition(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let Some(reply) = take_reply(
            &mut self.type_definition_request_id,
            || lsp.drain_type_definition(),
            |r| r.request_id,
        ) else {
            return false;
        };
        self.finish_jump(
            reply.target,
            reply.unsupported,
            "Go to Type Definition",
            "type definition",
        );
        true
    }

    /// Act on the reply to a go-to request: jump to its target the way Go to
    /// Definition does (open the file, move the caret, record nav history),
    /// or say so rather than silently doing nothing, both when the language
    /// server has no `feature` provider (`unsupported`, e.g. TypeScript's
    /// vtsls has no declarations) and when it found no `noun` at this
    /// position.
    pub(in crate::app) fn finish_jump(
        &mut self,
        target: Option<(PathBuf, u32, u32)>,
        unsupported: bool,
        feature: &str,
        noun: &str,
    ) {
        match target {
            Some((path, line, col)) => self.go_to_definition(path, line, col),
            None if unsupported => {
                self.status = format!("{feature}: not supported by this file's language server");
            }
            None => self.status = format!("No {noun} found"),
        }
    }

    pub fn drain_lsp_implementation(&mut self) -> bool {
        let mut targets: Vec<(PathBuf, u32, u32)> = Vec::new();
        let mut unsupported = false;
        let mut responded = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_implementation() {
                if Some(result.request_id) != self.implementation_request_id {
                    continue;
                }
                targets = result.targets;
                unsupported = result.unsupported;
                responded = true;
            }
        }
        if !responded {
            return false;
        }
        if unsupported {
            self.status =
                String::from("Go to Implementations: not supported by this file's language server");
            return true;
        }
        match targets.len() {
            0 => self.status = String::from("No implementations found"),
            // One implementor: behave exactly like Go to Definition.
            1 => {
                let (path, line, col) = targets.into_iter().next().expect("len == 1");
                self.go_to_definition(path, line, col);
            }
            // Many implementors: offer a picker rather than silently choosing
            // the first. This 1:many case is the whole point of the feature.
            _ => self.open_location_picker(targets, "implementations"),
        }
        true
    }

    pub fn drain_lsp_references(&mut self) -> bool {
        let mut targets: Vec<(PathBuf, u32, u32)> = Vec::new();
        let mut unsupported = false;
        let mut responded = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_references() {
                if Some(result.request_id) != self.references_request_id {
                    continue;
                }
                targets = result.targets;
                unsupported = result.unsupported;
                responded = true;
            }
        }
        if !responded {
            return false;
        }
        let peek = std::mem::take(&mut self.references_want_peek);
        if unsupported {
            self.status =
                String::from("Go to References: not supported by this file's language server");
            return true;
        }
        if peek && !targets.is_empty() {
            self.peek_refs = Some((targets, 0));
            self.show_peeked_reference();
            return true;
        }
        match targets.len() {
            0 => self.status = String::from("No references found"),
            // A symbol used exactly once: jump straight there like Go to
            // Definition rather than pop a one-row picker.
            1 => {
                let (path, line, col) = targets.into_iter().next().expect("len == 1");
                self.go_to_definition(path, line, col);
            }
            // The common case: a symbol used in many places. Offer every use as
            // a picker, exactly like Go to Implementations.
            _ => self.open_location_picker(targets, "references"),
        }
        true
    }

    /// Build a multi-location picker as a `ContextMenu` anchored at the editor
    /// cursor: one row per target, labelled `relative/path:line`, each carrying a
    /// `GoToLocation` that jumps on select. Reuses the context menu's existing
    /// keyboard (arrows + Enter) and mouse handling. Shared by Go to
    /// Implementations and Go to References; `noun` is the status-line plural
    /// ("implementations" / "references").
    pub(in crate::app) fn open_location_picker(
        &mut self,
        targets: Vec<(PathBuf, u32, u32)>,
        noun: &str,
    ) {
        // Same guard as present_call_hierarchy_menu: the LSP can answer
        // seconds later, and the reply must not replace a menu the user
        // opened while waiting (or yank focus to the editor).
        if self.context_menu.is_some() {
            self.status = String::from("Locations ready, but another menu is open");
            return;
        }
        let root = self.tree.root.clone();
        let current = self.location_under_caret(&targets);
        let items: Vec<(String, MenuAction)> = targets
            .into_iter()
            .map(|(path, line, col)| {
                let label = format!(
                    "{}:{}",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    line + 1
                );
                (label, MenuAction::GoToLocation { path, line, col })
            })
            .collect();
        self.status = format!("{} {noun}", items.len());
        let origin = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        // As in `present_call_hierarchy_menu`: the reply arrives whenever the
        // server is ready, and the `context_menu` guard is armed a line too
        // late to protect the sidebar from this focus move.
        self.without_auto_hide(|app| app.focus_pane(Pane::Editor));
        let mut menu = ContextMenu::flat(origin, items, root);
        // Start on the reference the caret is on, as VS Code's peek does.
        menu.selected = current;
        self.context_menu = Some(menu);
    }

    /// Which of `targets` the caret is on: the one in this file, on the
    /// caret's line, starting at or before the caret (the nearest such), else
    /// the first one on that line. 0 when none is (a reference list opened
    /// from elsewhere starts at the top).
    pub(in crate::app) fn location_under_caret(&self, targets: &[(PathBuf, u32, u32)]) -> usize {
        let Some(path) = self.editor.path.as_deref() else {
            return 0;
        };
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let on_line = || {
            targets
                .iter()
                .enumerate()
                .filter(move |(_, (p, line, _))| p == path && *line as usize == row)
        };
        on_line()
            .filter(|(_, (_, _, c))| *c as usize <= col)
            .max_by_key(|(_, (_, _, c))| *c)
            .or_else(|| on_line().next())
            .map_or(0, |(i, _)| i)
    }
}

impl App {
    pub(in crate::app) fn trigger_definition_at(&mut self, col: u16, row: u16) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some((line, c)) = self.editor.buffer_pos_at(col, row) else {
            return;
        };
        self.editor.cursor_row = line;
        self.editor.cursor_col = c;
        // A server-resolved document link outranks Go to Definition at
        // this spot (#254): URLs in comments have no definition at all,
        // and an import specifier's link and definition land in the same
        // file anyway — VS Code follows the link too.
        if let Some(target) = self.editor.document_link_at(line, c).map(str::to_string) {
            self.open_document_link(&target);
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let (uline, uchar) = self.editor.pos_to_utf16(line, c);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_definition(path, uline, uchar);
        self.definition_request_id = Some(id);
    }

    /// Request the LSP definition of the symbol at the current cursor (buffer)
    /// position. Used by the editor right-click "Go to Definition" item, where
    /// the cursor has already been moved to the click point.
    pub(in crate::app) fn request_definition_at_cursor(&mut self) {
        if let Some(id) = self.request_at_cursor(crate::lsp::LspManager::request_definition) {
            self.definition_request_id = Some(id);
        }
    }

    /// Send `request` for the symbol at the editor caret, as the UTF-16
    /// position LSP speaks, and return the id its reply will echo; `None`
    /// when the buffer has no file or no language server runs.
    pub(in crate::app) fn request_at_cursor(
        &mut self,
        request: fn(&mut crate::lsp::LspManager, PathBuf, u32, u32) -> u64,
    ) -> Option<u64> {
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let path = self.editor.path.clone()?;
        let lsp = self.lsp.as_mut()?;
        Some(request(lsp, path, line, character))
    }

    /// Request the LSP declaration of the symbol at the current cursor (buffer)
    /// position. Used by the editor right-click "Go to Declaration" item and the
    /// `⇧F12` keybinding. Declaration and definition coincide for languages with
    /// no separate declaration site (e.g. Python); they diverge for C/C++ (header
    /// prototype) and TypeScript (`.d.ts`).
    pub(in crate::app) fn request_declaration_at_cursor(&mut self) {
        if let Some(id) = self.request_at_cursor(crate::lsp::LspManager::request_declaration) {
            self.declaration_request_id = Some(id);
        }
    }

    /// Request the LSP type definition of the symbol at the current cursor
    /// (buffer) position. Used by the editor right-click "Go to Type Definition"
    /// item and the `⌃F12` keybinding. Jumps to where the *type* of the
    /// expression is defined, as opposed to the symbol itself (definition).
    pub(in crate::app) fn request_type_definition_at_cursor(&mut self) {
        if let Some(id) = self.request_at_cursor(crate::lsp::LspManager::request_type_definition) {
            self.type_definition_request_id = Some(id);
        }
    }

    /// Request the LSP implementations of the abstraction at the current cursor
    /// (buffer) position. Used by the editor right-click "Go to Implementations"
    /// item and the `⌘F12` keybinding. Meaningful on traits / interfaces /
    /// abstract methods; the server may return many, and croft jumps to the first.
    pub(in crate::app) fn request_implementation_at_cursor(&mut self) {
        if let Some(id) = self.request_at_cursor(crate::lsp::LspManager::request_implementation) {
            self.implementation_request_id = Some(id);
        }
    }

    /// Request the LSP references of the symbol at the current cursor (buffer)
    /// position. Used by the editor right-click "Go to References" item and the
    /// `⇧F12` keybinding. The server almost always returns many uses, which
    /// `drain_lsp_references` then offers as a picker.
    pub(in crate::app) fn request_references_at_cursor(&mut self) {
        // A request not sent (no path, no server) must not leave the last
        // one's id, or Peek would arm itself on it and a later plain Go to
        // References would open in the peek popup.
        self.references_request_id = None;
        self.references_want_peek = false;
        if let Some(id) = self.request_at_cursor(crate::lsp::LspManager::request_references) {
            self.references_request_id = Some(id);
        }
    }
}
