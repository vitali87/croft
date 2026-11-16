//! Symbol tabs and sibling tabs of one file: keeping a symbol tab on its symbol, and
//! mirroring a sibling's text, unsaved state and caret.

use super::*;

impl Editor {
    /// On a symbol tab, true when a bare Backspace (`forward` false) or
    /// Delete would join the symbol's first or last line with a line outside
    /// the tab: the caret sits on the clip's first character or after its
    /// last, with no selection to delete instead.
    pub(super) fn at_symbol_edge(&self, forward: bool) -> bool {
        let Some((first, end)) = self.symbol_clip() else {
            return false;
        };
        if self.selection.is_some_and(|s| s.has_area()) {
            return false;
        }
        if forward {
            self.cursor_row + 1 == end && self.cursor_col >= self.line_char_len(end - 1)
        } else {
            self.cursor_row == first && self.cursor_col == 0
        }
    }

    /// Copy the caret + scroll position from another editor onto this one,
    /// clamped to this buffer's bounds. Used when the editor is split so
    /// the duplicate opens exactly where the source was - including the
    /// wrap sub-line offset (`scroll_sub`), which is private to this
    /// module and can't be set from `App`.
    pub fn copy_view_position_from(&mut self, src: &Editor) {
        let max_row = self.lines.len().saturating_sub(1);
        self.cursor_row = src.cursor_row.min(max_row);
        let max_col = self
            .lines
            .get(self.cursor_row)
            .map_or(0, |l| l.chars().count());
        self.cursor_col = src.cursor_col.min(max_col);
        self.scroll = src.scroll.min(max_row);
        self.scroll_sub = src.scroll_sub;
        self.scroll_col = src.scroll_col;
    }

