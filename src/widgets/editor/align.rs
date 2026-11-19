//! Aligning carets: padding every caret's selection into one column, with
//! spaces, with tabs, or at the end of its line.

use super::*;

/// How a column's padding is split between a selection's two ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AlignPad {
    /// Spaces at the front until every start in the column lines up, then at
    /// the end until every selection is as long as the longest.
    Both,
    /// Spaces, all at the front, so the selected text right-aligns.
    Front,
}

/// One run of padding to insert: the line it goes on, the char column it
/// starts at, and the whitespace itself.
struct Pad {
    row: usize,
    col: usize,
    text: String,
}

/// Chars a run of `tabs` tab stops advances a position `from` columns into a
/// line, on a `width`-wide tab grid. No tab moves nothing, so `from` comes
/// back unchanged; any tab at all takes a column already on a stop a whole
/// stop further, since a tab can never stand still.
pub(super) fn tab_advance(from: usize, tabs: usize, width: usize) -> usize {
    if tabs == 0 {
        return from;
    }
    let width = width.max(1);
    (from / width + tabs) * width
}

/// Tabs that carry a position `from` columns into a line to the first tab stop
/// at or after `to`. Zero when `from` has already reached `to`, and at least
/// one whenever it has not: a tab never leaves its column where it was.
pub(super) fn tabs_to_reach(from: usize, to: usize, width: usize) -> usize {
    let width = width.max(1);
    if from >= to {
        return 0;
    }
    // The stop `to` needs, less the one `from` sits in. Integer division is
    // the floor the stop index needs, so this counts stops rather than
    // columns: `from` 3 to `to` 9 on a 4-wide grid is 3 stops (12), not the
    // two a rounded-up column count would give (8, still short).
    to.div_ceil(width) - from / width
}

impl Editor {
    /// Align Cursors (`Alt+A`): pad every caret's selection with spaces so
    /// that every selection starting in the same column starts in one column,
    /// as long as the longest of them. Selections are grouped left to right
    /// by the columns they touch — carets stacked over one another
    /// (`3..5`, `5..9`, `9..10`) are one column, not three — so a second
    /// group further right is aligned on its own. One undo step; fewer than
    /// two carets is a no-op.
    pub fn align_cursors(&mut self) -> bool {
        self.align_columns(AlignPad::Both, None)
    }

    /// Align Cursors (Pad Front) (`Alt+F`): the same columns, but every space
    /// goes at the front, so the selected text right-aligns into one column
    /// instead of starting in one. One undo step.
    pub fn align_cursors_pad_front(&mut self) -> bool {
        self.align_columns(AlignPad::Front, None)
    }

    /// Align Cursors Using Tabs (`Ctrl+Alt+A`): the same columns, padded with
    /// tab characters snapped to stops `tab_width` apart, so the text starts
    /// and ends on tab stops rather than on the exact columns spaces would
    /// reach. `tab_width` is the buffer's own indent width, never a fixed 4.
    /// One undo step.
    pub fn align_cursors_using_tabs(&mut self, tab_width: usize) -> bool {
        self.align_columns(AlignPad::Both, Some(tab_width))
    }

    /// Align Line Last Cursors (`Ctrl+Alt+Shift+A`): pad the front of every
    /// affected line with spaces so the right-most selection end on each of
    /// them lands in one column — whole lines shift right rather than the
    /// selected text moving inside them. One undo step; fewer than two carets
    /// is a no-op.
    pub fn align_line_last_cursors(&mut self) -> bool {
        let all = self.caret_selections();
        if all.len() < 2 {
            return false;
        }
        let width = self.indent_style().width as usize;
        // The right-most selection end on each affected line, in display
        // columns. A selection across lines has no single column of its own,
        // so only single-line ones name their line's end.
        let mut last: Vec<(usize, usize)> = Vec::new();
        for sel in &all {
            let (start, end) = sel.normalised();
            if start.0 != end.0 {
                continue;
            }
            let end = self.display_col(end.0, end.1, width.max(1));
            match last.iter_mut().find(|(row, _)| *row == start.0) {
                Some(slot) => slot.1 = slot.1.max(end),
                None => last.push((start.0, end)),
            }
        }
        let target = last.iter().map(|&(_, end)| end).max().unwrap_or(0);
        // The padding goes in after the line's own indentation: a space put
        // before a tab is swallowed by the tab's jump to its stop, so a line
        // indented with tabs would never move. Past the indentation every
        // space carries the rest of the line exactly one cell right.
        let pads: Vec<Pad> = last
            .iter()
            .filter(|&&(_, end)| end < target)
            .map(|&(row, end)| Pad {
                row,
                col: self
                    .lines
                    .get(row)
                    .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
                    .unwrap_or(0),
                text: " ".repeat(target - end),
            })
            .collect();
        let heads: Vec<(usize, usize)> = all.iter().map(|s| s.head).collect();
        let applied = !pads.is_empty();
        self.apply_alignment(&pads, &heads);
        applied
    }

