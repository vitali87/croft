//! Aligning carets: a port of Mike Moore's "Cursor Align" VS Code extension
//! (`yo1dog/vscode-cursor-align` v2.1.1, `extension.js`), so the same gestures
//! produce the same text here as they do there.
//!
//! The model the extension uses, and this file follows:
//!
//! * one *block* per single-line selection, ordered by line then start char; a
//!   selection spanning lines is ignored;
//! * blocks are grouped into *table columns* by their order on their line — the
//!   first selection on a line joins column 0, the second column 1, and so on —
//!   so several selections on one line each get their own column;
//! * a block's *padding* is the text between the previous block's end on that
//!   line (0 for the first there) and its own start, measured in display cells;
//! * every block in a column takes that column's widest padding at its start,
//!   and then the column's widest selection at its end (or at its start for Pad
//!   Front), so every selection in a column starts and ends in the same cells;
//! * with tabs a column's padding and selection widths are rounded up to tab
//!   stops and the counts become tab characters — the extension's "extra tabs
//!   are necessary" rule;
//! * columns are laid out left to right, each column's width feeding the next,
//!   which is why padding a left column cannot push a right one out of line.

use super::*;

/// Where a column's extra selection length goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AlignPad {
    /// At the end: every selection in the column keeps its start and grows to
    /// the column's longest.
    End,
    /// At the start, so the selected text right-aligns into the column.
    Front,
}

/// One run of whitespace to insert.
struct Pad {
    row: usize,
    col: usize,
    text: String,
    /// Pad Front puts a column's padding and a selection's own length at the
    /// same spot; the padding has to end up left of the length, so it goes in
    /// second.
    front: bool,
}

/// One selection, with the numbers the column layout gives it.
struct Block {
    row: usize,
    start: usize,
    end: usize,
    /// Char column the block's padding starts at: the previous block's end on
    /// its line, or 0 when it is the first there.
    padding_start: usize,
    /// Display cells of padding this block brings to its column, and of the
    /// selection itself.
    padding_cols: usize,
    block_cols: usize,
    column: usize,
}

/// A column of blocks, and the widths every block in it is padded out to.
#[derive(Default)]
struct Column {
    pad_cols: usize,
    block_cols: usize,
}

/// Display cells a run of text takes, starting at `start_col`: a tab jumps to
/// the next stop of `width`, and every other character takes the cells the
/// renderer gives it, so `界` counts two and a combining mark none — the
/// extension could only count code points, but here the widths are known.
fn text_cols(text: &str, start_col: usize, width: usize) -> usize {
    let width = width.max(1);
    let mut cols = 0;
    for c in text.chars() {
        cols += if c == '\t' {
            width - (start_col + cols) % width
        } else {
            unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
        };
    }
    cols
}

impl Editor {
    /// Align Cursors (`Alt+A`): pad every column of selections with spaces so
    /// each selection starts and ends in the same cells as the rest of its
    /// column. One undo step; fewer than two blocks is a no-op.
    pub fn align_cursors(&mut self) -> bool {
        self.align_columns(AlignPad::End, None)
    }

    /// Align Cursors (Pad Front) (`Alt+F`): the same columns, but a column's
    /// extra selection length goes to the front too, right-aligning the text.
    pub fn align_cursors_pad_front(&mut self) -> bool {
        self.align_columns(AlignPad::Front, None)
    }

    /// Align Cursors Using Tabs (`Ctrl+Alt+A`): the same columns padded with
    /// tabs, with every start and end snapped to a tab stop of `tab_width`.
    pub fn align_cursors_using_tabs(&mut self, tab_width: usize) -> bool {
        self.align_columns(AlignPad::End, Some(tab_width))
    }

    /// Align Cursors Using Indentation Settings: the extension's "auto"
    /// variant — tabs when the buffer indents with them, spaces when not.
    pub fn align_cursors_using_auto(&mut self) -> bool {
        let style = self.indent_style();
        if style.use_spaces {
            self.align_columns(AlignPad::End, None)
        } else {
            self.align_columns(AlignPad::End, Some(style.width as usize))
        }
    }

