//! The completion popup, auto-imports, snippets, signature help, ghost-text suggestions
//! and Emmet.

use super::*;

/// The undo step a completion's resolved import joins.
pub(super) enum ImportUndo {
    /// The accept's remote group (#928).
    Group(u64),
    /// The vim change the accept went in (#934), joined only while its step
    /// is still the newest.
    Step(u64),
}

/// A hash of the text before `(row, col)`, `None` past the buffer's end:
/// what a completion's resolved import checks is unchanged (#928).
pub(super) fn text_hash_before(lines: &[String], (row, col): (usize, usize)) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let line = lines.get(row)?;
    let byte = line.char_indices().nth(col).map_or(line.len(), |(b, _)| b);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lines[..row].hash(&mut hasher);
    line[..byte].hash(&mut hasher);
    Some(hasher.finish())
}

impl App {
    /// Drain LSP completion responses. Drops responses with stale request
    /// ids so a slow earlier reply cannot clobber a fresher popup. Returns
    /// true when the popup state changed and the next frame should redraw.
    pub fn drain_lsp_completion(&mut self) -> bool {
        let mut changed = false;
        while let Some(result) = self.lsp.as_ref().and_then(|lsp| lsp.drain_completion()) {
            changed |= self.apply_completion_result(result);
        }
        changed
    }

    /// Open the popup for one completion reply; the drain loop delegates here
    /// because the reply channel's sender lives inside the LSP worker. Returns
    /// true when the popup state changed.
    pub(super) fn apply_completion_result(
        &mut self,
        result: crate::lsp::manager::CompletionResult,
    ) -> bool {
        if self
            .completion_resolve
            .as_ref()
            .is_some_and(|p| p.request_id == result.request_id)
        {
            return self.apply_resolved_completion(result);
        }
        if Some(result.request_id) != self.completion_request_id {
            return false;
        }
        // A matching id is not enough: nothing but the open popup clears the
        // id, so a reply that lands after an Enter moved the caret would open
        // on the next line with an empty prefix, and the next Enter accepted
        // its first item instead of breaking the line (#842).
        if !self.completion_origin_covers_caret(&result.path) {
            self.completion_request_id = None;
            return false;
        }
        self.completion_incomplete = result.is_incomplete;
        let Some((cx, cy)) = self.editor.cursor_screen_pos() else {
            return false;
        };
        let prefix = self.editor.word_before_cursor();
        let server_count = result.items.len();
        // Merge user snippets whose prefix matches the typed word into the
        // suggestion list (VS Code shows snippets in the suggest widget);
        // the popup's own prefix filter narrows them further.
        let mut items = result.items;
        // A server with nothing to offer, or none at all, leaves the words
        // of the open documents, as VS Code's word-based suggestions do
        // (#1566). A server's own list is never mixed with them.
        let words = self.completion_words && items.is_empty();
        if words {
            // An empty reply to an ask already showing its words (the
            // manager answering for a file no server took) changes nothing:
            // rebuilding would only drop the user's place in the list.
            if self
                .completion_popup
                .as_ref()
                .is_some_and(|p| p.request_id == result.request_id)
            {
                return false;
            }
            items = self.word_completion_items(&prefix);
        }
        let lang = self.editor.scope_id();
        for snip in self.snippets.matching(&prefix, lang) {
            items.push(snippet_completion_item(snip));
        }
        if items.is_empty() {
            if words {
                self.status = String::from("No suggestions");
            }
            return self.completion_popup.take().is_some();
        }
        let popup = crate::widgets::completion_popup::CompletionPopup::new(
            items,
            prefix.clone(),
            (cx, cy),
            result.path,
            result.request_id,
        );
        let filtered = popup.visible_indices().len();
        crate::lsp::log_file::log(&format!(
            "popup filter: prefix={prefix:?} server={server_count} visible={filtered}"
        ));
        if popup.visible_is_empty() {
            self.completion_popup = None;
            if self.completion_explicit {
                self.status =
                    format!("No completions match '{prefix}' ({server_count} from server)");
            }
        } else {
            // Only now do the popup's items come from this reply: a reply
            // that returned early above left the older items on screen, and
            // those still shift from their own request.
            self.completion_items_pos = self.completion_ask_pos;
            self.completion_popup = Some(popup);
        }
        true
    }

    /// Whether the caret is still in the word the pending completion was
    /// requested for: the same buffer and row, not left of the request point,
    /// and only word characters typed since. VS Code likewise cancels its
    /// suggest session the moment the caret leaves the word.
    pub(super) fn completion_origin_covers_caret(&self, path: &Path) -> bool {
        let Some((row, col)) = self.completion_origin else {
            return false;
        };
        let caret = self.editor.cursor_col;
        // An untitled buffer's words are asked for with an empty path.
        let here = self.editor.path.as_deref().unwrap_or(Path::new(""));
        if here != path || self.editor.cursor_row != row || caret < col {
            return false;
        }
        self.editor.lines.get(row).is_some_and(|line| {
            line.chars()
                .skip(col)
                .take(caret - col)
                .all(is_word_continuation)
        })
    }

