//! What a server does as you type: on-type formatting, linked editing of paired tags, the
//! characters that open parameter hints, and user snippets as completion items.

use super::*;

impl App {
    /// On-type formatting (#254), run once per frame: when enabled and the
    /// last edit was a keystroke in the server's advertised trigger set,
    /// fire `textDocument/onTypeFormatting` at the caret. The typed record
    /// is consumed here — examined exactly once, on the frame right after
    /// the keystroke — so a later tab switch or settings toggle can never
    /// replay a stale trigger. Never fires unadvertised: the trigger set
    /// comes from the capability the server declared, and `None` (no server
    /// probed, or no provider) means stay inert.
    pub fn tick_on_type_formatting(&mut self) {
        let taken = self.editor.last_typed.take();
        // A keystroke and a tab switch can arrive in the same input burst,
        // draining before this tick runs — the record is then armed on a tab
        // that is no longer active, and nothing else ever clears it. Sweep
        // every editor in every group (the active one is already drained by
        // the `take` above) so a trigger can only ever fire from the tab
        // that was active at its tick.
        let groups =
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut());
        for ed in groups.flat_map(|g| g.editors.iter_mut()) {
            ed.last_typed = None;
        }
        let Some((ch, seq)) = taken else {
            return;
        };
        if !self.format_on_type_for(editor_scope_id(&self.editor)) || !self.editor_is_text() {
            return;
        }
        if seq != self.editor.edit_seq {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lang) = crate::lsp::Language::from_path(&path) else {
            return;
        };
        let triggers = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.language_on_type_triggers(lang));
        if !triggers.is_some_and(|t| t.contains(ch)) {
            return;
        }
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_on_type_formatting(
            path.clone(),
            line,
            character,
            ch,
            tab_size,
            insert_spaces,
        );
        self.on_type_request = Some(FileRequest { id, path, seq });
    }

    /// What to do with one on-type formatting reply. Extracted from the
    /// drain loop so the three-way decision is testable without a live
    /// server: the channel's sender lives inside the LSP worker, so a test
    /// cannot inject a `FormatResult` through it.
    pub(in crate::app) fn on_type_reply_disposition(
        &self,
        result: &crate::lsp::manager::FormatResult,
    ) -> OnTypeReply {
        let Some(FileRequest {
            id,
            path: ref requested,
            seq,
        }) = self.on_type_request
        else {
            return OnTypeReply::NotOurs;
        };
        // A superseded reply must NOT clear the armed request: a newer one
        // is in flight and its own reply still has to apply.
        if result.request_id != id {
            return OnTypeReply::NotOurs;
        }
        // `edit_seq` is per-tab, so the counter comparison only means
        // "buffer unchanged" when the active tab is still the one the
        // request was computed against — require the path to match too.
        // Content-unchanged is also the whole test on purpose: caret-only
        // motion (arrow keys, auto-pair type-over) leaves the counter
        // alone, and that is correct — the server's edits are document
        // edits computed against text that is still byte-identical, so
        // they apply cleanly wherever the caret has wandered.
        if self.editor.edit_seq != seq
            || requested != &result.path
            || self.editor.path.as_deref() != Some(result.path.as_path())
        {
            return OnTypeReply::Stale;
        }
        OnTypeReply::Apply
    }

    /// Apply an on-type formatting reply — quietly, and only while the
    /// buffer has not moved since the request (the edits were computed
    /// against exactly that text; continued typing makes them stale).
    pub fn drain_lsp_on_type_format(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_on_type_formatting() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            match self.on_type_reply_disposition(&result) {
                OnTypeReply::NotOurs => continue,
                OnTypeReply::Stale => {
                    self.on_type_request = None;
                    continue;
                }
                OnTypeReply::Apply => self.on_type_request = None,
            }
            if let Some(edits) = result.edits.filter(|e| !e.is_empty())
                && self.apply_on_type_edits(&result.path, &edits)
            {
                changed = true;
            }
        }
        changed
    }

    /// Apply an on-type formatting reply for `path`, the active tab's file
    /// (the reply's disposition checked that), keeping the caret beside the
    /// text that was just typed.
    pub(crate) fn apply_on_type_edits(
        &mut self,
        path: &std::path::Path,
        edits: &[crate::widgets::editor::TextSpanEdit],
    ) -> bool {
        if self.editor.path.as_deref() == Some(path) {
            return self.editor.apply_span_edits_moving_caret(edits) > 0;
        }
        let applied = self.editor.apply_rename_to_open_tab(path, edits).is_some();
        self.editor.clamp_cursor();
        applied
    }

    /// Linked editing (#254): replay the last keystroke across the
    /// sibling tag ranges, keep the set alive only while the caret
    /// stays inside one, and (debounced on caret idle) ask the server
    /// for the set at a new caret position. Never fires a request for a
    /// language the LSP layer isn't tracking; a server without the
    /// provider simply never answers, and the feature stays inert.
    pub fn tick_linked_editing(&mut self) -> bool {
        const LINKED_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
        let mirrored = self.editor.mirror_linked_edit();
        let eligible = self.editor_is_text() && self.lsp.is_some();
        let Some(path) = self.editor.path.clone().filter(|_| eligible) else {
            self.linked_observed = None;
            self.linked_request = None;
            self.linked_asked = false;
            self.editor.clear_linked_ranges();
            return mirrored;
        };
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        // While the caret stays inside the live set, keep it — the whole
        // point is typing inside a range. (The mirror above re-synced
        // positions for this frame's keystroke.)
        if self.editor.has_linked_ranges() {
            if self.editor.linked_ranges_contain(row, col) {
                return mirrored;
            }
            self.editor.clear_linked_ranges();
        }
        let cur = CaretSpot {
            path,
            row,
            col,
            seq: self.editor.edit_seq,
        };
        if self.linked_observed.as_ref() != Some(&cur) {
            self.linked_observed = Some(cur);
            self.linked_observed_at = std::time::Instant::now();
            self.linked_request = None;
            self.linked_asked = false;
            return mirrored;
        }
        if self.linked_asked || self.linked_observed_at.elapsed() < LINKED_IDLE {
            return mirrored;
        }
        let (uline, uchar) = self.editor.cursor_position_utf16();
        let Some(lsp) = self.lsp.as_mut() else {
            return mirrored;
        };
        let id = lsp.request_linked_editing(cur.path.clone(), uline, uchar);
        self.linked_request = Some(FileRequest {
            id,
            path: cur.path,
            seq: cur.seq,
        });
        self.linked_asked = true;
        mirrored
    }

    /// Install a linkedEditingRange reply, gated on the in-flight id,
    /// the path, and the edit seq the spans were computed against.
    pub fn drain_lsp_linked_editing(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_linked_editing() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some(FileRequest { id, path, seq }) = self.linked_request.as_ref() else {
                continue;
            };
            if result.request_id != *id
                || self.editor.path.as_deref() != Some(path.as_path())
                || self.editor.edit_seq != *seq
            {
                continue;
            }
            self.linked_request = None;
            self.editor.set_linked_ranges(&result.ranges);
            changed = true;
        }
        changed
    }
}

/// Characters that open or advance a call, triggering parameter hints.
pub(in crate::app) fn is_signature_help_trigger(c: char) -> bool {
    c == '(' || c == ','
}

/// Build a completion-popup item for a user snippet: the row shows the prefix,
/// filters on it, and carries the snippet body as `is_snippet` insertion text
/// the accept path expands through the editor's tab-stop engine.
pub(in crate::app) fn snippet_completion_item(
    snip: &crate::snippets::Snippet,
) -> crate::lsp::CompletionItem {
    crate::lsp::CompletionItem {
        label: snip.prefix.clone(),
        detail: Some(String::from("snippet")),
        insert_text: Some(snip.body.clone()),
        filter_text: Some(snip.prefix.clone()),
        kind: Some(lsp_types::CompletionItemKind::SNIPPET),
        is_snippet: true,
        ..Default::default()
    }
}