    /// Align Line Last Cursors (`Ctrl+Alt+L`): spaces at the start of
    /// every affected line until the right-most selection end of all of them
    /// lines up — whole lines shift right, as the extension does.
    pub fn align_line_last_cursors(&mut self) -> bool {
        let width = self.indent_style().width as usize;
        let all = self.caret_selections();
        // The right-most selection end on each line, in char columns.
        let mut ends: Vec<(usize, usize)> = Vec::new();
        for sel in &all {
            let (start, end) = sel.normalised();
            if start.0 != end.0 {
                continue;
            }
            match ends.iter_mut().find(|(row, _)| *row == start.0) {
                Some(slot) => slot.1 = slot.1.max(end.1),
                None => ends.push((start.0, end.1)),
            }
        }
        if ends.len() < 2 {
            return false;
        }
        let cols: Vec<(usize, usize)> = ends
            .iter()
            .map(|&(row, end)| (row, text_cols(&self.text_between(row, 0, end), 0, width)))
            .collect();
        let target = cols.iter().map(|&(_, c)| c).max().unwrap_or(0);
        if target == 0 {
            return false;
        }
        // At column 0, exactly where the extension puts it.
        let pads: Vec<Pad> = cols
            .iter()
            .filter(|&&(_, c)| c < target)
            .map(|&(row, c)| Pad {
                row,
                col: 0,
                text: " ".repeat(target - c),
                front: false,
            })
            .collect();
        let heads: Vec<((usize, usize), bool)> = all
            .iter()
            .map(|s| (s.head, s.head == s.normalised().0))
            .collect();
        let applied = !pads.is_empty();
        self.apply_alignment(&pads, &heads);
        applied
    }

    /// The text of `row` between two char columns.
    fn text_between(&self, row: usize, from: usize, to: usize) -> String {
        self.lines
            .get(row)
            .map(|l| l.chars().skip(from).take(to.saturating_sub(from)).collect())
            .unwrap_or_default()
    }

    /// The column geometry every align command shares, in the extension's
    /// passes over the blocks.
    fn align_columns(&mut self, how: AlignPad, tabs: Option<usize>) -> bool {
        let width = tabs
            .unwrap_or_else(|| self.indent_style().width as usize)
            .max(1);

        // Pass 1: one block per single-line selection, in reading order.
        let mut blocks: Vec<Block> = Vec::new();
        for sel in self.caret_selections() {
            let (start, end) = sel.normalised();
            if start.0 != end.0 {
                continue;
            }
            blocks.push(Block {
                row: start.0,
                start: start.1,
                end: end.1,
                padding_start: 0,
                padding_cols: 0,
                block_cols: 0,
                column: 0,
            });
        }
        blocks.sort_by_key(|b| (b.row, b.start));
        if blocks.len() < 2 {
            return false;
        }

        // Group into columns: the nth selection on a line joins column n, and
        // its padding starts where the previous one on that line ended.
        let mut columns: Vec<Column> = Vec::new();
        let (mut cur_row, mut cur_char, mut index) = (blocks[0].row, 0usize, 0usize);
        for block in blocks.iter_mut() {
            if block.row != cur_row {
                cur_row = block.row;
                cur_char = 0;
                index = 0;
            }
            if index == columns.len() {
                columns.push(Column::default());
            }
            block.column = index;
            block.padding_start = cur_char;
            cur_char = block.end;
            index += 1;
        }

        // Passes 2 and 3: each column's padding and selection widths, left to
        // right, because a column's cells depend on the ones before it.
        let mut col_start = 0usize;
        for (c, column) in columns.iter_mut().enumerate() {
            for b in blocks.iter_mut().filter(|b| b.column == c) {
                let text = self.text_between(b.row, b.padding_start, b.start);
                b.padding_cols = text_cols(&text, col_start, width);
                column.pad_cols = column.pad_cols.max(b.padding_cols);
            }
            if tabs.is_some() {
                column.pad_cols = column.pad_cols.div_ceil(width) * width;
            }
            col_start += column.pad_cols;

            for b in blocks.iter_mut().filter(|b| b.column == c) {
                let text = self.text_between(b.row, b.start, b.end);
                b.block_cols = text_cols(&text, col_start, width);
                column.block_cols = column.block_cols.max(b.block_cols);
            }
            if tabs.is_some() {
                column.block_cols = column.block_cols.div_ceil(width) * width;
            }
            col_start += column.block_cols;
        }

        // Lay the whitespace down in reading order, so the counts stay the
        // extension's own.
        let unit = if tabs.is_some() { "\t" } else { " " };
        let mut pads: Vec<Pad> = Vec::new();
        for b in &blocks {
            let column = &columns[b.column];
            let mut add_pad = column.pad_cols - b.padding_cols;
            let mut add_block = column.block_cols - b.block_cols;
            if tabs.is_some() {
                // These counts are cells; one tab covers a whole stop of them.
                add_pad = add_pad.div_ceil(width);
                add_block = add_block.div_ceil(width);
            }
            if add_pad > 0 {
                pads.push(Pad {
                    row: b.row,
                    col: b.start,
                    text: unit.repeat(add_pad),
                    front: true,
                });
            }
            if add_block > 0 {
                pads.push(Pad {
                    row: b.row,
                    col: if how == AlignPad::Front {
                        b.start
                    } else {
                        b.end
                    },
                    text: unit.repeat(add_block),
                    front: false,
                });
            }
        }
        if pads.is_empty() {
            return false;
        }
        let heads: Vec<((usize, usize), bool)> = self
            .caret_selections()
            .iter()
            .map(|s| (s.head, s.head == s.normalised().0))
            .collect();
        self.apply_alignment(&pads, &heads);
        true
    }
}

