//! Whole-line commands: move, duplicate, delete, join and sort lines, indent and dedent,
//! trim whitespace, replace in place, and converting indentation.

use super::*;

impl Editor {
    pub fn yank_lines(&self, count: usize) -> String {
        let n = count.max(1);
        let start = self.cursor_row;
        self.lines_slice_text(start, n)
    }

    pub fn delete_lines(&mut self, count: usize) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.push_undo(EditKind::DeleteLines);
        self.clear_selection();
        let n = count.max(1);
        // On a symbol tab the lines deleted stay inside the symbol: a count
        // stops at its last line, and a caret above it starts at its first.
        let (first, limit) = self.symbol_clip().unwrap_or((0, self.lines.len()));
        let start = self.cursor_row.max(first);
        let end = (start + n).min(limit).max(start + 1).min(self.lines.len());
        let yanked = self.lines_slice_text(start, end - start);
        self.lines.drain(start..end);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = start.min(self.lines.len() - 1);
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        yanked
    }

    /// Delete every line touched by the primary cursor or selection and by
    /// each secondary caret, yanking the removed text. VS Code's
    /// `editor.action.deleteLines` works across all cursors, and `Cmd+D`
    /// exists to make them: deleting only the primary's line would strand the
    /// others, still painted on rows that moved out from under them.
    pub fn delete_caret_lines(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.push_undo(EditKind::DeleteLines);
        let last = self.lines.len() - 1;
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        // Mark while collecting, then scan once: the rows come out sorted and
        // deduped without a sort, and Change All Occurrences (Cmd+F2) can put
        // a caret on every match in the file.
        let mut doomed = vec![false; self.lines.len()];
        for sel in std::iter::once(primary).chain(self.carets.iter().copied()) {
            let (a, h) = sel.normalised();
            for slot in &mut doomed[a.0.min(last)..=h.0.min(last)] {
                *slot = true;
            }
        }
        let rows: Vec<usize> = doomed
            .iter()
            .enumerate()
            .filter_map(|(r, &drop)| drop.then_some(r))
            .collect();
        let yanked = rows
            .iter()
            .map(|&r| self.lines[r].as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // One retain pass, not a `remove` per row: removing tens of thousands
        // of rows one at a time is quadratic — a multi-second freeze on a large
        // file, on the render thread.
        let mut i = 0;
        self.lines.retain(|_| {
            let keep = !doomed[i];
            i += 1;
            keep
        });
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.selection = None;
        self.carets.clear();
        self.cursor_row = rows[0].min(self.lines.len() - 1);
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        yanked
    }

    pub fn kill_to_bol(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.clear_selection();
        let row = self.cursor_row;
        if self.cursor_col == 0 {
            return String::new();
        }
        self.push_undo(EditKind::KillToBol);
        let from = self.byte_index(row, 0);
        let to = self.byte_index(row, self.cursor_col);
        let killed = self.lines[row][from..to].to_string();
        self.lines[row].replace_range(from..to, "");
        self.cursor_col = 0;
        self.mark_buffer_changed();
        self.recompute_highlights();
        killed
    }

    pub fn open_line_below(&mut self) {
        self.pin_on_edit();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.push_undo(EditKind::OpenLine);
        self.clear_selection();
        let row = self.cursor_row;
        let indent: String = self.lines[row]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.lines.insert(row + 1, indent.clone());
        self.cursor_row = row + 1;
        self.cursor_col = indent.chars().count();
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    pub fn open_line_above(&mut self) {
        self.pin_on_edit();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.push_undo(EditKind::OpenLine);
        self.clear_selection();
        let row = self.cursor_row;
        let indent: String = self.lines[row]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        self.lines.insert(row, indent.clone());
        self.cursor_col = indent.chars().count();
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    pub fn kill_to_eol(&mut self) -> String {
        self.pin_on_edit();
        if self.lines.is_empty() {
            return String::new();
        }
        self.clear_selection();
        let row = self.cursor_row;
        let line_len = self.line_char_len(row);
        if self.cursor_col >= line_len {
            return String::new();
        }
        self.push_undo(EditKind::KillToEol);
        let from = self.byte_index(row, self.cursor_col);
        let to = self.byte_index(row, line_len);
        let killed = self.lines[row][from..to].to_string();
        self.lines[row].replace_range(from..to, "");
        self.mark_buffer_changed();
        self.recompute_highlights();
        killed
    }

    /// Splice `text` over the `len_chars` characters starting at
    /// `(row, col_chars)` — the find bar's Replace. One undo step; the
    /// cursor lands at the end of the inserted text so replace-and-advance
    /// continues from there.
    pub fn replace_find_match(
        &mut self,
        row: usize,
        col_chars: usize,
        len_chars: usize,
        text: &str,
    ) {
        if row >= self.lines.len() {
            return;
        }
        let line_len = self.line_char_len(row);
        if col_chars.saturating_add(len_chars) > line_len {
            return;
        }
        self.undoable_edit(EditKind::Replace, |this| {
            this.clear_selection();
            let from = this.byte_index(row, col_chars);
            let to = this.byte_index(row, col_chars + len_chars);
            let text = &normalize_newlines(text);
            let breaks = text.matches('\n').count();
            if breaks == 0 {
                this.lines[row].replace_range(from..to, text);
                this.cursor_row = row;
                this.cursor_col = col_chars + text.chars().count();
            } else {
                // A replacement newline (VS Code's regex `\n`) becomes a real
                // line break, never an embedded control char in one line.
                let line = std::mem::take(&mut this.lines[row]);
                let merged = format!("{}{}{}", &line[..from], text, &line[to..]);
                this.lines
                    .splice(row..=row, merged.split('\n').map(str::to_string));
                this.cursor_row = row + breaks;
                this.cursor_col = text
                    .rsplit('\n')
                    .next()
                    .map_or(0, |tail| tail.chars().count());
            }
        });
    }

    /// Swap in a whole new buffer as one undo step: the find bar's Replace
    /// All, a Search Editor rerun, an agent's or a runner's rewrite. The
    /// cursor clamps to the new buffer's bounds, and the selection and any
    /// extra carets go: they point into the old text, and one left on a row
    /// the new text no longer has crashes the next frame (#2074).
    pub fn replace_all_lines(&mut self, new_lines: Vec<String>) {
        self.undoable_edit(EditKind::Replace, |this| {
            this.clear_selection();
            this.collapse_carets();
            this.lines = new_lines;
            if this.lines.is_empty() {
                this.lines.push(String::new());
            }
            this.cursor_row = this.cursor_row.min(this.lines.len() - 1);
            this.cursor_col = this.cursor_col.min(this.line_char_len(this.cursor_row));
        });
    }

    pub(super) fn lines_slice_text(&self, start: usize, count: usize) -> String {
        if count == 0 || start >= self.lines.len() {
            return String::new();
        }
        let end = (start + count).min(self.lines.len());
        let mut out = self.lines[start..end].join("\n");
        out.push('\n');
        out
    }

    /// VS Code's "Copy Line Down" (Shift+Option+Down on macOS). Duplicates
    /// the current line — or, when a selection is active, every line the
    /// selection touches as one block — directly below the original, then
    /// moves the cursor (and the selection, if any) onto the duplicate so
    /// the next keystroke acts on the copy. With several carets every
    /// caret's lines are copied (#1467); carets on touching lines share one
    /// block. Recorded as a single undo step via the dedicated
    /// `EditKind::DuplicateLines`.
    pub fn duplicate_lines_down(&mut self) {
        self.push_undo(EditKind::DuplicateLines);
        let blocks = self.selected_row_blocks_over_folds();
        for &(start, end) in blocks.iter().rev() {
            let block: Vec<String> = self.lines[start..=end].to_vec();
            self.lines.splice(end + 1..end + 1, block);
        }
        // A caret moves down by every block copied at or above it, its own
        // block's copy included.
        self.remap_carets(|(r, c)| (r + rows_in_blocks(&blocks, |b| b.0 <= r), c));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Copy Line Up" (Shift+Option+Up on macOS). Mirror of
    /// `duplicate_lines_down`: inserts the copy *above* the original
    /// block, leaving the cursor on the upper copy at the same row index
    /// it started at (the original is pushed down by `block_len`).
    pub fn duplicate_lines_up(&mut self) {
        self.push_undo(EditKind::DuplicateLines);
        let blocks = self.selected_row_blocks_over_folds();
        for &(start, end) in blocks.iter().rev() {
            let block: Vec<String> = self.lines[start..=end].to_vec();
            self.lines.splice(start..start, block);
        }
        // Only the blocks wholly above a caret push it down: its own copy
        // goes in above it, onto the row it already has.
        self.remap_carets(|(r, c)| (r + rows_in_blocks(&blocks, |b| b.1 < r), c));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    pub(super) fn selected_or_cursor_row_range(&self) -> (usize, usize) {
        match &self.selection {
            Some(sel) => {
                let (start, end) = sel.normalised();
                (start.0, end.0)
            }
            None => (self.cursor_row, self.cursor_row),
        }
    }

    /// The rows every selection or caret touches, as sorted `(first, last)`
    /// blocks. Blocks that overlap or touch are one block, so a line
    /// command never edits a row twice or moves one block into another.
    pub(super) fn selected_row_blocks(&self) -> Vec<(usize, usize)> {
        let last = self.lines.len().saturating_sub(1);
        let rows = self
            .selections_with_primary()
            .iter()
            .map(|s| {
                let (a, b) = s.normalised();
                (a.0.min(last), b.0.min(last))
            })
            .collect();
        merge_row_blocks(rows, true)
    }

    /// [`Self::selected_row_blocks`], with a block that ends on a folded
    /// header widened over the lines the fold hides, so Move Line and Copy
    /// Line take a folded region whole instead of its header alone (#1611).
    pub(super) fn selected_row_blocks_over_folds(&self) -> Vec<(usize, usize)> {
        let rows = self
            .selected_row_blocks()
            .into_iter()
            .map(|(start, end)| (start, self.folded_end(end).unwrap_or(end)))
            .collect();
        merge_row_blocks(rows, true)
    }

    /// Char-based slice of the buffer between two `(row, char_col)` points
    /// (`start <= end`), joining spanned rows with `\n`.
    pub(super) fn char_range_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        if start.0 == end.0 {
            return self.lines[start.0]
                .chars()
                .skip(start.1)
                .take(end.1.saturating_sub(start.1))
                .collect();
        }
        let mut out: String = self.lines[start.0].chars().skip(start.1).collect();
        for line in &self.lines[start.0 + 1..end.0] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        let last: String = self.lines[end.0].chars().take(end.1).collect();
        out.push_str(&last);
        out
    }

    /// Replace the char-range `start..end` with `new`, re-splitting on `\n`
    /// so a replacement that adds or removes newlines reshapes `lines`.
    pub(super) fn replace_char_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        new: &str,
    ) {
        let prefix: String = self.lines[start.0].chars().take(start.1).collect();
        let suffix: String = self.lines[end.0].chars().skip(end.1).collect();
        let combined = format!("{prefix}{new}{suffix}");
        let replacement: Vec<String> = combined.split('\n').map(str::to_string).collect();
        self.lines.splice(start.0..=end.0, replacement);
    }

    /// VS Code "Move Line Down" (Alt+Down): swap the current line / selected
    /// block with the line below it, carrying the cursor and selection along.
    /// With several carets every caret's block moves. No-op when a block
    /// already touches the last line: all of them move or none does.
    ///
    /// A folded region moves as one line and stays folded (#1611): a block
    /// on a folded header takes the hidden lines with it, and a block above
    /// a folded header moves below its whole region, never into its body.
    pub fn move_lines_down(&mut self) {
        let blocks = self.selected_row_blocks_over_folds();
        // What each block swaps with: the line below it, or all of a folded
        // region that starts there.
        let below: Vec<(usize, usize)> = blocks
            .iter()
            .map(|&(_, end)| (end + 1, self.folded_end(end + 1).unwrap_or(end + 1)))
            .collect();
        // A symbol tab's block stops at the symbol's last line.
        let limit = self.symbol_clip().map_or(self.lines.len(), |(_, e)| e);
        if below.last().is_none_or(|&(_, last)| last >= limit)
            || below
                .iter()
                .zip(blocks.iter().skip(1))
                .any(|(b, next)| b.1 >= next.0)
        {
            return;
        }
        self.push_undo(EditKind::MoveLines);
        let folds = self.folded.clone();
        for (&(start, end), &(_, last)) in blocks.iter().zip(&below).rev() {
            self.lines[start..=last].rotate_right(last - end);
        }
        let map = |r: usize| {
            for (&(start, end), &(first, last)) in blocks.iter().zip(&below) {
                if (start..=end).contains(&r) {
                    return r + (last - end);
                }
                // The lines below a block swap up over it.
                if (first..=last).contains(&r) {
                    return r - (end - start + 1);
                }
            }
            r
        };
        self.remap_carets(|(r, c)| (map(r), c));
        self.mark_buffer_changed();
        self.move_folds(folds, map);
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Move Line Up" (Alt+Up): mirror of `move_lines_down`. No-op
    /// when a block already touches the first line. A block below a folded
    /// region moves above all of it (#1611).
    pub fn move_lines_up(&mut self) {
        let blocks = self.selected_row_blocks_over_folds();
        let first = self.symbol_clip().map_or(0, |(first, _)| first);
        if blocks.first().is_none_or(|&(start, _)| start <= first) {
            return;
        }
        // What each block swaps with: the line above it, or all of the
        // folded region whose hidden lines end there.
        let above: Vec<(usize, usize)> = blocks
            .iter()
            .map(|&(start, _)| {
                let row = start - 1;
                (self.folded_header_hiding(row).unwrap_or(row), row)
            })
            .collect();
        if above.first().is_some_and(|&(top, _)| top < first)
            || above
                .iter()
                .skip(1)
                .zip(&blocks)
                .any(|(a, prev)| a.0 <= prev.1)
        {
            return;
        }
        self.push_undo(EditKind::MoveLines);
        let folds = self.folded.clone();
        for (&(start, end), &(top, _)) in blocks.iter().zip(&above) {
            self.lines[top..=end].rotate_left(start - top);
        }
        let map = |r: usize| {
            for (&(start, end), &(top, bottom)) in blocks.iter().zip(&above) {
                if (start..=end).contains(&r) {
                    return r - (start - top);
                }
                // The lines above a block swap down under it.
                if (top..=bottom).contains(&r) {
                    return r + (end - start + 1);
                }
            }
            r
        };
        self.remap_carets(|(r, c)| (map(r), c));
        self.mark_buffer_changed();
        self.move_folds(folds, map);
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Join Lines": collapse the selected lines (or the current line
    /// with the one below it) into one, trimming each joined line's leading
    /// whitespace and separating with a single space. Every extra caret
    /// joins its own lines too. No-op when no caret has a following line to
    /// join.
    pub fn join_lines(&mut self) {
        // A symbol tab's last line has nothing below it to join.
        let limit = self.symbol_clip().map_or(self.lines.len(), |(_, e)| e);
        // (first row, last row, primary, caret) for each selection.
        let mut items: Vec<(usize, usize, bool, (usize, usize))> = self
            .selections_with_primary()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (a, b) = s.normalised();
                let last = if a.0 == b.0 { a.0 + 1 } else { b.0 };
                (a.0, last, i == 0, s.head)
            })
            .collect();
        items.sort_unstable();
        let mut merged: Vec<(usize, usize, bool, (usize, usize))> = Vec::new();
        for (start, last, primary, head) in items {
            match merged.last_mut() {
                Some(m) if start <= m.1 => {
                    m.1 = m.1.max(last);
                    m.2 |= primary;
                }
                _ => merged.push((start, last, primary, head)),
            }
        }
        if merged.iter().all(|&(_, last, _, _)| last >= limit) {
            return;
        }
        self.push_undo(EditKind::JoinLines);
        let mut placed: Vec<(bool, (usize, usize))> = Vec::with_capacity(merged.len());
        for &(start, last, primary, head) in merged.iter().rev() {
            // A caret with no line below it stays where it is.
            let caret = if last >= limit {
                head
            } else {
                let mut result = self.lines[start].trim_end().to_string();
                let cursor_col = result.chars().count();
                for line in &self.lines[start + 1..=last] {
                    let piece = line.trim_start();
                    if !result.is_empty()
                        && !result.ends_with(char::is_whitespace)
                        && !piece.is_empty()
                    {
                        result.push(' ');
                    }
                    result.push_str(piece);
                }
                self.lines.splice(start..=last, std::iter::once(result));
                (start, cursor_col)
            };
            placed.push((primary, self.pin_from_end(caret)));
        }
        self.place_pinned_carets(&placed);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Sort Lines Ascending/Descending": sort the selected lines
    /// lexicographically, or the whole buffer when nothing is selected. With
    /// several carets each selection's lines are sorted on their own. No-op
    /// on a single line.
    pub fn sort_lines(&mut self, ascending: bool) {
        let blocks = if self.carets.is_empty() {
            vec![match &self.selection {
                Some(sel) => {
                    let (s, e) = sel.normalised();
                    (s.0, e.0)
                }
                None => (0, self.lines.len().saturating_sub(1)),
            }]
        } else {
            let rows = self
                .selections_with_primary()
                .iter()
                .map(|s| {
                    let (a, b) = s.normalised();
                    (a.0, b.0)
                })
                .collect();
            merge_row_blocks(rows, false)
        };
        let blocks: Vec<(usize, usize)> = blocks.into_iter().filter(|&(s, e)| s < e).collect();
        if blocks.is_empty() {
            return;
        }
        self.push_undo(EditKind::SortLines);
        for &(start, end) in &blocks {
            let mut block: Vec<String> = self.lines[start..=end].to_vec();
            block.sort();
            if !ascending {
                block.reverse();
            }
            self.lines.splice(start..=end, block);
            self.forget_folds_in(start, end);
        }
        // The rows changed under the carets: a column past the new line's
        // end made the next word motion index out of bounds.
        self.remap_carets(|p| p);
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// VS Code "Trim Trailing Whitespace": strip trailing spaces and tabs from
    /// every line, clamping the cursor onto the new line end. Returns false
    /// (a no-op) when the buffer is already clean.
    pub fn trim_trailing_whitespace(&mut self) -> bool {
        let any = self
            .lines
            .iter()
            .any(|l| l.trim_end_matches([' ', '\t']).len() != l.len());
        if !any {
            return false;
        }
        self.push_undo(EditKind::TrimWhitespace);
        self.carets.clear();
        for line in &mut self.lines {
            let trimmed_len = line.trim_end_matches([' ', '\t']).len();
            line.truncate(trimmed_len);
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// VS Code "Convert Indentation to Spaces" (`editor.action.indentationToSpaces`):
    /// replace every tab in each line's leading indentation with `tabSize`
    /// spaces. Only leading whitespace is touched.
    pub fn indentation_to_spaces(&mut self) {
        self.convert_indentation(true);
    }

    /// VS Code "Convert Indentation to Tabs" (`editor.action.indentationToTabs`):
    /// replace each run of `tabSize` spaces in the leading indentation with a
    /// tab, leaving any remainder. Only leading whitespace is touched.
    pub fn indentation_to_tabs(&mut self) {
        self.convert_indentation(false);
    }

    pub(super) fn convert_indentation(&mut self, to_spaces: bool) {
        let tab_w = (self.indent_style().width as usize).max(1);
        let spaces = " ".repeat(tab_w);
        let mut new_lines = self.lines.clone();
        let mut changed = false;
        for line in new_lines.iter_mut() {
            let indent_len = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let indent: String = line.chars().take(indent_len).collect();
            let converted = if to_spaces {
                indent.replace('\t', &spaces)
            } else {
                indent.replace(&spaces, "\t")
            };
            if converted != indent {
                let rest: String = line.chars().skip(indent_len).collect();
                *line = format!("{converted}{rest}");
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.push_undo(EditKind::IndentConvert);
        self.lines = new_lines;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// VS Code's `files.trimFinalNewlines`, surfaced as a command: drop trailing
    /// empty lines at the end of the buffer, always keeping at least one line.
    /// Returns false (a no-op) when there are no trailing blank lines.
    pub fn trim_final_newlines(&mut self) -> bool {
        let mut end = self.lines.len();
        while end > 1 && self.lines[end - 1].is_empty() {
            end -= 1;
        }
        if end == self.lines.len() {
            return false;
        }
        self.push_undo(EditKind::TrimFinalNewlines);
        self.lines.truncate(end);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.mark_buffer_changed();
        self.recompute_highlights();
        true
    }

    /// VS Code's plain-Tab indent (no multi-line selection): insert spaces up
    /// to the next tab stop from the cursor column, replacing any single-line
    /// selection first. With `editor.insertSpaces` + `editor.useTabStops`
    /// (both on by default) a Tab at column 2 with a width-4 unit adds 2
    /// spaces to reach column 4, not a flat 4.
    pub fn indent_at_cursor(&mut self) {
        self.undoable_edit(EditKind::Indent, |this| {
            if this.selection.is_some() {
                this.delete_selection_inner();
            }
            if this.lines.is_empty() {
                this.lines.push(String::new());
            }
            let style = this.indent_style();
            let row = this.cursor_row;
            let byte = this.byte_index(row, this.cursor_col);
            let (ins, advance): (String, usize) = if style.use_spaces {
                let unit_w = (style.width as usize).max(1);
                let pad = unit_w - (this.cursor_col % unit_w);
                (" ".repeat(pad), pad)
            } else {
                // A tab-indented buffer inserts one literal tab (useTabStops).
                ("\t".to_string(), 1)
            };
            this.lines[row].insert_str(byte, &ins);
            this.cursor_col += advance;
        });
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Indent Lines" (Tab with a multi-line selection): prepend one
    /// indentation unit to every line the selection touches. Empty lines are
    /// left untouched, matching VS Code's "do not indent empty lines" rule
    /// for block indent. Cursor and selection columns shift right by the unit
    /// width on the lines that actually gained indentation so the same text
    /// stays highlighted.
    pub fn indent_lines(&mut self) {
        self.undoable_edit(EditKind::Indent, |this| {
            let unit = this.indent_unit();
            let unit_w = unit.chars().count();
            let (start_row, end_row) = this.selected_or_cursor_row_range();
            for row in start_row..=end_row {
                if !this.lines[row].is_empty() {
                    this.lines[row].insert_str(0, &unit);
                }
            }
            // A row gained indentation iff it was non-empty (empty lines were
            // skipped and stay empty), so a row that is non-empty now is exactly
            // one that shifted right by `unit_w`.
            let shift = |row: usize, col: usize, lines: &[String]| -> usize {
                if row >= start_row && row <= end_row && !lines[row].is_empty() {
                    col + unit_w
                } else {
                    col
                }
            };
            this.cursor_col = shift(this.cursor_row, this.cursor_col, &this.lines);
            if let Some(mut sel) = this.selection {
                sel.anchor.1 = shift(sel.anchor.0, sel.anchor.1, &this.lines);
                sel.head.1 = shift(sel.head.0, sel.head.1, &this.lines);
                this.selection = Some(sel);
            }
        });
        self.ensure_cursor_col_visible();
    }

    /// VS Code "Outdent" (Shift+Tab): strip one indentation level from every
    /// line the selection touches, or just the current line when there is no
    /// selection. Removal is tab-stop aligned (the `useTabStops` default): a
    /// line indented 6 spaces drops to 4, a line indented 3 drops to 0. A
    /// single leading tab counts as one level. Cursor and selection columns
    /// follow the removal on their own row.
    pub fn dedent_lines(&mut self) {
        self.pin_on_edit();
        self.push_undo(EditKind::Indent);
        let unit_w = (self.indent_style().width as usize).max(1);
        let (start_row, end_row) = self.selected_or_cursor_row_range();
        let mut removed = vec![0usize; self.lines.len()];
        let mut any = false;
        for (row, slot) in removed
            .iter_mut()
            .enumerate()
            .take(end_row + 1)
            .skip(start_row)
        {
            let n = leading_outdent_chars(&self.lines[row], unit_w);
            if n > 0 {
                self.lines[row].replace_range(0..n, "");
                *slot = n;
                any = true;
            }
        }
        let pull = |row: usize, col: usize, removed: &[usize]| -> usize {
            if row < removed.len() {
                col.saturating_sub(removed[row])
            } else {
                col
            }
        };
        self.cursor_col = pull(self.cursor_row, self.cursor_col, &removed);
        if let Some(mut sel) = self.selection {
            sel.anchor.1 = pull(sel.anchor.0, sel.anchor.1, &removed);
            sel.head.1 = pull(sel.head.0, sel.head.1, &removed);
            self.selection = Some(sel);
        }
        if any {
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        self.ensure_cursor_col_visible();
    }
}

/// Sort `(first, last)` row blocks and merge the ones that overlap, and
/// with `touching` also the ones that meet end to end.
pub(super) fn merge_row_blocks(
    mut blocks: Vec<(usize, usize)>,
    touching: bool,
) -> Vec<(usize, usize)> {
    blocks.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(blocks.len());
    for (start, end) in blocks {
        match out.last_mut() {
            Some(last) if start <= last.1 + usize::from(touching) => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// How many rows the blocks that `keep` picks span.
pub(super) fn rows_in_blocks(
    blocks: &[(usize, usize)],
    keep: impl Fn(&(usize, usize)) -> bool,
) -> usize {
    blocks
        .iter()
        .filter(|b| keep(b))
        .map(|&(start, end)| end - start + 1)
        .sum()
}

/// Where a caret at char column `col` lands after `delta` chars are
/// inserted (positive) or removed (negative) at column `at` of its line.
pub(super) fn shift_col(col: usize, at: usize, delta: isize) -> usize {
    let n = delta.unsigned_abs();
    if delta >= 0 {
        if col >= at { col + n } else { col }
    } else if col >= at + n {
        col - n
    } else {
        col.min(at)
    }
}

/// The position just after `text` when it is written at `start`.
pub(super) fn text_end(start: (usize, usize), text: &str) -> (usize, usize) {
    match text.rsplit_once('\n') {
        Some((head, tail)) => (
            start.0 + head.matches('\n').count() + 1,
            tail.chars().count(),
        ),
        None => (start.0, start.1 + text.chars().count()),
    }
}