    /// Add the auto-import a `completionItem/resolve` reply carries (#928),
    /// in the accept's undo step while nothing else has edited the buffer.
    /// Typing on after the item keeps it. An undo, an edit before the item,
    /// or an import placed at or after it, where the server's positions may
    /// no longer hold, drops it rather than put it in the wrong place.
    pub(super) fn apply_resolved_completion(
        &mut self,
        result: crate::lsp::manager::CompletionResult,
    ) -> bool {
        let Some(pending) = self.completion_resolve.take() else {
            return false;
        };
        let Some(item) = result.items.into_iter().next() else {
            return false;
        };
        if self.editor.path.as_ref() != Some(&pending.path)
            || self.editor.has_non_text_view()
            || text_hash_before(&self.editor.lines, pending.upto) != Some(pending.text_hash)
        {
            return false;
        }
        let (row, start) = pending.start;
        let Some(line) = self.editor.lines.get(row) else {
            return false;
        };
        let before_item = |e: &crate::widgets::editor::TextSpanEdit| {
            let col = |c| crate::widgets::editor::utf16_to_char_col(line, c as u32);
            let end = if e.utf16 { col(e.end.1) } else { e.end.1 };
            e.end.0 < row || (e.end.0 == row && end <= start)
        };
        let edits = item.additional_edits;
        if edits.is_empty() || !edits.iter().all(before_item) {
            return false;
        }
        let group = match pending.undo {
            ImportUndo::Group(group) => group,
            ImportUndo::Step(step) => {
                let group = next_remote_undo_group();
                self.editor.adopt_undo_step(step, group);
                group
            }
        };
        self.editor
            .apply_span_edits_as_in(&edits, crate::provenance::Seat::Generated, Some(group));
        true
    }

    /// Tab in the editor: advance a live snippet, else expand a user snippet
    /// whose prefix sits before the caret, else fall back to indentation.
    pub(super) fn handle_editor_tab(&mut self) {
        if self.editor.snippet_active() {
            self.editor.snippet_next();
            return;
        }
        if self.try_expand_snippet() {
            return;
        }
        if self.editor.selection_is_multiline() {
            self.editor.indent_lines();
        } else {
            self.editor.indent_at_cursor();
        }
    }

    /// Expand the user snippet whose prefix exactly matches the word before the
    /// caret (in the buffer's language). Returns false when nothing matches, so
    /// Tab can fall through to indenting.
    pub(super) fn try_expand_snippet(&mut self) -> bool {
        if self.snippets.is_empty() || self.editor.selection.is_some() {
            return false;
        }
        let word = self.editor.word_before_cursor();
        if word.is_empty() {
            return false;
        }
        let lang = self.editor.scope_id();
        let Some(body) = self.snippets.exact(&word, lang).map(|s| s.body.clone()) else {
            return false;
        };
        self.completion_popup = None;
        self.completion_request_id = None;
        self.expand_snippet(&body, word.chars().count());
        true
    }

    /// Expand snippet `body` at the caret (see
    /// [`Editor::expand_snippet_with`]), its variables resolving from the
    /// buffer and from the workspace: `CLIPBOARD`, `RELATIVE_FILEPATH`,
    /// `WORKSPACE_NAME` and `WORKSPACE_FOLDER` (#914).
    pub(super) fn expand_snippet(&mut self, body: &str, prefix_len: usize) {
        self.expand_snippet_in(body, prefix_len, None);
    }

    /// [`Self::expand_snippet`] in the undo step `group` (see
    /// [`Editor::expand_snippet_with_in`]): an accepted snippet completion
    /// and the auto-import its server resolves afterwards undo as one (#928).
    pub(super) fn expand_snippet_in(&mut self, body: &str, prefix_len: usize, group: Option<u64>) {
        let path = self.editor.path.clone();
        let root = path.as_deref().and_then(|p| self.roots.owning_root(p));
        let root = root.unwrap_or(self.workspace_root()).to_path_buf();
        let vars = |name: &str| {
            Some(match name {
                "CLIPBOARD" => crate::clipboard::read_string().unwrap_or_default(),
                "RELATIVE_FILEPATH" => path
                    .as_deref()
                    .map_or(String::new(), |p| relative_clipboard_text(p, &root)),
                "WORKSPACE_NAME" => root
                    .file_name()
                    .map_or(String::new(), |n| n.to_string_lossy().into()),
                "WORKSPACE_FOLDER" => root.display().to_string(),
                _ => return None,
            })
        };
        self.editor
            .expand_snippet_with_in(body, prefix_len, &vars, group);
    }

    pub fn trigger_completion(&mut self) {
        self.request_completion_at_caret(CompletionTrigger::Invoked, true);
    }