    /// The display column `col` chars into `row` occupy: a tab takes the cells
    /// to the next stop of `width`-wide tabs, exactly as the editor draws it,
    /// so alignment is measured on screen rather than in characters and a line
    /// that already holds tabs still lines up.
    fn display_col(&self, row: usize, col: usize, width: usize) -> usize {
        let width = width.max(1);
        let mut display = 0;
        let Some(line) = self.lines.get(row) else {
            return display;
        };
        for c in line.chars().take(col) {
            display += if c == '\t' {
                width - display % width
            } else {
                // A wide character is two cells on screen, and a combining
                // mark none, exactly as the renderer draws it.
                unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
            };
        }
        display
    }

    /// The column commands' shared geometry: group the carets' selections into
    /// columns, work out each one's front and back padding, and lay it down as
    /// one edit.
    ///
    /// Everything is measured in display columns, and each row carries the
    /// width already inserted left of where it stands: a pad worked out from
    /// the original columns lands short by exactly that much, so only the
    /// shortfall goes in and a second group further right still lines up.
    fn align_columns(&mut self, how: AlignPad, tabs: Option<usize>) -> bool {
        let all = self.caret_selections();
        if all.len() < 2 {
            return false;
        }
        let spans: Vec<((usize, usize), (usize, usize))> =
            all.iter().map(|s| s.normalised()).collect();
        // A selection across lines has no single column and no single line to
        // pad, so it takes no part in the geometry; its caret keeps its place.
        let single: Vec<usize> = (0..all.len())
            .filter(|&i| spans[i].0.0 == spans[i].1.0)
            .collect();
        // Tabs in the line before a selection also set its column, so the tab
        // stop is needed even when the padding itself is spaces.
        let width = tabs
            .unwrap_or_else(|| self.indent_style().width as usize)
            .max(1);
        let mut pads: Vec<Pad> = Vec::new();
        let mut shifted: std::collections::BTreeMap<usize, usize> =
            std::collections::BTreeMap::new();
        // A tab in front of a selection moves it right without moving its char
        // index, so two selections in one column on screen can have different
        // char starts. Grouping compares the columns they occupy on screen.
        let spans_d: Vec<(usize, usize)> = spans
            .iter()
            .map(|&(start, end)| {
                (
                    self.display_col(start.0, start.1, width),
                    self.display_col(end.0, end.1, width),
                )
            })
            .collect();
        // Bare carets have no width to group by, so they are all one column:
        // "line these carets up" is the whole meaning of the command when
        // nothing is selected. Selections keep their own columns, where only
        // the ones that touch share.
        let groups = if single.len() > 1 && single.iter().all(|&i| spans[i].0 == spans[i].1) {
            vec![single.clone()]
        } else {
            column_groups(&spans_d, &single)
        };
        for group in groups {
            let start_d = |e: &Editor, i: usize| e.display_col(spans[i].0.0, spans[i].0.1, width);
            let end_d = |e: &Editor, i: usize| e.display_col(spans[i].1.0, spans[i].1.1, width);
            // Where this group's selections stand now: earlier groups on the
            // same row already pushed them right, and aligning to the original
            // columns would leave the second group short by exactly that.
            let target = group
                .iter()
                .map(|&i| start_d(self, i) + shifted.get(&spans[i].0.0).copied().unwrap_or(0))
                .max()
                .unwrap_or(0);
            let longest = group
                .iter()
                .map(|&i| end_d(self, i) - start_d(self, i))
                .max()
                .unwrap_or(0);
            // The column every selection in this group starts in: the
            // right-most start, or — for the tab variant — the first tab stop
            // at or after it, because starts only line up if they all sit on
            // that one stop. The end every selection is padded out to follows
            // from it: the aligned column plus the longest selection.
            let align_col = if tabs.is_some() {
                target.div_ceil(width) * width
            } else {
                target
            };
            // Tabs can only land on a stop, so with the tab variant the end
            // every selection is padded out to is the next stop at or after
            // the longest one — otherwise a longer selection would end on the
            // stop and a shorter one exactly at its own length, and the ends
            // would not line up.
            let aim_end = if tabs.is_some() {
                (align_col + longest).div_ceil(width) * width
            } else {
                align_col + longest
            };
            let mut members = group.clone();
            members.sort_by_key(|&i| start_d(self, i));
            for i in members {
                let (start, end) = spans[i];
                let len = end_d(self, i) - start_d(self, i);
                let row = start.0;
                let moved = shifted.get(&row).copied().unwrap_or(0);
                let (front, back, advance) = match (how, tabs) {
                    (_, Some(_)) => {
                        let from = start_d(self, i) + moved;
                        let front = tabs_to_reach(from, align_col, width);
                        let after_front = tab_advance(from, front, width);
                        let back = tabs_to_reach(after_front + len, aim_end, width);
                        let after = tab_advance(after_front + len, back, width);
                        (
                            front,
                            back,
                            (after_front - from) + (after - after_front - len),
                        )
                    }
                    // All the padding at the front pushes the text's end out
                    // to the column the longest selection reaches.
                    (AlignPad::Front, None) => {
                        let front = (align_col - start_d(self, i)).max(0) + (longest - len);
                        (front.saturating_sub(moved), 0, front.saturating_sub(moved))
                    }
                    (AlignPad::Both, None) => {
                        let front = (align_col - start_d(self, i)).saturating_sub(moved);
                        (front, longest - len, front + (longest - len))
                    }
                };
                if front > 0 {
                    pads.push(Pad {
                        row,
                        col: start.1,
                        text: if tabs.is_some() {
                            "\t".repeat(front)
                        } else {
                            " ".repeat(front)
                        },
                    });
                }
                if back > 0 {
                    pads.push(Pad {
                        row,
                        col: end.1,
                        text: if tabs.is_some() {
                            "\t".repeat(back)
                        } else {
                            " ".repeat(back)
                        },
                    });
                }
                *shifted.entry(row).or_insert(0) += advance;
            }
        }
        let heads: Vec<(usize, usize)> = all.iter().map(|s| s.head).collect();
        let applied = !pads.is_empty();
        self.apply_alignment(&pads, &heads);
        applied
    }

