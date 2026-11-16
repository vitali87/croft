//! Undo and redo: snapshots, undo groups and coalescing, the stack limits, and the history
//! a closed tab keeps for when its file is opened again.

use super::*;

/// A fresh id for an undo step, unique across every editor, so a symbol-tab
/// sibling can tell one of its source's steps from the next (#369).
pub(super) fn next_undo_step_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Snapshot {
    /// Take a collab peer's edit, given in this state's own text (#919):
    /// the caret, selection and extra carets move with their text.
    pub(super) fn take_peer_splice(&mut self, s: &crate::undo_rebase::Splice) {
        let edit = TextSpanEdit {
            start: self.lines.position(s.at),
            end: self.lines.position(s.at + s.deleted),
            new_text: s.inserted.clone(),
            utf16: false,
        };
        let map = |p| map_through_span_edits(p, std::slice::from_ref(&edit));
        (self.cursor_row, self.cursor_col) = map((self.cursor_row, self.cursor_col));
        for sel in self.selection.iter_mut().chain(self.carets.iter_mut()) {
            sel.anchor = map(sel.anchor);
            sel.head = map(sel.head);
        }
        self.lines
            .replace_range(s.at..s.at + s.deleted, &s.inserted);
        self.bytes = self.lines.heap_bytes();
        // No save so far wrote the peer's edit, which arrives only now.
        self.dirty = true;
    }
}

pub(super) const UNDO_STACK_LIMIT: usize = 500;

/// Bytes of text the undo steps of one buffer may hold (#694). Every step is
/// a whole copy of the buffer, so the 500-step cap alone let a 10 MB file
/// keep 5 GB of history. The oldest steps go first; the newest
/// [`UNDO_MIN_STEPS`] always stay, so a large file can still be undone.
pub(super) const UNDO_BYTES_LIMIT: usize = if cfg!(test) {
    1024 * 1024
} else {
    256 * 1024 * 1024
};

/// Undo steps kept whatever their size (see [`UNDO_BYTES_LIMIT`]).
pub(super) const UNDO_MIN_STEPS: usize = 16;

/// The undo and redo history of a file whose tab closed (#1568), kept for
/// the session so reopening the file brings it back, as VS Code's
/// `files.restoreUndoStack` does. It only applies to the text it was
/// recorded against: `text_hash` is the buffer at close, and a file that
/// reopens with other content (edited elsewhere, or closed with its unsaved
/// edits discarded) starts with no history.
pub(super) struct ClosedHistory {
    path: PathBuf,
    text_hash: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The editor's save point, so an undo across it re-dirties the buffer
    /// exactly as it would have in the tab that closed.
    save_seq: u64,
    bytes: usize,
}

/// Closed files whose history is kept, newest last; the oldest go first.
pub(super) const CLOSED_HISTORY_FILES: usize = 30;

/// Bytes of history kept across every closed file, on top of the per-buffer
/// [`UNDO_BYTES_LIMIT`]: the oldest closed file's history goes first.
pub(super) const CLOSED_HISTORY_BYTES: usize = UNDO_BYTES_LIMIT;

/// One hash of a buffer's lines, to tell whether a reopened file still has
/// the text a closed tab's history was recorded against.
pub(super) fn lines_hash(lines: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    lines.hash(&mut h);
    h.finish()
}