    /// Ask for completions at the caret. The server is asked when the file
    /// has a path and a manager runs. When no server reports on the file
    /// (a Makefile, a `.env`, a plain-text note), an explicit ask (`words`)
    /// opens at once on the words of the open documents (#1566); a server
    /// that answers later still replaces them with its own list.
    pub(super) fn request_completion_at_caret(&mut self, trigger: CompletionTrigger, words: bool) {
        if self.editor.has_non_text_view() {
            return;
        }
        let path = self.editor.path.clone();
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let asked = match (path.clone(), self.lsp.as_mut()) {
            (Some(path), Some(lsp)) => Some(lsp.request_completion(path, line, character, trigger)),
            _ => None,
        };
        let served = path
            .as_deref()
            .is_some_and(|p| self.lsp.as_ref().is_some_and(|l| l.document_has_server(p)));
        self.completion_explicit = words;
        let words = words && self.word_suggestions != crate::word_suggest::WordSuggestions::Off;
        if asked.is_none() && !words {
            return;
        }
        let id = asked.unwrap_or_else(|| {
            self.local_completion_seq -= 1;
            self.local_completion_seq
        });
        self.completion_request_id = Some(id);
        self.completion_origin = Some((self.editor.cursor_row, self.editor.cursor_col));
        self.completion_ask_pos = self.completion_origin;
        self.completion_incomplete = false;
        self.completion_reask_due = false;
        self.completion_ask_due = None;
        self.completion_words = words;
        if words && !served {
            self.apply_completion_result(crate::lsp::manager::CompletionResult {
                request_id: id,
                path: path.unwrap_or_default(),
                items: Vec::new(),
                is_incomplete: false,
            });
        }
    }

