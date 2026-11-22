//! Expand and Shrink Selection by the server's selection ranges, and highlighting the
//! other occurrences of the symbol under a resting caret.

use super::*;

impl App {
    /// Expand Selection (#254): grow every cursor to the next
    /// semantically meaningful range. Cached chain steps apply
    /// instantly; otherwise the LSP `selectionRange` chains are fetched
    /// (one request, all cursors), with tree-sitter node ancestry as the
    /// serverless fallback — so the gesture works with no LSP at all.
    pub(in crate::app) fn expand_selection(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.editor.validate_expand_stacks();
        if self.editor.expand_selection_from_stack() {
            return;
        }
        let lang_known = self.editor_language().is_some();
        if lang_known
            && self.lsp.is_some()
            && let Some(path) = self.editor.path.clone()
        {
            let seq = self.editor.edit_seq;
            let positions = self.editor.cursor_positions_utf16();
            // Reuse the pending request only when it was fired for these
            // exact cursors; a caret move re-fires for the new spot.
            if let Some(pending) = self.selection_range_request.as_mut()
                && pending.path == path
                && pending.seq == seq
                && pending.positions == positions
            {
                pending.presses += 1;
                return;
            }
            if let Some(lsp) = self.lsp.as_mut() {
                let id = lsp.request_selection_ranges(path.clone(), positions.clone());
                self.selection_range_request = Some(PendingSelectionRanges {
                    id,
                    path,
                    seq,
                    presses: 1,
                    positions,
                });
            }
            return;
        }
        if !self.editor.expand_selection_syntax() {
            self.status = String::from("No larger range to select");
        }
    }

    /// Shrink Selection: retrace the expand stack one step. Purely
    /// local — the stack already remembers the exact ranges.
    pub(in crate::app) fn shrink_selection(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.editor.shrink_selection_step();
    }

    /// Drain `selectionRange` replies (#254): triple-gated like
    /// occurrences (id + path + edit_seq), then either install the
    /// server's chains or resolve the queued presses via tree-sitter
    /// when the request came back unsupported/empty.
    pub(in crate::app) fn drain_lsp_selection_ranges(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut results = Vec::new();
        while let Some(r) = lsp.drain_selection_ranges() {
            results.push(r);
        }
        let mut changed = false;
        for r in results {
            let Some(pending) = self.selection_range_request.as_ref() else {
                continue;
            };
            // Gate on id + path + edit_seq AND the cursors the request
            // was fired for: a click between request and reply leaves
            // `edit_seq` unchanged, but the chains describe the OLD
            // caret and must not apply at the new one.
            if r.request_id != pending.id
                || self.editor.path.as_deref() != Some(pending.path.as_path())
                || self.editor.edit_seq != pending.seq
                || self.editor.cursor_positions_utf16() != pending.positions
            {
                if r.request_id == pending.id {
                    self.selection_range_request = None;
                }
                continue;
            }
            let presses = pending.presses;
            self.selection_range_request = None;
            if r.unsupported || r.chains.is_empty() {
                for _ in 0..presses {
                    if !self.editor.expand_selection_syntax() {
                        break;
                    }
                }
                changed = true;
                continue;
            }
            self.editor.install_selection_chains(r.chains);
            for _ in 0..presses {
                if !self.editor.expand_selection_from_stack() {
                    break;
                }
            }
            changed = true;
        }
        changed
    }

    /// Idle-caret occurrences trigger, run once per frame: when the caret has
    /// rested on one buffer position for `OCCURRENCES_IDLE`, fire ONE
    /// `documentHighlight` request for it; the moment it moves (or the buffer
    /// edits), drop the painted set. Mirrors VS Code's word highlight, which
    /// follows the cursor with a small debounce rather than every keystroke.
    pub fn tick_occurrences(&mut self) -> bool {
        const OCCURRENCES_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
        let eligible = self.editor.diff.is_none()
            && self.editor.sheet.is_none()
            && self.editor.image.is_none()
            && self.lsp.is_some();
        let Some(path) = self.editor.path.clone().filter(|_| eligible) else {
            self.occ_observed = None;
            self.occurrences_request = None;
            return self.editor.clear_occurrences();
        };
        let cur = (
            path,
            self.editor.cursor_row,
            self.editor.cursor_col,
            self.editor.edit_seq,
        );
        if self.occ_observed.as_ref() != Some(&cur) {
            self.occ_observed = Some(cur);
            self.occ_observed_at = std::time::Instant::now();
            // The old symbol's tints are stale the moment the caret leaves it
            // (an edit already cleared them via mark_buffer_changed), and an
            // in-flight reply for the old spot must never paint late.
            self.occurrences_request = None;
            return self.editor.clear_occurrences();
        }
        let already_requested = self
            .occurrences_request
            .as_ref()
            .is_some_and(|(_, p, r, c, s)| (p, r, c, s) == (&cur.0, &cur.1, &cur.2, &cur.3));
        if already_requested || self.occ_observed_at.elapsed() < OCCURRENCES_IDLE {
            return false;
        }
        let (uline, uchar) = self.editor.pos_to_utf16(cur.1, cur.2);
        let Some(lsp) = self.lsp.as_mut() else {
            return false;
        };
        let id = lsp.request_document_highlights(cur.0.clone(), uline, uchar);
        self.occurrences_request = Some((id, cur.0, cur.1, cur.2, cur.3));
        false
    }

    /// Apply a documentHighlight reply to the editor, dropping it unless it
    /// answers the in-flight request AND the editor still shows that file at
    /// that edit seq (the columns were computed against that text).
    pub fn drain_lsp_document_highlights(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_document_highlights() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, _row, _col, seq)) = self.occurrences_request.as_ref() else {
                continue;
            };
            if result.request_id != *id
                || self.editor.path.as_deref() != Some(path.as_path())
                || self.editor.edit_seq != *seq
            {
                continue;
            }
            self.editor.apply_occurrences(result.items);
            changed = true;
        }
        changed
    }
}