/// The key a file's closed history is filed under: its canonical path, so a
/// symlink or a relative spelling finds the same history.
pub(super) fn closed_history_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Editor {
    /// File this tab's undo/redo history away when the file leaves the tab
    /// (#1568). A tab with no history, no file, or a non-text view (whose
    /// `lines` are a placeholder) has nothing to keep, and a symbol tab's
    /// history covers a clip of the file, not the file.
    pub(super) fn stash_history(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        if (self.undo_stack.is_empty() && self.redo_stack.is_empty())
            || self.has_non_text_view()
            || self.symbol_view.is_some()
        {
            return;
        }
        let undo = std::mem::take(&mut self.undo_stack);
        let redo = std::mem::take(&mut self.redo_stack);
        let entry = ClosedHistory {
            path: closed_history_key(path),
            text_hash: lines_hash(&self.lines),
            bytes: undo.iter().chain(&redo).map(|s| s.bytes).sum(),
            undo,
            redo,
            save_seq: self.save_seq,
        };
        // `try_with`: a tab dropped while the thread's storage is being torn
        // down has nowhere to keep it, and must not panic.
        let _ = CLOSED_HISTORY.try_with(|kept| {
            let mut kept = kept.borrow_mut();
            kept.retain(|h| h.path != entry.path);
            kept.push(entry);
            let mut total: usize = kept.iter().map(|h| h.bytes).sum();
            while kept.len() > CLOSED_HISTORY_FILES
                || (total > CLOSED_HISTORY_BYTES && kept.len() > 1)
            {
                total -= kept.remove(0).bytes;
            }
        });
    }

    /// Take back the history kept for this tab's file when it closed, if the
    /// buffer is the text that history was recorded against (#1568).
    /// Whichever way it goes, the kept copy is used up: a second tab of the
    /// file must not replay the same history over its own edits.
    pub(super) fn restore_history(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        if self.has_non_text_view() || self.symbol_view.is_some() {
            return;
        }
        let key = closed_history_key(path);
        let Ok(Some(entry)) = CLOSED_HISTORY.try_with(|kept| {
            let mut kept = kept.borrow_mut();
            let i = kept.iter().position(|h| h.path == key)?;
            Some(kept.remove(i))
        }) else {
            return;
        };
        if entry.text_hash != lines_hash(&self.lines) {
            return;
        }
        self.undo_stack = entry.undo;
        self.redo_stack = entry.redo;
        self.save_seq = entry.save_seq;
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        let lines = PackedLines::from(&self.lines[..]);
        let bytes = lines.heap_bytes();
        Snapshot {
            lines,
            bytes,
            final_newline: self.final_newline,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            selection: self.selection,
            carets: self.carets.clone(),
            dirty: self.dirty,
            save_seq: self.save_seq,
            provenance: self.provenance.clone(),
            lsp_folds: self.lsp_folds.clone(),
            lsp_folds_lines: self.lsp_folds_lines,
        }
    }

    /// Drop the oldest undo steps past [`UNDO_STACK_LIMIT`] steps or
    /// [`UNDO_BYTES_LIMIT`] bytes, keeping at least [`UNDO_MIN_STEPS`].
    pub(super) fn trim_undo_stack(&mut self) {
        let mut total: usize = self.undo_stack.iter().map(|s| s.bytes).sum();
        let mut drop = 0;
        while self.undo_stack.len() - drop > UNDO_MIN_STEPS
            && (self.undo_stack.len() - drop > UNDO_STACK_LIMIT || total > UNDO_BYTES_LIMIT)
        {
            total -= self.undo_stack[drop].bytes;
            drop += 1;
        }
        self.undo_stack.drain(..drop);
    }

    /// Drop the redo steps furthest from the live text while the history
    /// holds more than [`UNDO_BYTES_LIMIT`] bytes, as a peer's edit carried
    /// into every state can make it (#919).
    pub(super) fn trim_redo_stack(&mut self) {
        let held = self.undo_stack.iter().chain(&self.redo_stack);
        let mut total: usize = held.map(|s| s.bytes).sum();
        let mut drop = 0;
        while drop < self.redo_stack.len() && total > UNDO_BYTES_LIMIT {
            total -= self.redo_stack[drop].bytes;
            drop += 1;
        }
        self.redo_stack.drain(..drop);
    }

    /// Undo and redo steps held, and the bytes of text they hold, for the
    /// memory report (#694).
    pub fn undo_history_size(&self) -> (usize, usize) {
        let steps = self.undo_stack.iter().chain(&self.redo_stack);
        (
            self.undo_stack.len() + self.redo_stack.len(),
            steps.map(|s| s.bytes).sum(),
        )
    }

    /// Push an undo entry tagged with the kind of edit about to happen.
    /// Coalesces consecutive `InsertChar` (or `MultiInsertChar`) ops into
    /// one step so a typing burst is undone as one unit; everything else
    /// opens a new step.
    pub(super) fn push_undo(&mut self, kind: EditKind) {
        self.push_undo_step(kind, None);
    }

    /// [`Self::push_undo`], joining the open step when `group` is the remote
    /// edit group that opened it (#1207).
    pub(super) fn push_undo_step(&mut self, kind: EditKind, group: Option<u64>) {
        // The pre-edit text, for bookmarks to be diffed against afterwards.
        self.seed_bookmark_shadow();
        // Where the edit about to happen begins — the linked-editing
        // mirror reads this to locate the keystroke (#254).
        self.last_edit_origin = (self.cursor_row, self.cursor_col);
        // Where the edit about to happen begins — the merge editor's
        // region tracker reads this when it reconciles (#253).
        self.merge_edit_row = self.cursor_row;
        // Inside an undo group a local edit joins the step the group's first
        // edit opened, unless an undo, a redo or a remote edit has put
        // another step on top since (#934).
        let in_group = group.is_none()
            && self.undo_group == Some(Some(self.undo_step_id))
            && !self.undo_stack.is_empty();
        let typing = matches!(kind, EditKind::InsertChar | EditKind::MultiInsertChar);
        let coalesce = in_group
            || (typing && self.last_edit_kind == Some(kind))
            || (group.is_some() && group == self.remote_undo_group && !self.undo_stack.is_empty());
        self.remote_undo_group = group;
        if !coalesce {
            self.undo_stack.push(self.snapshot());
            self.trim_undo_stack();
            self.undo_step_id = next_undo_step_id();
            self.mirrored_step = None;
        }
        if group.is_none()
            && let Some(step) = self.undo_group.as_mut()
        {
            *step = Some(self.undo_step_id);
        }
        // A fresh edit branches history: whatever was undone can no longer be
        // redone (VS Code's model). Cleared even on a coalesced keystroke so a
        // typing burst after an undo discards the redo branch on its first char.
        self.redo_stack.clear();
        self.last_edit_kind = Some(kind);
    }

    /// Undo the most recent edit step. Returns true iff state was changed.
    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else {
            return false;
        };
        self.remote_undo_group = None;
        // Stash the pre-undo state so `redo` can reinstate it.
        self.redo_stack.push(self.snapshot());
        self.restore_snapshot(snap);
        self.undo_step_id = next_undo_step_id();
        true
    }

    /// Reapply the most recently undone edit step. Returns true iff state was
    /// changed. Mirrors `undo`, moving the current state back onto the undo
    /// stack so the redone edit can itself be undone.
    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else {
            return false;
        };
        self.remote_undo_group = None;
        self.undo_stack.push(self.snapshot());
        self.restore_snapshot(snap);
        self.undo_step_id = next_undo_step_id();
        true
    }

    /// Replace the buffer state with a snapshot (shared by undo/redo), clamping
    /// the caret into the restored buffer and refreshing highlights.
    pub(super) fn restore_snapshot(&mut self, snap: Snapshot) {
        self.lines = snap.lines.iter().map(String::from).collect();
        self.final_newline = snap.final_newline;
        // The map that described this text comes back with it (#349): an
        // undo re-credits the lines it restores to the seats that wrote
        // them, a redo re-credits the edit to its seat. Truncated below to
        // the restored line count, as every other whole-buffer path does.
        self.provenance = snap.provenance;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.provenance.truncate(self.lines.len());
        self.lsp_folds = snap.lsp_folds;
        self.lsp_folds_lines = snap.lsp_folds_lines;
        self.cursor_row = snap.cursor_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap.cursor_col.min(self.line_char_len(self.cursor_row));
        self.selection = snap.selection;
        self.carets = snap.carets;
        // Crossing a save point (auto save or Cmd+S since the snapshot)
        // means the restored text differs from disk: stay dirty so the next
        // sweep reconverges them instead of leaving a silent divergence.
        // A restore that leaves unsaved changes pins the tab like any edit:
        // a file reopened with its closed-file history can be undone in the
        // preview tab without a keystroke typed there (#2073).
        self.set_dirty(snap.dirty || self.save_seq != snap.save_seq);
        // Undo/redo restored the WHOLE buffer, siblings included; the
        // linked-editing mirror must not replay on top of that (#254).
        self.linked_ranges.clear();
        self.linked_rows.clear();
        self.merge_edit_row = self.cursor_row;
        self.last_edit_kind = None;
        // A restore changes the buffer content, so bump the change counter (the
        // app resyncs the LSP / git gutter off `edit_seq`) and drop the wrap /
        // hscroll geometry caches keyed to the old text. `dirty` is taken from
        // the snapshot above, so this is not `mark_buffer_changed` (which would
        // force it true and lose an undo back to a saved state).
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.hscroll_content_cols = None;
        self.wrap_total_cache.clear();
        // Undo / redo move whole lines back and forth; marks follow them.
        self.sync_bookmarks_to_buffer();
        self.recompute_highlights();
    }

    /// Open a new undo step for the next edit (so a typing run doesn't
    /// merge with whatever comes after a movement / mouse / focus change).
    pub fn break_undo_coalescing(&mut self) {
        self.last_edit_kind = None;
    }

    /// Make every edit until [`Self::end_undo_group`] one undo step (#934):
    /// vim's `cw` and the text typed after it, or `o` and the new line's
    /// text, come back with one undo. The first edit opens a fresh step.
    pub fn begin_undo_group(&mut self) {
        self.undo_group = Some(None);
        self.last_edit_kind = None;
    }

    /// Close the group [`Self::begin_undo_group`] opened, if any. The next
    /// edit opens its own step.
    pub fn end_undo_group(&mut self) {
        self.undo_group = None;
        self.last_edit_kind = None;
    }

    /// Whether a group [`Self::begin_undo_group`] opened is still open: a
    /// vim change is under way (#934).
    pub fn undo_group_open(&self) -> bool {
        self.undo_group.is_some()
    }

    /// Let the remote edits of `group` join the newest undo step while it
    /// is still `step` (#934): a completion's import resolved after its vim
    /// change goes back into that change, but never into a step opened
    /// since, where they open a step of their own.
    pub fn adopt_undo_step(&mut self, step: u64, group: u64) {
        if self.undo_step_id == step && !self.undo_stack.is_empty() {
            self.remote_undo_group = Some(group);
        }
    }
}