    /// Take a sibling's text, replacing only the lines that differ so a
    /// caret or selection below the change moves with its line rather than
    /// staying on a row number that now holds different text.
    ///
    /// `source_step` is the sibling's [`Self::undo_step_id`]: edits it made
    /// within one of its undo steps (a typing burst) fold into one step
    /// here, so undo in this tab walks back as far as it would there.
    /// `source_caret` is where the sibling's caret sits after the edit.
    /// Returns whether the text changed.
    pub fn mirror_lines_from(
        &mut self,
        new_lines: &[String],
        source_step: u64,
        source_caret: (usize, usize),
    ) -> bool {
        if self.lines == new_lines || new_lines.is_empty() {
            return false;
        }
        let prefix = self
            .lines
            .iter()
            .zip(new_lines)
            .take_while(|(a, b)| a == b)
            .count();
        let room = self.lines.len().min(new_lines.len()) - prefix;
        let suffix = self
            .lines
            .iter()
            .rev()
            .zip(new_lines.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        let old_end = self.lines.len() - suffix;
        let new_end = new_lines.len() - suffix;
        self.pin_on_edit();
        if self.mirrored_step == Some(source_step) {
            self.seed_bookmark_shadow();
            self.redo_stack.clear();
        } else {
            self.push_undo(EditKind::Replace);
            self.mirrored_step = Some(source_step);
        }
        // A selection clear of the changed lines survives, moving with its
        // lines; one touching them is cleared.
        let shift = |row: usize| row + new_end - old_end;
        if let Some(sel) = self.selection.as_mut() {
            let (lo, hi) = (sel.anchor.0.min(sel.head.0), sel.anchor.0.max(sel.head.0));
            if lo >= old_end {
                sel.anchor.0 = shift(sel.anchor.0);
                sel.head.0 = shift(sel.head.0);
            } else if hi >= prefix {
                self.selection = None;
            }
        }
        // Extra carets follow the same rule as the selection.
        self.carets.retain_mut(|c| {
            let (lo, hi) = (c.anchor.0.min(c.head.0), c.anchor.0.max(c.head.0));
            if lo >= old_end {
                c.anchor.0 = shift(c.anchor.0);
                c.head.0 = shift(c.head.0);
                true
            } else {
                hi < prefix
            }
        });
        self.mirror_caret = Some(source_caret);
        self.lines
            .splice(prefix..old_end, new_lines[prefix..new_end].iter().cloned());
        if self.cursor_row >= old_end {
            self.cursor_row = shift(self.cursor_row);
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// Start a fresh tab from a sibling's unsaved text, without an undo step:
    /// the tab never held the disk text as far as the user can tell, so
    /// undo must not walk back to it.
    pub fn seed_unsaved(&mut self, lines: Vec<String>, dirty: bool) {
        self.lines = lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.mark_buffer_changed();
        self.dirty = dirty;
        self.recompute_highlights();
    }

    /// Record that this buffer matches the file on disk: a sibling holding
    /// the same text was just saved (or undone back to its saved state).
    pub fn mark_clean_like_sibling(&mut self) {
        self.dirty = false;
        self.mark_synced_with_disk();
    }

    /// Turn a symbol tab into an ordinary tab of its whole file: the
    /// buffer already holds the whole file, so only the clip and the
    /// mirror state go.
    pub fn leave_symbol_view(&mut self) {
        self.symbol_view = None;
        self.mirror_seq = None;
        self.mirrored_step = None;
        self.mirror_caret = None;
    }

    /// Turn a symbol tab into an ordinary tab of its whole file while its
    /// text stays in step with its siblings: only the clip and the pending
    /// caret go, and the mirror state is kept. Callers: only right after a
    /// mirror pass settled the tab, or on a file a collab session keeps in
    /// step; a tab reused for other text needs `leave_symbol_view`.
    pub fn drop_symbol_clip(&mut self) {
        self.symbol_view = None;
        self.mirror_caret = None;
    }

    /// A symbol tab's visible lines as `(first, end)`, `end` exclusive,
    /// kept inside the buffer. `None` on an ordinary tab.
    pub fn symbol_clip(&self) -> Option<(usize, usize)> {
        let v = self.symbol_view.as_ref()?;
        let end = (v.last + 1).min(self.lines.len()).max(1);
        Some((v.first.min(end - 1), end))
    }

    /// The lines find and replace work over, and the row of the first: a
    /// symbol tab's symbol, or the whole buffer.
    pub fn find_scope(&self) -> (&[String], usize) {
        match self.symbol_clip() {
            Some((first, end)) => (&self.lines[first..end], first),
            None => (&self.lines, 0),
        }
    }

    /// The clip in the scrollbar's units: content rows above it, and rows
    /// in it. Visual rows when wrapping, else lines plus comment-box rows.
    pub(super) fn clip_content_rows(&self, width: usize, wrap: bool) -> Option<(usize, usize)> {
        let (first, end) = self.symbol_clip()?;
        let rows = |a: usize, b: usize| -> usize {
            if wrap {
                (a..b).map(|l| self.group_visual_rows(l, width)).sum()
            } else {
                (b - a) + self.box_rows_between(a, b, width)
            }
        };
        Some((rows(0, first), rows(first, end)))
    }

    /// Keep a symbol tab's caret, selections and viewport on its symbol's
    /// lines. Positions above the clip snap to its first character, below
    /// it to its last, so every motion that would leave the symbol stops at
    /// its edge instead. Secondary carets outside the clip are dropped
    /// rather than snapped, and duplicates removed.
    pub fn clamp_to_symbol_view(&mut self) {
        let Some((first, end)) = self.symbol_clip() else {
            return;
        };
        let last = end - 1;
        let last_len = self.line_char_len(last);
        let clamp = |p: (usize, usize)| -> (usize, usize) {
            if p.0 < first {
                (first, 0)
            } else if p.0 > last {
                (last, last_len)
            } else {
                p
            }
        };
        (self.cursor_row, self.cursor_col) = clamp((self.cursor_row, self.cursor_col));
        let clamp_sel = |sel: &mut EditorSelection| {
            sel.anchor = clamp(sel.anchor);
            sel.head = clamp(sel.head);
        };
        if let Some(sel) = self.selection.as_mut() {
            clamp_sel(sel);
        }
        // A secondary caret outside the symbol is dropped, not clamped:
        // clamped, several would stack on one edge and each type there.
        self.carets.retain(|c| (first..=last).contains(&c.head.0));
        self.carets.iter_mut().for_each(clamp_sel);
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        let mut seen = vec![primary];
        self.carets.retain(|c| {
            let fresh = !seen.contains(c);
            seen.push(*c);
            fresh
        });
        if self.scroll < first || self.scroll > last {
            self.scroll = self.scroll.clamp(first, last);
            self.scroll_sub = 0;
        }
    }
}
