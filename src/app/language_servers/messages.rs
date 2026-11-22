//! What the servers tell croft: work-done progress and the status-bar line it makes,
//! `window/*` messages as status lines, a server's questions in the list picker, and
//! `window/showDocument`.

use super::*;

/// A server's message as one status line, `server: first line`, so a
/// multi-line cargo error does not spill over the bar.
pub(in crate::app) fn server_message_status(server: &str, text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    format!("{server}: {line}")
}

impl App {
    /// Drain server work-done progress updates into the per-server map the
    /// status bar reads. Returns true when anything changed so the main loop
    /// repaints the bar. A `Some(message)` upserts the server's status; a
    /// `None` (the server's `End`) clears it.
    pub fn drain_lsp_progress(&mut self) -> bool {
        let mut updates = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(u) = lsp.drain_server_update() {
                updates.push(u);
            }
        }
        let mut changed = false;
        for u in updates {
            changed |= self.apply_server_update(u);
        }
        changed | self.ask_next_lsp_question()
    }

    /// Act on one progress update or `window/*` message from a server.
    /// Returns true when the screen needs a repaint.
    pub(in crate::app) fn apply_server_update(
        &mut self,
        u: crate::lsp::manager::ServerUpdate,
    ) -> bool {
        use crate::lsp::manager::ServerEvent;
        match u.event {
            ServerEvent::Progress(Some(message)) => {
                self.lsp_progress.insert(u.server, message);
                true
            }
            ServerEvent::Progress(None) => self.lsp_progress.remove(&u.server).is_some(),
            // An error a server can't recover from on its own, such as
            // rust-analyzer's "Failed to load workspaces", used to sit in
            // OUTPUT while the status bar said all was well (#1599).
            ServerEvent::Message { text, .. } => {
                self.status = server_message_status(&u.server, &text);
                true
            }
            ServerEvent::Question {
                text,
                actions,
                reply,
                ..
            } => {
                if actions.is_empty() {
                    // Nothing to pick: a message, answered "no action" by
                    // dropping `reply`.
                    self.status = server_message_status(&u.server, &text);
                    return true;
                }
                self.lsp_questions.push_back(LspQuestion {
                    server: u.server,
                    text,
                    actions,
                    reply,
                });
                false
            }
            ServerEvent::ShowDocument {
                uri,
                external,
                take_focus,
                selection,
                reply,
            } => {
                let shown = self.show_server_document(uri, external, take_focus, selection);
                let _ = reply.send(shown);
                true
            }
        }
    }

    /// Put the oldest waiting server question in the list picker, once no
    /// other picker is open. Returns true when one was opened.
    pub(in crate::app) fn ask_next_lsp_question(&mut self) -> bool {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let asking = self
            .list_picker
            .as_ref()
            .is_some_and(|p| p.purpose == ListPurpose::ServerQuestion);
        if !asking {
            // Its picker was replaced or went away without a pick.
            self.lsp_question = None;
        }
        if self.list_picker.is_some() {
            return false;
        }
        let Some(q) = self.lsp_questions.pop_front() else {
            return false;
        };
        let rows = q
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| ListRow {
                id: i.to_string(),
                label: a.title.clone(),
            })
            .collect();
        let title = server_message_status(&q.server, &q.text);
        self.status = title.clone();
        self.lsp_question = Some(q);
        self.open_list_picker(
            ListPicker::new(ListPurpose::ServerQuestion, title, rows),
            "",
        );
        true
    }

    /// `window/showDocument`: a link goes to the browser, a file opens with
    /// the server's range selected. Without `take_focus` the file still
    /// opens but keyboard focus stays in the pane that had it. Returns
    /// whether the document was shown, which is the server's answer.
    pub(in crate::app) fn show_server_document(
        &mut self,
        uri: lsp_types::Url,
        external: bool,
        take_focus: bool,
        selection: Option<lsp_types::Range>,
    ) -> bool {
        let path = (!external && uri.scheme() == "file")
            .then(|| uri.to_file_path().ok())
            .flatten();
        let Some(path) = path else {
            return self.try_open_url_for_user(uri.as_str());
        };
        let start = selection.map(|r| r.start).unwrap_or_default();
        let (focus, maximized) = (self.focus, self.terminal_maximized);
        if !self.try_go_to_definition(path.clone(), start.line, start.character) {
            return false;
        }
        if !take_focus && focus != Pane::Editor {
            self.focus_pane(focus);
            self.terminal_maximized = maximized;
        }
        let Some(end) = selection.map(|r| r.end).filter(|&end| end != start) else {
            return true;
        };
        if self.editor.path.as_deref() != Some(path.as_path()) {
            return true;
        }
        let row = (end.line as usize).min(self.editor.lines.len().saturating_sub(1));
        let col = self.editor.utf16_col_to_char_pub(row, end.character);
        let anchor = (self.editor.cursor_row, self.editor.cursor_col);
        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
            anchor,
            head: (row, col),
        });
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
        self.editor.ensure_cursor_col_visible();
        true
    }

    /// The active LSP progress line for the status bar, if any server is busy
    /// (e.g. `rust-analyzer: Indexing 112/340 33%`). Joins multiple busy
    /// servers with `  ·  ` so a Rust + Python workspace shows both.
    pub(in crate::app) fn lsp_progress_status(&self) -> Option<String> {
        if self.lsp_progress.is_empty() {
            return None;
        }
        let mut entries: Vec<String> = self
            .lsp_progress
            .iter()
            .map(|(server, msg)| format!("{server}: {msg}"))
            .collect();
        entries.sort();
        Some(entries.join("  ·  "))
    }
}
