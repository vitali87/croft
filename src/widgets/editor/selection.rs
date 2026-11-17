//! Selections: making and clearing them, the selected text, linked editing, and expanding
//! or shrinking a selection by syntax.

use super::*;

/// Inclusive char-indexed range `(row, col)` anchor and head, where head
/// follows the cursor as the user drags / shift-arrows.  `normalised()` returns
/// the pair in row-major order so callers don't have to care which end came
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorSelection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl EditorSelection {
    pub fn new(row: usize, col: usize) -> Self {
        Self {
            anchor: (row, col),
            head: (row, col),
        }
    }

    pub fn normalised(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

/// One linked-editing span (#254): a single-line char-column range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LinkedRange {
    row: usize,
    start: usize,
    len: usize,
}

/// The Expand/Shrink Selection state (#254): per-cursor step stacks.
/// See the `select_expand` field doc for the validity contract.
pub(super) struct SelectExpandStacks {
    /// The `edit_seq` the stacks were built against.
    edit_seq: u64,
    /// One stack per cursor: primary first, then `carets` in order.
    stacks: Vec<ExpandStack>,
}

/// One cursor's selection-growth history, smallest step first.
pub(super) struct ExpandStack {
    steps: Vec<EditorSelection>,
    /// Index of the step the buffer currently shows.
    pos: usize,
}

/// Max selected-text length that still drives occurrence highlighting, matching
/// VS Code's default `editor.selectionHighlightMaxLength`.
pub(super) const SELECTION_HIGHLIGHT_MAX_LEN: usize = 200;

impl Editor {
    pub fn start_selection_at_cursor(&mut self) {
        self.selection = Some(EditorSelection::new(self.cursor_row, self.cursor_col));
    }

    pub fn extend_selection_to_cursor(&mut self) {
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (self.cursor_row, self.cursor_col);
        } else {
            self.start_selection_at_cursor();
        }
    }

    /// An editor `(row, char col)` as an LSP UTF-16 `(line, character)`.
    pub fn pos_to_utf16(&self, row: usize, col: usize) -> (u32, u32) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        let row = row.min(self.lines.len() - 1);
        let line = &self.lines[row];
        let byte = char_byte(line, col.min(line.chars().count()));
        (row as u32, line[..byte].encode_utf16().count() as u32)
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.occurrence_word_sel = None;
    }

    // ---- Linked editing (#254) ------------------------------------------

    /// Install the server's linked-editing set: UTF-16
    /// `(start_line, start_char, end_line, end_char)` spans. Multi-line
    /// spans (which the protocol forbids anyway) and sets smaller than a
    /// pair are dropped. Positions are snapshotted against the current
    /// buffer (`edit_seq` + per-row lengths).
    pub fn set_linked_ranges(&mut self, spans: &[(u32, u32, u32, u32)]) {
        let mut ranges: Vec<LinkedRange> = spans
            .iter()
            .filter(|&&(sl, _, el, _)| sl == el && (sl as usize) < self.lines.len())
            .map(|&(sl, sc, _, ec)| {
                let row = sl as usize;
                let start = utf16_to_char_col(&self.lines[row], sc);
                let end = utf16_to_char_col(&self.lines[row], ec);
                LinkedRange {
                    row,
                    start,
                    len: end.saturating_sub(start),
                }
            })
            .collect();
        ranges.sort_by_key(|r| (r.row, r.start));
        if ranges.len() < 2 {
            self.clear_linked_ranges();
            return;
        }
        self.linked_ranges = ranges;
        self.resnapshot_linked_rows();
        self.linked_seq = self.edit_seq;
    }

    /// The primary caret as an LSP UTF-16 `(line, character)` pair.
    pub fn cursor_position_utf16(&self) -> (u32, u32) {
        let row = self.cursor_row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        let byte = char_byte(line, self.cursor_col.min(line.chars().count()));
        (row as u32, line[..byte].encode_utf16().count() as u32)
    }

    pub fn clear_linked_ranges(&mut self) {
        self.linked_ranges.clear();
        self.linked_rows.clear();
    }

    pub fn has_linked_ranges(&self) -> bool {
        !self.linked_ranges.is_empty()
    }

    /// True when `(row, col)` sits inside (or at the end boundary of) a
    /// linked range — the caret positions where the set stays alive.
    pub fn linked_ranges_contain(&self, row: usize, col: usize) -> bool {
        self.linked_ranges
            .iter()
            .any(|r| r.row == row && col >= r.start && col <= r.start + r.len)
    }

    pub(super) fn resnapshot_linked_rows(&mut self) {
        let mut rows: Vec<usize> = self.linked_ranges.iter().map(|r| r.row).collect();
        rows.dedup();
        self.linked_rows = rows
            .into_iter()
            .map(|row| (row, self.lines[row].chars().count()))
            .collect();
    }

    /// Replay the last edit across the sibling linked ranges (#254): the
    /// paired-tag auto-rename. Called once per tick after key handling;
    /// returns true when siblings were rewritten. Trusts only a clean
    /// single-step, single-row edit that began inside one range and
    /// keeps the content a plausible tag word — anything else clears
    /// the set rather than corrupt text far from the caret.
    pub fn mirror_linked_edit(&mut self) -> bool {
        if self.linked_ranges.len() < 2 || self.edit_seq == self.linked_seq {
            return false;
        }
        // A single edit advances the seq exactly once; a missed frame
        // (paste burst, undo, reload) is not safely replayable.
        if self.edit_seq != self.linked_seq.wrapping_add(1) {
            self.clear_linked_ranges();
            return false;
        }
        let (erow, ecol) = self.last_edit_origin;
        // Exactly one linked row may have changed, by the edit's delta.
        let changed: Vec<(usize, isize)> = self
            .linked_rows
            .iter()
            .filter_map(|&(row, old_len)| {
                let now = self.lines.get(row)?.chars().count() as isize;
                let delta = now - old_len as isize;
                (delta != 0).then_some((row, delta))
            })
            .collect();
        let delta = match changed.as_slice() {
            [] => 0isize,
            [(row, delta)] if *row == erow => *delta,
            _ => {
                self.clear_linked_ranges();
                return false;
            }
        };
        // The edit must have begun inside one range on that row.
        let Some(i) = self
            .linked_ranges
            .iter()
            .position(|r| r.row == erow && ecol >= r.start && ecol <= r.start + r.len)
        else {
            self.clear_linked_ranges();
            return false;
        };
        let new_len = self.linked_ranges[i].len as isize + delta;
        if new_len < 0 {
            self.clear_linked_ranges();
            return false;
        }
        let new_len = new_len as usize;
        // A boundary-adjacent backspace/delete-forward removes the
        // delimiter OUTSIDE the range, not the range's own content, and
        // leaves the caret outside the updated span. Reject that instead
        // of writing a truncated word into the siblings.
        if self.cursor_row != erow
            || self.cursor_col < self.linked_ranges[i].start
            || self.cursor_col > self.linked_ranges[i].start + new_len
        {
            self.clear_linked_ranges();
            return false;
        }
        let src = self.linked_ranges[i];
        let line = &self.lines[src.row];
        let from = char_byte(line, src.start);
        let to = char_byte(line, src.start + new_len);
        let text: String = line[from..to].to_string();
        // Keep it a plausible identifier/tag word; a delimiter means the
        // user is doing something the mirror shouldn't propagate.
        if text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '/' | '"' | '\''))
        {
            self.clear_linked_ranges();
            return false;
        }
        // Rewrite every sibling, rightmost-first per row so earlier
        // starts stay valid while lengths change.
        let mut order: Vec<usize> = (0..self.linked_ranges.len()).filter(|&j| j != i).collect();
        order.sort_by_key(|&j| {
            let r = &self.linked_ranges[j];
            (r.row, std::cmp::Reverse(r.start))
        });
        for j in order {
            let r = self.linked_ranges[j];
            // Siblings after the edit on the edited row already shifted.
            let start = if r.row == erow && r.start > ecol {
                (r.start as isize + delta).max(0) as usize
            } else {
                r.start
            };
            let line = &mut self.lines[r.row];
            let from = char_byte(line, start);
            let to = char_byte(line, start + r.len);
            line.replace_range(from..to, &text);
        }
        // Recompute stored positions: every range now has `new_len`; on
        // each row, starts shift by the accumulated growth of preceding
        // ranges (the edited range's own delta included).
        let per = new_len as isize - src.len as isize;
        let mut shift: std::collections::HashMap<usize, isize> = std::collections::HashMap::new();
        for r in self.linked_ranges.iter_mut() {
            let acc = shift.entry(r.row).or_insert(0);
            r.start = (r.start as isize + *acc).max(0) as usize;
            r.len = new_len;
            *acc += per;
        }
        // The mirrors changed the buffer: bump the seq so highlights /
        // LSP resync, and adopt it as the new baseline. Deliberately no
        // undo push — the user's keystroke snapshot holds the whole
        // pre-edit buffer, so one Undo reverts the keystroke AND the
        // mirrors together (VS Code's model).
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.resnapshot_linked_rows();
        self.linked_seq = self.edit_seq;
        true
    }

    // ---- Expand / Shrink Selection (#254) -------------------------------

    /// Every cursor's current selection, primary first — a zero-area
    /// selection at the caret when nothing is selected.
    pub(super) fn cursor_selections(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// Ensure the expand stacks describe the CURRENT cursors; rebuild
    /// from scratch on any mismatch (edit, click, caret reshuffle). See
    /// the `select_expand` field doc for why matching, not indexing, is
    /// the validity test.
    pub fn validate_expand_stacks(&mut self) {
        let cur = self.cursor_selections();
        let valid = self.select_expand.as_ref().is_some_and(|se| {
            se.edit_seq == self.edit_seq
                && se.stacks.len() == cur.len()
                && se.stacks.iter().zip(&cur).all(|(st, c)| {
                    st.steps
                        .get(st.pos)
                        .is_some_and(|s| s.normalised() == c.normalised())
                })
        });
        if !valid {
            self.select_expand = Some(SelectExpandStacks {
                edit_seq: self.edit_seq,
                stacks: cur
                    .into_iter()
                    .map(|c| ExpandStack {
                        steps: vec![c],
                        pos: 0,
                    })
                    .collect(),
            });
        }
    }

    /// Write every stack's current step back onto the cursors, keeping
    /// the cursor-follows-head convention of every other selection setter.
    pub(super) fn apply_expand_steps(&mut self) {
        let Some(se) = self.select_expand.as_ref() else {
            return;
        };
        let sels: Vec<EditorSelection> = se.stacks.iter().map(|st| st.steps[st.pos]).collect();
        let primary = sels[0];
        self.selection = primary.has_area().then_some(primary);
        self.cursor_row = primary.head.0.min(self.lines.len().saturating_sub(1));
        self.cursor_col = primary.head.1.min(self.line_char_len(self.cursor_row));
        self.carets = sels[1..].to_vec();
        self.ensure_cursor_col_visible();
    }

    /// Step every cursor one level up its cached chain. False when no
    /// stack has a deeper step cached (the caller then computes one —
    /// LSP chain or syntax ancestry).
    pub fn expand_selection_from_stack(&mut self) -> bool {
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for st in &mut se.stacks {
            if st.pos + 1 < st.steps.len() {
                st.pos += 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// Shrink Selection: retrace one step down the stack. False at the
    /// bottom (the gesture's starting point).
    pub fn shrink_selection_step(&mut self) -> bool {
        self.validate_expand_stacks();
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for st in &mut se.stacks {
            if st.pos > 0 {
                st.pos -= 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// Install per-cursor LSP `selectionRange` chains (#254): UTF-16
    /// `(start_line, start_char, end_line, end_char)` spans, smallest
    /// first, index-aligned with the cursors. Each stack's history above
    /// its current step is replaced by the strictly-growing tail of its
    /// chain; the caller then calls [`Self::expand_selection_from_stack`].
    pub fn install_selection_chains(&mut self, chains: Vec<Vec<(u32, u32, u32, u32)>>) {
        self.validate_expand_stacks();
        // Convert before mutably borrowing the stacks.
        let converted: Vec<Vec<EditorSelection>> = chains
            .iter()
            .map(|chain| {
                chain
                    .iter()
                    .map(|&(sl, sc, el, ec)| {
                        let sr = (sl as usize).min(self.lines.len().saturating_sub(1));
                        let er = (el as usize).min(self.lines.len().saturating_sub(1));
                        EditorSelection {
                            anchor: (sr, utf16_to_char_col(&self.lines[sr], sc)),
                            head: (er, utf16_to_char_col(&self.lines[er], ec)),
                        }
                    })
                    .collect()
            })
            .collect();
        let Some(se) = self.select_expand.as_mut() else {
            return;
        };
        for (st, chain) in se.stacks.iter_mut().zip(converted) {
            let cur = st.steps[st.pos].normalised();
            st.steps.truncate(st.pos + 1);
            let mut last = cur;
            for sel in chain {
                let n = sel.normalised();
                // Keep only strictly-growing spans that still contain the
                // current selection, so a chain answered for a slightly
                // different anchor can't shrink or jump the gesture.
                if n.0 <= last.0 && n.1 >= last.1 && n != last {
                    st.steps.push(sel);
                    last = n;
                }
            }
        }
    }

    /// Grow every cursor's selection to the nearest strictly-larger
    /// tree-sitter node (#254's serverless fallback). Without a grammar,
    /// grows to the line span and then the whole buffer, so the command
    /// always answers. Returns false when nothing could grow.
    pub fn expand_selection_syntax(&mut self) -> bool {
        self.validate_expand_stacks();
        let tree = self.lang.map(crate::highlight::language_for).and_then(|l| {
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&l).ok()?;
            parser.parse(self.lines.join("\n"), None)
        });
        let Some(se) = self.select_expand.as_ref() else {
            return false;
        };
        let mut grown: Vec<Option<EditorSelection>> = Vec::with_capacity(se.stacks.len());
        for st in &se.stacks {
            let cur = st.steps[st.pos].normalised();
            let next = match &tree {
                Some(t) => self.syntax_enclosing_range(t, cur),
                None => self.textual_enclosing_range(cur),
            };
            grown.push(next);
        }
        let Some(se) = self.select_expand.as_mut() else {
            return false;
        };
        let mut any = false;
        for (st, next) in se.stacks.iter_mut().zip(grown) {
            if let Some(sel) = next {
                st.steps.truncate(st.pos + 1);
                st.steps.push(sel);
                st.pos += 1;
                any = true;
            }
        }
        if any {
            self.apply_expand_steps();
        }
        any
    }

    /// The smallest tree-sitter node range strictly containing `cur`
    /// (char coordinates), climbing `.parent()` past same-range wrappers.
    pub(super) fn syntax_enclosing_range(
        &self,
        tree: &tree_sitter::Tree,
        cur: ((usize, usize), (usize, usize)),
    ) -> Option<EditorSelection> {
        let start_b = self.char_pos_to_byte(cur.0);
        let end_b = self.char_pos_to_byte(cur.1);
        let mut node = tree.root_node().descendant_for_byte_range(start_b, end_b)?;
        loop {
            let (ns, ne) = (node.start_byte(), node.end_byte());
            let contains = ns <= start_b && ne >= end_b;
            let strictly = contains && (ns < start_b || ne > end_b);
            if strictly {
                let anchor = self.ts_point_to_char(node.start_position());
                let head = self.ts_point_to_char(node.end_position());
                return Some(EditorSelection { anchor, head });
            }
            node = node.parent()?;
        }
    }

    /// Grammar-less growth: the current line span, then the whole buffer.
    pub(super) fn textual_enclosing_range(
        &self,
        cur: ((usize, usize), (usize, usize)),
    ) -> Option<EditorSelection> {
        let (start, end) = cur;
        let line_sel = EditorSelection {
            anchor: (start.0, 0),
            head: (end.0, self.line_char_len(end.0)),
        };
        let n = line_sel.normalised();
        if n.0 < start || n.1 > end {
            return Some(line_sel);
        }
        let last = self.lines.len().saturating_sub(1);
        let all = EditorSelection {
            anchor: (0, 0),
            head: (last, self.line_char_len(last)),
        };
        let n = all.normalised();
        (n.0 < start || n.1 > end).then_some(all)
    }

    /// Byte offset of char position `(row, col)` in the `\n`-joined text.
    pub(super) fn char_pos_to_byte(&self, pos: (usize, usize)) -> usize {
        let mut off = 0usize;
        for (i, l) in self.lines.iter().enumerate() {
            if i == pos.0 {
                return off + char_byte(l, pos.1.min(l.chars().count()));
            }
            off += l.len() + 1;
        }
        off.saturating_sub(1)
    }

    /// Tree-sitter `Point` (row + BYTE column) → editor `(row, char col)`.
    pub(super) fn ts_point_to_char(&self, p: tree_sitter::Point) -> (usize, usize) {
        let row = p.row.min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        let col = line[..p.column.min(line.len())].chars().count();
        (row, col)
    }

    /// The cursors' positions as LSP UTF-16 `(line, character)` pairs,
    /// primary first — `textDocument/selectionRange`'s `positions` input.
    pub fn cursor_positions_utf16(&self) -> Vec<(u32, u32)> {
        self.cursor_selections()
            .iter()
            .map(|s| {
                let (row, col) = s.head;
                let row = row.min(self.lines.len().saturating_sub(1));
                let line = &self.lines[row];
                let byte = char_byte(line, col.min(line.chars().count()));
                let u16col = line[..byte].encode_utf16().count() as u32;
                (row as u32, u16col)
            })
            .collect()
    }

    pub fn select_all(&mut self) {
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let last_row = self.lines.len() - 1;
        let last_col = self.line_char_len(last_row);
        self.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (last_row, last_col),
        });
        self.cursor_row = last_row;
        self.cursor_col = last_col;
    }

    /// Extract the selection text (`\n`-joined across rows) using the
    /// editor's char-indexed coordinates.  Returns "" when there's no
    /// selection or the selection is zero-area.
    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        if !sel.has_area() {
            return String::new();
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            return line[from..to].to_string();
        }
        let mut out = String::new();
        // first row: from sc to end of line
        let first = &self.lines[sr];
        let from = char_byte(first, sc);
        out.push_str(&first[from..]);
        out.push('\n');
        // full middle rows
        for r in (sr + 1)..er {
            out.push_str(&self.lines[r]);
            out.push('\n');
        }
        // last row: from start to ec
        let last = &self.lines[er];
        let to = char_byte(last, ec);
        out.push_str(&last[..to]);
        out
    }

    /// The literal text to light up as "other occurrences" of the current
    /// selection (VS Code's selection highlight). Returns `None` when the
    /// selection shouldn't drive occurrence highlighting at all. Computed once
    /// per render and fed to `paint_selection_occurrences` for every visible
    /// line.
    pub(super) fn selection_occurrence_needle(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr != er {
            // VS Code only highlights occurrences for single-line selections.
            return None;
        }
        let line = &self.lines[sr];
        let text = &line[char_byte(line, sc)..char_byte(line, ec)];
        // Mirror VS Code's non-empty-selection branch: skip whitespace-only
        // selections and anything past the 200-char ceiling
        // (`editor.selectionHighlightMaxLength`); otherwise highlight the
        // literal substring. No minimum length, no whole-word filter.
        if text.trim().is_empty() || text.chars().count() > SELECTION_HIGHLIGHT_MAX_LEN {
            return None;
        }
        Some(text.to_string())
    }

    /// Delete the current selection if it has area.  Returns true iff content
    /// was removed.  Cursor lands at the start of the deleted range and the
    /// selection is cleared.  Pushes an undo step.
    pub fn delete_selection(&mut self) -> bool {
        if !self.selection.map(|s| s.has_area()).unwrap_or(false) {
            self.selection = None;
            return false;
        }
        self.push_undo(EditKind::DeleteSelection);
        let removed = self.delete_selection_inner();
        if removed {
            self.recompute_highlights();
        }
        removed
    }

    /// Same as `delete_selection` but does NOT push an undo step or
    /// recompute highlights — used by other public mutators that have
    /// already snapshotted state and will recompute themselves.
    pub(super) fn delete_selection_inner(&mut self) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        if !sel.has_area() {
            self.selection = None;
            return false;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &mut self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            line.replace_range(from..to, "");
        } else {
            let last = self.lines.remove(er);
            for _ in (sr + 1)..er {
                self.lines.remove(sr + 1);
            }
            let first = &mut self.lines[sr];
            let from = char_byte(first, sc);
            first.truncate(from);
            let to = char_byte(&last, ec);
            first.push_str(&last[to..]);
        }
        self.cursor_row = sr;
        self.cursor_col = sc;
        self.selection = None;
        self.mark_buffer_changed();
        true
    }

    /// True when the active selection spans more than one row, so Tab should
    /// indent the whole block (VS Code) rather than replace the selection
    /// with a single indentation unit.
    pub fn selection_is_multiline(&self) -> bool {
        match &self.selection {
            Some(sel) => {
                let (start, end) = sel.normalised();
                start.0 != end.0
            }
            None => false,
        }
    }

    /// Double-click word selection: place the cursor at the click point, then
    /// select the maximal run of word characters covering it and leave the
    /// caret at the right edge of that run (VS Code parity). A click on
    /// whitespace or past the end of an empty line clears any selection.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        self.click(col, row);
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let r = self.cursor_row;
        let chars: Vec<char> = self.lines[r].chars().collect();
        let c = self.cursor_col;
        let pivot = if c < chars.len() && is_word_char(chars[c]) {
            Some(c)
        } else if c == chars.len() && c > 0 && is_word_char(chars[c - 1]) {
            Some(c - 1)
        } else {
            None
        };
        let Some(p) = pivot else {
            self.selection = None;
            return;
        };
        let mut start = p;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = p + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        self.cursor_col = end;
        self.selection = Some(EditorSelection {
            anchor: (r, start),
            head: (r, end),
        });
        self.last_edit_kind = None;
    }

    /// Lines `from` through `to` as one whole-line selection (#1570): from
    /// the start of the top line to the start of the line after the bottom
    /// one, or to the end of the buffer's last line, which has none. With
    /// `to` above `from` it runs upward, the caret at the top.
    pub(super) fn whole_lines(&self, from: usize, to: usize) -> EditorSelection {
        let last = self.lines.len().saturating_sub(1);
        let (top, bottom) = (from.min(to).min(last), from.max(to).min(last));
        let start = (top, 0);
        let end = if bottom < last {
            (bottom + 1, 0)
        } else {
            (last, self.line_char_len(last))
        };
        let (anchor, head) = if to < from {
            (end, start)
        } else {
            (start, end)
        };
        EditorSelection { anchor, head }
    }

    /// Whole-line selection by mouse (#1570), as VS Code's triple-click and
    /// line-number click make: select from line `anchor` (the line under the
    /// pointer when `None`) through the line under screen `(col, row)`, and
    /// return the anchor so a drag can keep extending from it.
    pub fn select_lines_at(&mut self, anchor: Option<usize>, col: u16, row: u16) -> usize {
        self.click(col, row);
        let anchor = anchor.unwrap_or(self.cursor_row);
        self.set_selections(&[self.whole_lines(anchor, self.cursor_row)]);
        anchor
    }

    /// VS Code "Expand Line Selection" (#1570): every cursor's selection
    /// grows to the whole lines it touches, through the start of the next
    /// line. A selection that already ends at a line start reaches one line
    /// further, so each press adds a line.
    pub fn expand_line_selection(&mut self) {
        let all: Vec<EditorSelection> = self
            .selections_with_primary()
            .iter()
            .map(|s| {
                let (start, end) = s.normalised();
                self.whole_lines(start.0, end.0)
            })
            .collect();
        self.set_selections(&all);
        self.last_edit_kind = None;
    }
}