    /// The words of the open documents for a completion at the caret, as
    /// `word_suggestions` picks them (#1566).
    pub(super) fn word_completion_items(&self, prefix: &str) -> Vec<crate::lsp::CompletionItem> {
        use crate::word_suggest::WordSuggestions;
        let mode = self.word_suggestions;
        if mode == WordSuggestions::Off {
            return Vec::new();
        }
        let active = self.editor.active_index();
        let lang = self.editor.scope_id();
        let current_path = self.editor.path.as_deref();
        // Every open document, the other split groups' too, but not the one
        // being edited, nor a copy of it open in another group, whose words
        // would include the one being typed.
        let others: Vec<&[String]> = match mode {
            WordSuggestions::Off | WordSuggestions::CurrentDocument => Vec::new(),
            WordSuggestions::MatchingDocuments | WordSuggestions::AllDocuments => self
                .editor
                .editors
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != active)
                .map(|(_, e)| e)
                .chain(
                    self.editor_layout
                        .inactive_groups()
                        .into_iter()
                        .flat_map(|g| g.editors.iter()),
                )
                .filter(|e| current_path.is_none() || e.path.as_deref() != current_path)
                .filter(|e| mode == WordSuggestions::AllDocuments || e.scope_id() == lang)
                .map(|e| e.lines.as_slice())
                .collect(),
        };
        crate::word_suggest::collect(
            &self.editor.lines,
            (self.editor.cursor_row, self.editor.cursor_col),
            &others,
            prefix,
        )
        .into_iter()
        .map(crate::word_suggest::item)
        .collect()
    }

    /// Ask the server for parameter hints at the caret (typing `(` or `,`).
    pub fn trigger_signature_help(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_signature_help(path, line, character);
        self.signature_help_request_id = Some(id);
        self.signature_help_anchor = Some((
            self.editor.cursor_row,
            self.editor.cursor_col,
            self.editor.edit_seq,
        ));
    }

    /// Dismiss the parameter-hints popup (typing `)`, Esc, or moving away).
    pub(super) fn dismiss_signature_help(&mut self) {
        self.signature_help_popup = None;
        self.signature_help_request_id = None;
        self.signature_help_anchor = None;
    }

    /// While the parameter-hints popup is open, re-query when the caret moves
    /// or the buffer changes (typing an argument, backspacing, arrowing, a
    /// click). The empty-response path closes the popup once the caret is no
    /// longer inside a call; a non-empty reply keeps it open with the active
    /// parameter following the caret. Cheap: only runs while a popup is up.
    pub(super) fn refresh_signature_help_if_moved(&mut self) {
        if self.signature_help_popup.is_none() {
            return;
        }
        let now = (
            self.editor.cursor_row,
            self.editor.cursor_col,
            self.editor.edit_seq,
        );
        if self.signature_help_anchor == Some(now) {
            return;
        }
        // The caret left the editor entirely (focus moved to tree/terminal):
        // drop the hint rather than re-query against a stale position.
        if self.focus != Pane::Editor {
            self.dismiss_signature_help();
            return;
        }
        self.trigger_signature_help();
    }

    /// Drain LSP signatureHelp responses, dropping stale ids. An empty reply
    /// closes any open popup; otherwise the popup is re-anchored to the caret.
    pub fn drain_lsp_signature_help(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Some(result) = lsp.drain_signature_help() {
            if Some(result.request_id) != self.signature_help_request_id {
                continue;
            }
            if result.signatures.is_empty() {
                if self.signature_help_popup.is_some() {
                    self.signature_help_popup = None;
                    changed = true;
                }
                continue;
            }
            let Some((cx, cy)) = self.editor.cursor_screen_pos() else {
                continue;
            };
            self.signature_help_popup = Some(
                crate::widgets::signature_help_popup::SignatureHelpPopup::new(
                    result.signatures,
                    result.active_signature,
                    (cx, cy),
                    result.path,
                    result.request_id,
                ),
            );
            changed = true;
        }
        changed
    }

    /// Accept a completion by the server's own edit and its additional edits
    /// (auto-imports), shifted by what was typed or deleted at the caret since
    /// the request, as one generated edit and one undo step. A snippet's
    /// range is cleared by the same edit and its body then expanded at the
    /// caret, tab stops and all: expanded over the typed prefix alone, a
    /// range reaching further left the rest in the buffer, and the imports
    /// never came. An item without an edit replaces the typed word, imports
    /// and all, and one whose imports come on resolve asks for them (#928).
    /// False when the item has no usable edit, for the prefix fallback.
    pub(super) fn accept_completion_edit(&mut self, item: &crate::lsp::CompletionItem) -> bool {
        use crate::widgets::editor::TextSpanEdit;
        let Some(te) = item
            .text_edit
            .clone()
            .or_else(|| self.typed_word_edit(item))
        else {
            return false;
        };
        let row = self.editor.cursor_row;
        if te.start.0 != row || te.end.0 != row || (!item.is_snippet && te.new_text.contains('\n'))
        {
            return false;
        }
        let inserted = if item.is_snippet {
            String::new()
        } else {
            te.new_text.clone()
        };
        let Some(line) = self.editor.lines.get(row) else {
            return false;
        };
        let to_char = |e: &TextSpanEdit, c: usize| {
            if e.utf16 {
                crate::widgets::editor::utf16_to_char_col(line, c as u32)
            } else {
                c
            }
        };
        let caret = self.editor.cursor_col;
        // The server's ranges describe the line when the completion was asked
        // for; the caret has moved since by what was typed (or deleted) at it.
        // Everything on this row from the request point on shifts with it,
        // both ways: a Backspace after the request made the server's end
        // reach past the word and swallow the `)` after it.
        // The shift runs from where the reply's own request was sent, not
        // from the session's anchor: a re-ask answers for the caret as it was
        // then, and counting from the anchor again took the letters typed
        // before it twice, swallowing that many characters after the word.
        let asked_at = self.completion_items_pos.or(self.completion_origin);
        let shift: isize = match asked_at {
            Some((r, c)) if r == row => caret as isize - c as isize,
            _ => 0,
        };
        let origin_col = asked_at.map_or(caret, |(_, c)| c);
        let moved = |c: usize| -> usize {
            if c >= origin_col {
                (c as isize + shift).max(0) as usize
            } else {
                c
            }
        };
        let line_len = line.chars().count();
        let start = to_char(&te, te.start.1);
        let end = moved(to_char(&te, te.end.1)).max(caret).min(line_len);
        if start > caret {
            return false;
        }
        // The main edit and its additional edits (auto-imports) go in as ONE
        // edit against the same text, as the spec defines them: applied one
        // after the other, an import on this row landed in the edited line,
        // and one accept took two undos.
        let mut edits = vec![TextSpanEdit {
            start: (row, start),
            end: (row, end),
            new_text: inserted.clone(),
            utf16: false,
        }];
        for e in &item.additional_edits {
            let mut e = e.clone();
            if e.start.0 == row {
                e.start.1 = moved(to_char(&e, e.start.1));
                e.end.1 = if e.end.0 == row {
                    moved(to_char(&e, e.end.1))
                } else {
                    e.end.1
                };
                e.utf16 = false;
            }
            edits.push(e);
        }
        // The caret lands after the inserted text, moved by what the
        // additional edits before it added or removed.
        let mut caret_row = row as isize;
        let mut caret_col = (start + inserted.chars().count()) as isize;
        for e in &edits[1..] {
            if e.end > (row, start) {
                continue;
            }
            let added_rows = e.new_text.matches('\n').count() as isize;
            caret_row += added_rows - (e.end.0 - e.start.0) as isize;
            if e.end.0 == row {
                let tail = e.new_text.rsplit('\n').next().unwrap_or("").chars().count() as isize;
                caret_col += if added_rows == 0 && e.start.0 == row {
                    tail - (e.end.1 - e.start.1) as isize
                } else {
                    tail + e.start.1 as isize - e.end.1 as isize
                };
            }
        }
        // An accepted completion is the server's text, not the user's (#349),
        // and an import its server resolves later joins its undo step. In a
        // vim change both go in the change's own step, since Vim takes back
        // all an Insert added with one `u` (#934).
        let in_change = self.editor.undo_group_open();
        let group = (item.resolve.is_some() && !in_change).then(next_remote_undo_group);
        self.editor
            .apply_span_edits_as_in(&edits, crate::provenance::Seat::Generated, group);
        self.editor.cursor_row = caret_row.max(0) as usize;
        self.editor.cursor_col = caret_col.max(0) as usize;
        self.editor.clamp_cursor();
        if item.is_snippet {
            self.expand_snippet_in(&te.new_text, 0, group);
        }
        if let Some(listed) = item.resolve.clone() {
            let undo = group.map_or(
                ImportUndo::Step(self.editor.undo_step_id),
                ImportUndo::Group,
            );
            self.request_completion_imports(listed, (row, start), undo);
        }
        true
    }

    /// The edit an item without a `textEdit` makes (ty's auto-imports, #928):
    /// its text over the word typed before the caret, the end at the request
    /// point as a server's range is. `None` for a snippet, which the prefix
    /// path expands.
    pub(super) fn typed_word_edit(
        &self,
        item: &crate::lsp::CompletionItem,
    ) -> Option<crate::widgets::editor::TextSpanEdit> {
        let (row, caret) = (self.editor.cursor_row, self.editor.cursor_col);
        let word = self.editor.word_before_cursor().chars().count();
        let origin = self
            .completion_items_pos
            .or(self.completion_origin)
            .filter(|&(r, _)| r == row);
        let text = item.insert_text.as_ref().unwrap_or(&item.label);
        (!item.is_snippet).then(|| crate::widgets::editor::TextSpanEdit {
            start: (row, caret.saturating_sub(word)),
            end: (row, origin.map_or(caret, |(_, c)| c)),
            new_text: text.clone(),
            utf16: false,
        })
    }

    /// Ask the accepted item's server for the auto-import it left out of
    /// the list (#928); [`Self::apply_resolved_completion`] adds it.
    pub(super) fn request_completion_imports(
        &mut self,
        item: std::sync::Arc<lsp_types::CompletionItem>,
        start: (usize, usize),
        undo: ImportUndo,
    ) -> Option<()> {
        let path = self.editor.path.clone()?;
        let upto = (self.editor.cursor_row, self.editor.cursor_col);
        let text_hash = text_hash_before(&self.editor.lines, upto)?;
        let request_id = (self.lsp.as_mut()?).request_completion_resolve(path.clone(), item);
        self.completion_resolve = Some(PendingCompletionResolve {
            request_id,
            path,
            start,
            upto,
            text_hash,
            undo,
        });
        Some(())
    }

    /// Route the key through the completion popup when one is open.
    /// Up/Down navigate, Enter/Tab accept and replace the typed prefix
    /// with the chosen completion's full text, Esc dismisses. Printable
    /// chars and Backspace pass through to the editor and refresh the
    /// popup's filter prefix in place; if no items match the new prefix
    /// the popup dismisses on the next tick.
    pub(super) fn handle_completion_popup_key(&mut self, key: KeyEvent) -> bool {
        if self.completion_popup.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Up => {
                if let Some(p) = self.completion_popup.as_mut() {
                    p.move_up();
                }
                true
            }
            KeyCode::Down => {
                if let Some(p) = self.completion_popup.as_mut() {
                    p.move_down();
                }
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                let prefix_len = self
                    .completion_popup
                    .as_ref()
                    .map(|p| p.prefix.chars().count())
                    .unwrap_or(0);
                let text = self
                    .completion_popup
                    .as_ref()
                    .and_then(|p| p.selected_label());
                let is_snippet = self
                    .completion_popup
                    .as_ref()
                    .is_some_and(|p| p.selected_is_snippet());
                let item = self
                    .completion_popup
                    .as_ref()
                    .and_then(|p| p.selected_item().cloned());
                self.completion_popup = None;
                self.completion_request_id = None;
                if let Some(item) = item
                    && self.accept_completion_edit(&item)
                {
                    return true;
                }
                if let Some(t) = text {
                    if is_snippet {
                        // The body carries $1/$0 tab stops; expand_snippet
                        // removes the typed prefix itself and drives the caret.
                        self.expand_snippet(&t, prefix_len);
                    } else {
                        for _ in 0..prefix_len {
                            self.editor.backspace();
                        }
                        // An accepted completion is the SERVER's text, not
                        // the user's: they picked it from a list rather than
                        // writing it (#349).
                        self.editor
                            .insert_str_as(&t, crate::provenance::Seat::Generated);
                    }
                }
                true
            }
            KeyCode::Esc => {
                self.completion_popup = None;
                self.completion_request_id = None;
                true
            }
            // Ctrl/Alt+Backspace deletes a word (#909): it closes the popup
            // and falls through below, like any other key.
            KeyCode::Backspace
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.editor.backspace();
                self.refresh_completion_prefix();
                true
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.completion_popup = None;
                    self.completion_request_id = None;
                    return false;
                }
                self.type_char_in_editor(c);
                self.refresh_completion_prefix();
                if !is_word_continuation(c) {
                    self.completion_popup = None;
                    self.completion_request_id = None;
                }
                // `console.` with the list open for `console` asks anew.
                self.note_completion_typed(c);
                if is_signature_help_trigger(c) {
                    self.trigger_signature_help();
                }
                true
            }
            _ => {
                self.completion_popup = None;
                self.completion_request_id = None;
                false
            }
        }
    }

    pub(super) fn refresh_completion_prefix(&mut self) {
        let prefix = self.editor.word_before_cursor();
        let incomplete = self.note_incomplete_completion_edit();
        let Some(popup) = self.completion_popup.as_mut() else {
            return;
        };
        popup.set_prefix(prefix);
        if !popup.visible_is_empty() {
            return;
        }
        self.completion_popup = None;
        // A capped list (`isIncomplete`) only holds what matched the word
        // as it was (#1529): with none of it left, the session still waits
        // on the re-ask for the word as it is.
        if !incomplete {
            self.completion_request_id = None;
        }
    }

    /// A word edit while the pending completion is an incomplete list
    /// (#1529): mark a re-ask due, popup shown or not. Returns whether the
    /// session is incomplete.
    pub(super) fn note_incomplete_completion_edit(&mut self) -> bool {
        let live = self.completion_incomplete && self.completion_request_id.is_some();
        if live {
            self.completion_reask_due = true;
        }
        live
    }

    /// Send the re-ask a word edit left due (#1529). Runs after `sync_lsp`,
    /// so the server has the edit's `didChange` before the request; keeps
    /// the session's origin so the reply opens while the caret stays in the
    /// word, and ends the session once the caret has left it.
    pub(super) fn send_due_completion_reask(&mut self) {
        if !std::mem::take(&mut self.completion_reask_due) {
            return;
        }
        if !self.completion_incomplete || self.completion_request_id.is_none() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        if self.editor.has_non_text_view() || !self.completion_origin_covers_caret(&path) {
            self.completion_incomplete = false;
            self.completion_request_id = None;
            return;
        }
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_completion(path, line, character, CompletionTrigger::Incomplete);
        self.completion_request_id = Some(id);
        self.completion_ask_pos = Some((self.editor.cursor_row, self.editor.cursor_col));
    }

    /// After `c` is typed, open completion as VS Code does (#929): a
    /// character the file's server lists in `triggerCharacters` asks, and
    /// so, with quick suggestions on, does the first letter of a word in
    /// code. Its later letters only filter the list, even a closed one; a
    /// number or a letter typed into a word asks nothing.
    pub(super) fn note_completion_typed(&mut self, c: char) {
        let Some(path) = self.editor.path.as_deref() else {
            return;
        };
        let lsp = self.lsp.as_ref();
        let Some(triggers) = lsp.and_then(|l| l.completion_triggers(path)) else {
            return;
        };
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let line = self.editor.lines.get(row).map_or("", String::as_str);
        let quick = self.quick_suggestions
            && is_word_continuation(c)
            && !line.chars().nth(col).is_some_and(is_word_continuation)
            && !self
                .editor
                .word_before_cursor()
                .starts_with(|s: char| s.is_ascii_digit())
            && !self.completion_origin_covers_caret(path)
            && self.editor.caret_in_code();
        let trigger = if triggers.contains(c) {
            CompletionTrigger::Character(c)
        } else if quick {
            CompletionTrigger::Invoked
        } else {
            return;
        };
        self.completion_ask_due = Some((trigger, path.to_path_buf()));
        self.completion_origin = Some((self.editor.cursor_row, self.editor.cursor_col));
        self.completion_request_id = None;
    }

    /// Send the ask a typed key left due (#929), after `sync_lsp` like the
    /// re-ask, while the caret is still in what was typed: an Enter in the
    /// same burst would open it on the next line, for the next Enter to
    /// accept. Letters typed after a trigger character make it invoked.
    pub(super) fn send_due_completion_ask(&mut self) {
        let Some((trigger, path)) = self.completion_ask_due.take() else {
            return;
        };
        if !self.completion_origin_covers_caret(&path) {
            return;
        }
        let caret = (self.editor.cursor_row, self.editor.cursor_col);
        let trigger = match self.completion_origin {
            Some(origin) if origin != caret => CompletionTrigger::Invoked,
            _ => trigger,
        };
        self.request_completion_at_caret(trigger, false);
    }

    /// Emmet: Expand Abbreviation. Says why nothing happened rather than
    /// failing silently — the two reasons (wrong language, unparseable
    /// abbreviation) need different fixes from the user.
    pub(super) fn expand_emmet_abbreviation(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.status = if !self.editor.emmet_available() {
            format!(
                "Emmet: not available in {} files",
                self.editor.language_label()
            )
        } else if self.editor.expand_emmet_abbreviation() {
            String::from("Emmet: expanded abbreviation")
        } else {
            String::from("Emmet: no abbreviation at the cursor")
        };
    }

    /// Emmet: Wrap with Abbreviation or Update Tag (#1557): ask for the
    /// abbreviation or the new name in a one-line prompt, as Go to Line
    /// does. Says so instead where Emmet is not offered.
    pub(super) fn open_emmet_prompt(&mut self, wrap: bool) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        if !self.editor_is_text() {
            return;
        }
        if !self.editor.emmet_available() {
            self.status = format!(
                "Emmet: not available in {} files",
                self.editor.language_label()
            );
            return;
        }
        self.open_input_prompt(if wrap {
            InputPrompt::new(
                InputPurpose::EmmetWrap,
                "Wrap with Abbreviation",
                "abbreviation, such as div.card or ul>li*",
            )
        } else {
            InputPrompt::new(InputPurpose::EmmetUpdateTag, "Update Tag", "new tag name")
        });
    }

    /// Emmet's commands for markup already written that need no input
    /// (#1557), each saying why when it did nothing.
    pub(super) fn run_emmet_edit(&mut self, edit: EmmetEdit) {
        if !self.editor_is_text() {
            return;
        }
        if !self.editor.emmet_available() {
            self.status = format!(
                "Emmet: not available in {} files",
                self.editor.language_label()
            );
            return;
        }
        let (done, failed) = match edit {
            EmmetEdit::RemoveTag => (self.editor.emmet_remove_tag(), "no tag around the cursor"),
            EmmetEdit::MatchTag => (
                self.editor.emmet_match_tag(),
                "the cursor is not on a tag with a pair",
            ),
            EmmetEdit::BalanceOutward => (
                self.editor.emmet_balance(true),
                "nothing encloses the selection",
            ),
            EmmetEdit::BalanceInward => (
                self.editor.emmet_balance(false),
                "nothing inside the selection to narrow to",
            ),
        };
        if !done {
            self.status = format!("Emmet: {failed}");
        }
    }

    /// Editor: Toggle Inline Suggestions (#607, Cmd+K Shift+G). Turning it on
    /// resolves the backend first and says plainly when there is none; a
    /// file whose language server offers suggestions needs no model.
    pub(super) fn toggle_inline_suggestions(&mut self) {
        if self.inline_enabled {
            self.inline_enabled = false;
            self.editor.ghost = None;
            self.inline_job = None;
            self.inline_lsp = None;
            self.inline_asked = None;
            self.status = String::from("Inline suggestions: off");
            return;
        }
        let local =
            crate::session::read_pair_record(&crate::session::pair_record_path(&self.tree.root))
                .filter(|r| r.provider.as_deref() == Some("ollama"))
                .map(|r| {
                    (
                        r.base_url
                            .unwrap_or_else(|| String::from("http://localhost:11434")),
                        r.model,
                    )
                });
        match crate::inline_complete::resolve(local, |k| std::env::var(k).ok()) {
            Ok(config) => self.turn_inline_model_on(config),
            Err(why) if self.editor_offers_inline_completion() => {
                self.inline_config = None;
                self.inline_enabled = true;
                self.status = format!(
                    "Inline suggestions: on from the language server (no model: {why}); Tab accepts, Esc dismisses"
                );
            }
            Err(why) => self.status = format!("Inline suggestions need a model: {why}"),
        }
    }

    /// Whether the current file's language server offers inline completions
    /// (LSP 3.18): it then answers instead of the model (#607).
    pub(super) fn editor_offers_inline_completion(&self) -> bool {
        let path = self.editor.path.as_deref();
        path.zip(self.lsp.as_ref())
            .is_some_and(|(p, lsp)| lsp.document_offers_inline_completion(p))
    }

    /// Land the language server's inline completion (#607) when it still
    /// answers for the caret and the text it was asked about.
    pub(super) fn land_inline_lsp(&mut self) -> bool {
        let mut results = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_inline_completion() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, seq, row, col)) = self.inline_lsp.clone() else {
                continue;
            };
            if result.request_id != id {
                continue;
            }
            self.inline_lsp = None;
            let ed = &*self.editor;
            let still = ed.path.as_ref() == Some(&path)
                && ed.edit_seq == seq
                && ed.cursor_row == row
                && ed.cursor_col == col;
            let (Some(crate::lsp::manager::InlineItem { text, range }), true) =
                (result.item, still)
            else {
                continue;
            };
            let line = ed.lines.get(row).map_or("", String::as_str);
            let to_col = |c| crate::widgets::editor::utf16_to_char_col(line, c);
            let ghost = match range {
                None => Some(crate::inline_complete::Ghost::insert(row, col, seq, text)),
                Some((sl, sc, el, ec)) if sl as usize == row && el as usize == row => {
                    let range = to_col(sc)..to_col(ec);
                    crate::inline_complete::Ghost::replacing(row, col, seq, line, range, text)
                }
                Some(_) => None,
            };
            if ghost.is_some() {
                self.editor.ghost = ghost;
                changed = true;
            }
        }
        changed
    }

    /// Turn inline suggestions on from `config`'s model. Its worker is
    /// started, or started again when it stopped after crashing twice
    /// (#2598).
    pub(super) fn turn_inline_model_on(&mut self, config: crate::inline_complete::Config) {
        self.status = format!(
            "Inline suggestions: on ({}); Tab accepts, Esc dismisses",
            config.model
        );
        self.inline_config = Some(config);
        self.inline_worker
            .get_or_insert_with(crate::inline_complete::Worker::spawn)
            .restart_if_stopped();
        self.inline_enabled = true;
    }

    /// Ask for a suggestion once typing has paused with the caret at the end
    /// of a non-blank line, and land answers that still apply (#607).
    pub fn tick_inline_complete(&mut self) -> bool {
        if !self.inline_enabled {
            return false;
        }
        let mut changed = false;
        let mut landed = Vec::new();
        let (mut restarted, mut stopped) = (false, false);
        if let Some(worker) = &mut self.inline_worker {
            loop {
                match worker.poll() {
                    crate::jobs::WorkerPoll::Answer(done) => landed.push(done),
                    crate::jobs::WorkerPoll::Idle => break,
                    crate::jobs::WorkerPoll::Restarted => restarted = true,
                    crate::jobs::WorkerPoll::Stopped => {
                        stopped = true;
                        break;
                    }
                }
            }
        }
        for done in landed {
            let Some((id, path, seq, row, col)) = self.inline_job.clone() else {
                continue;
            };
            if done.id != id {
                continue;
            }
            self.inline_job = None;
            let still = self.editor.path.as_ref() == Some(&path)
                && self.editor.edit_seq == seq
                && self.editor.cursor_row == row
                && self.editor.cursor_col == col;
            match done.result {
                Ok(Some(text)) if still => {
                    self.editor.ghost =
                        Some(crate::inline_complete::Ghost::insert(row, col, seq, text));
                    changed = true;
                }
                Ok(_) => {}
                Err(e) => self.status = format!("Inline suggestion: {e}"),
            }
        }
        // Its thread crashed, and the crash notice said so. The request it
        // was on is not sent again, as each one is a model call: the next
        // pause in typing asks afresh, and after a second crash the status
        // line says how to start it again (#2598).
        if restarted || stopped {
            self.inline_job = None;
        }
        if stopped {
            self.status = String::from(INLINE_SUGGESTIONS_STOPPED);
            changed = true;
        }
        changed |= self.land_inline_lsp();
        let ed = &*self.editor;
        let Some(path) = ed.path.clone() else {
            return changed;
        };
        let row = ed.cursor_row;
        let col = ed.cursor_col;
        let seq = ed.edit_seq;
        let at_line_end = ed
            .lines
            .get(row)
            .is_some_and(|l| !l.trim().is_empty() && l.chars().skip(col).all(char::is_whitespace));
        let settled = ed
            .last_edit_at
            .is_some_and(|t| t.elapsed() >= crate::inline_complete::DEBOUNCE);
        let already = ed
            .ghost
            .as_ref()
            .is_some_and(|g| (g.row, g.col, g.seq) == (row, col, seq))
            // Asked already at this caret and edit: an empty answer, an
            // error or an Esc'd ghost must not send the request again on
            // the next tick, which looped hundreds of paid calls a second.
            || self
                .inline_asked
                .as_ref()
                .is_some_and(|a| a.0 == path && (a.1, a.2, a.3) == (seq, row, col))
            || [&self.inline_job, &self.inline_lsp]
                .into_iter()
                .flatten()
                .any(|j| j.1 == path && (j.2, j.3, j.4) == (seq, row, col));
        if self.focus != Pane::Editor
            || !at_line_end
            || !settled
            || already
            || ed.selection.is_some()
            || ed.has_non_text_view()
            || self.completion_popup.is_some()
        {
            return changed;
        }
        if self.editor_offers_inline_completion() {
            let (line, character) = self.editor.cursor_position_utf16();
            if let Some(lsp) = self.lsp.as_mut() {
                let id = lsp.request_inline_completion(path.clone(), line, character);
                self.inline_asked = Some((path.clone(), seq, row, col));
                self.inline_lsp = Some((id, path, seq, row, col));
            }
            return changed;
        }
        let (Some(worker), Some(config)) = (&self.inline_worker, &self.inline_config) else {
            return changed;
        };
        if worker.is_stopped() {
            return changed;
        }
        let id = self.inline_next_id;
        self.inline_next_id += 1;
        worker.submit(crate::inline_complete::Job {
            id,
            config: config.clone(),
            prompt: crate::inline_complete::prompt(&ed.lines, row, col),
        });
        self.inline_asked = Some((path.clone(), seq, row, col));
        self.inline_job = Some((id, path, seq, row, col));
        changed
    }

    /// Tab takes a live suggestion (#607), recorded as the navigator's
    /// writing; Esc drops it. Ctrl+Right takes its next word and Ctrl+Down
    /// its next line (#1625), leaving the rest live at the new caret. A
    /// language server's replacement is taken whole by Tab only.
    /// Returns whether the key was used.
    pub(super) fn inline_suggestion_key(&mut self, key: KeyEvent) -> bool {
        if self.completion_popup.is_some() || self.editor.snippet_active() {
            return false;
        }
        let Some(ghost) = self.editor.live_ghost_full().cloned() else {
            return false;
        };
        if let Some(r) = ghost.replaces {
            match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => {
                    self.editor.selection = Some(crate::widgets::editor::EditorSelection {
                        anchor: (ghost.row, r.start),
                        head: (ghost.row, r.end),
                    });
                    self.editor
                        .insert_str_as(&ghost.text, crate::provenance::Seat::Navigator);
                }
                (KeyCode::Esc, KeyModifiers::NONE) => {}
                _ => return false,
            }
            self.editor.ghost = None;
            return true;
        }
        let text = ghost.text;
        let take = match (key.code, key.modifiers) {
            (KeyCode::Tab, KeyModifiers::NONE) => text.len(),
            (KeyCode::Right, KeyModifiers::CONTROL) => crate::inline_complete::next_word_len(&text),
            (KeyCode::Down, KeyModifiers::CONTROL) => crate::inline_complete::next_line_len(&text),
            (KeyCode::Esc, KeyModifiers::NONE) => {
                self.editor.ghost = None;
                return true;
            }
            _ => return false,
        };
        self.editor.ghost = None;
        self.editor
            .insert_str_as(&text[..take], crate::provenance::Seat::Navigator);
        let rest = &text[take..];
        // The remainder stays live where the caret now is; its position
        // also counts as asked, so the next tick does not replace it.
        if !rest.trim().is_empty() {
            let at = (
                self.editor.cursor_row,
                self.editor.cursor_col,
                self.editor.edit_seq,
            );
            self.editor.ghost = Some(crate::inline_complete::Ghost::insert(
                at.0,
                at.1,
                at.2,
                rest.to_string(),
            ));
            // Recorded as asked too: once the ghost is Esc'd the tick
            // would otherwise send a fresh (paid) request at this caret.
            if let Some(path) = self.editor.path.clone() {
                self.inline_asked = Some((path, at.2, at.0, at.1));
            }
        }
        true
    }
}

/// Whether typing `c` continues a word (a letter, a digit or `_`), so the
/// completion list stays open.
pub(super) fn is_word_continuation(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The Emmet commands [`App::run_emmet_edit`] runs (#1557).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EmmetEdit {
    RemoveTag,
    MatchTag,
    BalanceOutward,
    BalanceInward,
}

/// The status when the inline suggestion worker crashed again soon after a
/// restart: turning suggestions on starts it again (#2598).
pub(super) const INLINE_SUGGESTIONS_STOPPED: &str = "Inline suggestions stopped after their worker crashed twice: turn them off and on to start them again";