impl Editor {
    /// Insert every pad as one undo step, then leave a bare caret where each
    /// of `heads` was pushed to by the padding at or before it.
    ///
    /// A pad inserted at or left of a caret feeds it forward with its line, so
    /// a place pinned from the line end before the edits still names the spot
    /// of the caret afterwards. Padding right of it on the same line is the
    /// exception — the line grows without the caret moving — so the pin counts
    /// those chars in. A pad exactly at a caret is that selection's own end
    /// padding and feeds it forward; a front pad there belongs to the next
    /// block, and an end caret has to stay in front of it, so the pin counts it
    /// in — unless the caret is the start of its own selection (a bare caret
    /// or one drawn backwards), where that front pad is its own and carries the
    /// caret along.
    fn apply_alignment(&mut self, pads: &[Pad], heads: &[((usize, usize), bool)]) {
        if pads.is_empty() || self.lines.is_empty() {
            return;
        }
        self.multi_typing = None;
        self.pin_on_edit();
        self.break_undo_coalescing();
        self.push_undo(EditKind::MultiEdit);
        let pins: Vec<(bool, (usize, usize))> = heads
            .iter()
            .enumerate()
            .map(|(i, &((row, col), at_start))| {
                let right: usize = pads
                    .iter()
                    .filter(|p| {
                        p.row == row && (p.col > col || (p.col == col && p.front && !at_start))
                    })
                    .map(|p| p.text.chars().count())
                    .sum();
                let (from_bottom, from_end) = self.pin_from_end((row, col));
                (i == 0, (from_bottom, from_end + right))
            })
            .collect();
        // Bottom-up and right-to-left, so an insert never shifts a place still
        // to be written. At one spot the selection's length goes in first, so
        // the column's padding ends up to its left.
        let mut order: Vec<usize> = (0..pads.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse((pads[i].row, pads[i].col, !pads[i].front)));
        self.selection = None;
        let last = self.lines.len().saturating_sub(1);
        for i in order {
            let pad = &pads[i];
            self.cursor_row = pad.row.min(last);
            self.cursor_col = pad.col.min(self.line_char_len(self.cursor_row));
            for c in pad.text.chars() {
                self.insert_char_raw(c);
            }
        }
        self.selection = None;
        self.place_pinned_carets(&pins);
        self.recompute_highlights();
    }
}
