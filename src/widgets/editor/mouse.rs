//! The mouse in the editor: clicks, double and triple clicks, drags, and what lies under
//! the pointer (a word, a diagnostic, a hover region).

use super::*;

impl Editor {
    /// Mouse-down: position the cursor at the click point and start a
    /// fresh zero-area selection there. A subsequent drag widens it.
    pub fn mouse_down(&mut self, col: u16, row: u16) {
        self.click(col, row);
        self.start_selection_at_cursor();
    }

    /// Mouse-drag: move the cursor to the drag point and extend the selection
    /// head to the new cursor.  Anchors at the current cursor if no prior
    /// selection exists.
    pub fn mouse_drag(&mut self, col: u16, row: u16) {
        if self.selection.is_none() {
            self.start_selection_at_cursor();
        }
        self.click(col, row);
        self.extend_selection_to_cursor();
    }

    /// Move the cursor to the screen coordinates (col, row). Used for mouse clicks.
    pub fn click(&mut self, col: u16, row: u16) {
        if self.lines.is_empty() || self.last_inner.height == 0 {
            return;
        }
        if row < self.last_inner.y || row >= self.last_inner.y + self.last_inner.height {
            return;
        }
        let row_idx = (row - self.last_inner.y) as usize;
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        // Gutter clicks land on the row's first column (saturates to 0).
        let visible = col.saturating_sub(text_x) as usize;
        // Map the screen row through the last render's visual-row layout (the
        // same map `buffer_pos_at` reads) so wrapped and folded lines resolve
        // to the line actually shown on that row; the linear map drifts by one
        // for every wrapped continuation row above the pointer. A press below
        // the last visual row clamps to it. The map is empty only before the
        // first paint, when nothing can be wrapped or folded and the linear
        // map is exact.
        // A press on a comment-box row leaves the caret alone (the App
        // intercepts box clicks; this is the belt-and-braces path). A press
        // below the last visual row clamps to the nearest text row above.
        let clamped = row_idx.min(self.last_wrap_rows.len().saturating_sub(1));
        let nearest_text = (0..=clamped)
            .rev()
            .find_map(|i| self.text_row(i).map(|t| (i, t)));
        if !self.last_wrap_rows.is_empty()
            && row_idx < self.last_wrap_rows.len()
            && self.text_row(row_idx).is_none()
        {
            return;
        }
        // `start + visible` is a DISPLAY column, so inlay hints spliced into
        // the row have to be mapped back out of it — the same inversion
        // `buffer_pos_at` (and therefore hover) applies. Without this the
        // caret lands `hint` cells right of the pointer.
        let (target_line, target_col) = match nearest_text {
            Some((_, (line, start, end))) => {
                // In wrap mode a drag past a segment's right edge stops at the
                // segment end; non-wrap keeps the click-to-line-end behaviour.
                let cap = if self.wrap_enabled() { end } else { usize::MAX };
                (
                    line,
                    self.buffer_col_at_display(line, start, visible).min(cap),
                )
            }
            None => {
                let line = (self.scroll + row_idx).min(self.lines.len().saturating_sub(1));
                (
                    line,
                    self.buffer_col_at_display(line, self.scroll_col, visible),
                )
            }
        };
        self.cursor_row = target_line;
        self.cursor_col = floor_grapheme_col(&self.lines[target_line], target_col);
        self.last_edit_kind = None;
        // A click is a deliberate reposition; any multi-cursor session ends.
        self.carets.clear();
    }