    /// Insert every pad as one undo step, then leave a bare caret where each
    /// of `heads` was pushed to by the padding at or before it.
    ///
    /// A pad inserted at or left of a caret feeds it forward with its line, so
    /// a place pinned from the line's end before the edits still names the
    /// caret's own spot afterwards. Padding right of it on the same line is
    /// the exception — the line grows without the caret moving — so the pin
    /// counts those chars in.
    fn apply_alignment(&mut self, pads: &[Pad], heads: &[(usize, usize)]) {
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
            .map(|(i, &(row, col))| {
                let right: usize = pads
                    .iter()
                    .filter(|p| p.row == row && p.col > col)
                    .map(|p| p.text.chars().count())
                    .sum();
                let (from_bottom, from_end) = self.pin_from_end((row, col));
                (i == 0, (from_bottom, from_end + right))
            })
            .collect();
        // Bottom-up and right-to-left, so an insert never shifts a place
        // still to be written.
        let mut order: Vec<usize> = (0..pads.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse((pads[i].row, pads[i].col)));
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

/// The columns `spans` fall into, left to right, as indices into `spans`.
///
/// A span joins the column whose furthest end it starts at or before, so
/// touching and overlapping selections — carets stacked over one another —
/// share a column, and only a gap starts a new one. `spans` holds display
/// columns, so a tab in front of a selection counts for where it stands.
fn column_groups(spans: &[(usize, usize)], members: &[usize]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = members.to_vec();
    order.sort_by_key(|&i| (spans[i].0, spans[i].1));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut furthest = 0;
    for i in order {
        let (start, end) = spans[i];
        match groups.last_mut() {
            Some(group) if start <= furthest => {
                group.push(i);
                furthest = furthest.max(end);
            }
            _ => {
                groups.push(vec![i]);
                furthest = end.max(start);
            }
        }
    }
    groups
}