    pub fn buffer_pos_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        if self.last_inner.height == 0 || self.lines.is_empty() || row < self.last_inner.y {
            return None;
        }
        self.sync_tab_cells();
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if self.wrap_enabled() {
            // Map the screen row through the last render's visual-row layout so
            // a click lands on the right logical line/column even when wrapped.
            // The blank tail of a folded row maps to the segment end, letting
            // you click to end-of-visual-line.
            let (line, start, end) = self.text_row((row - self.last_inner.y) as usize)?;
            if col < text_x {
                return None;
            }
            let visible_col = (col - text_x) as usize;
            let char_col = self
                .buffer_col_at_display(line, start, visible_col)
                .min(end)
                .min(self.line_char_len(line));
            return Some((line, char_col));
        }
        // Non-wrap: map through the painted layout (comment boxes shift the
        // rows below them); before the first paint the linear map is exact.
        if row >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let line = if self.last_wrap_rows.is_empty() {
            self.scroll + (row - self.last_inner.y) as usize
        } else {
            self.text_row((row - self.last_inner.y) as usize)?.0
        };
        if line >= self.lines.len() || col < text_x {
            return None;
        }
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        let visible_col = (col - text_x) as usize;
        if visible_col >= text_width as usize {
            return None;
        }
        Some((
            line,
            self.buffer_col_at_display(line, self.scroll_col, visible_col),
        ))
    }

    /// Invert the row's display map: the first buffer column of `line`
    /// (counting from `base`, the column the row starts at) whose first cell
    /// reaches `visible_cells` cells right of the row's left edge. Cells are
    /// as painted: a tab takes its Tab Size, a wide character two, and a
    /// point landing inside an inlay hint's own cells resolves to its anchor
    /// column, so the caret snaps beside the code the hint annotates.
    pub(super) fn buffer_col_at_display(
        &self,
        line: usize,
        base: usize,
        visible_cells: usize,
    ) -> usize {
        self.sync_tab_cells();
        let line_len = self.line_char_len(line);
        let base = base.min(line_len);
        let text = self.lines.get(line).map_or("", String::as_str);
        let mut row = RowCells::default();
        row.fill(
            &text[byte_index_of_char(text, base)..],
            base,
            self.row_origin(text, base),
            self.row_inlay_spans(line)
                .iter()
                .filter(|(hc, _, _)| *hc >= base)
                .map(|(hc, l, _)| (*hc, l.chars().count())),
        );
        let mut c = base;
        while c < line_len && row.cell(c) < visible_cells {
            c += 1;
        }
        c
    }

    pub fn word_at(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        if col >= chars.len() || !is_word_char(chars[col]) {
            return None;
        }
        let mut start = col;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        Some((start, end))
    }

    /// Like [`Self::word_at`], but over the identifier runs the naming-style
    /// transforms rewrite: letters, digits, `_` and `-` (#1573). A `-` joins
    /// the run only between a letter or digit and a letter, so the minus in
    /// `total-5` or `-count` stays an operator.
    pub(super) fn identifier_run_at(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        let ident = |i: usize| {
            let c = chars[i];
            c.is_alphanumeric()
                || c == '_'
                || (c == '-'
                    && i > 0
                    && chars[i - 1].is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|n| n.is_alphabetic()))
        };
        if col >= chars.len() || !ident(col) {
            return None;
        }
        let mut start = col;
        while start > 0 && ident(start - 1) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && ident(end) {
            end += 1;
        }
        Some((start, end))
    }

    /// The identifier text under `(line, col)`, or None over non-word
    /// characters. Used by debug hover-to-evaluate to name the variable.
    pub fn word_string_at(&self, line: usize, col: usize) -> Option<String> {
        let (start, end) = self.word_at(line, col)?;
        let chars: Vec<char> = self.lines.get(line)?.chars().collect();
        Some(chars[start..end].iter().collect())
    }

    /// Severities and messages of every diagnostic whose range covers the
    /// character position, each message followed by its rule as
    /// `source(code)` and the rule's link on a line of its own (#1540), then
    /// a line per related location (#1523), its file relative to `root`.
    /// The position is `(line, ch)`. Decoded on demand from the retained
    /// `diagnostics` (UTF-16 positions) rather than the per-line span cache so
    /// the messages don't have to be duplicated into the render-time cache.
    /// Only runs on a hover dwell, never per frame. A zero-width diagnostic is
    /// widened to one cell to match the underline, so a point diagnostic is
    /// still hoverable. Returns an empty vec when no squiggle is under the
    /// point (and when the retained batch belongs to another file).
    pub fn diagnostics_at(
        &self,
        line: usize,
        ch: usize,
        root: &Path,
    ) -> Vec<(crate::lsp::manager::DiagnosticSeverity, String)> {
        if self.diagnostics_path.as_deref() != self.path.as_deref() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for d in &self.diagnostics {
            let (start_line, start_char) = self.diagnostic_row(d.start_line, d.start_char);
            let (end_line, end_char) = self.diagnostic_row(d.end_line, d.end_char);
            if line < start_line || line > end_line {
                continue;
            }
            let Some(text) = self.lines.get(line) else {
                continue;
            };
            let from = if line == start_line {
                utf16_to_char_col(text, start_char)
            } else {
                0
            };
            let to = if line == end_line {
                utf16_to_char_col(text, end_char)
            } else {
                text.chars().count()
            };
            let to = to.max(from + 1);
            if ch >= from && ch < to {
                let mut text = message_with_code(d);
                for related in d.related() {
                    text.push('\n');
                    text.push_str(&related.label(root));
                }
                out.push((d.severity, text));
            }
        }
        out
    }

    /// The region a hover popup should be keyed to at `(line, ch)`: the word
    /// range when one is under the point, otherwise the span of the first
    /// diagnostic covering it (so a squiggle on punctuation, e.g. a `..`
    /// syntax error, still anchors and dismisses a hover). `None` when neither
    /// is present. Shared by `poll_hover`, `drain_lsp_hover`, and the
    /// mouse-move dismissal so all three agree on what the popup belongs to.
    pub fn hover_region_at(&self, line: usize, ch: usize) -> Option<(usize, usize, usize)> {
        if let Some((start, end)) = self.word_at(line, ch) {
            return Some((line, start, end));
        }
        if self.diagnostics_path.as_deref() != self.path.as_deref() {
            return None;
        }
        for d in &self.diagnostics {
            let (start_line, start_char) = self.diagnostic_row(d.start_line, d.start_char);
            let (end_line, end_char) = self.diagnostic_row(d.end_line, d.end_char);
            if line < start_line || line > end_line {
                continue;
            }
            let text = self.lines.get(line)?;
            let from = if line == start_line {
                utf16_to_char_col(text, start_char)
            } else {
                0
            };
            let to = if line == end_line {
                utf16_to_char_col(text, end_char)
            } else {
                text.chars().count()
            };
            let to = to.max(from + 1);
            if ch >= from && ch < to {
                return Some((line, from, to));
            }
        }
        None
    }
}
