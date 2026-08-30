//! Display-cell geometry of one line of text (#404).
//!
//! A terminal cell is not a character: CJK and most emoji take two cells, a
//! combining mark takes none, and a control character is not painted at all.
//! Anything that paints text and then reasons about WHERE it landed - a
//! span painted after another, a selection band, a click mapped back to a
//! column - has to agree with the painter about that geometry, and the
//! painter is ratatui's `Buffer::set_stringn`: it walks graphemes, drops any
//! grapheme containing a control character, drops zero-width graphemes, and
//! advances by each grapheme's `unicode-width`. This map applies exactly
//! those rules per character, so the two cannot drift apart.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Where each character of a line lands on screen, in cells from the line's
/// left edge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CellMap {
    /// Per character: its byte offset, the first cell it occupies, and how
    /// many cells it occupies. A character after the first in a grapheme
    /// cluster occupies none: the cluster's base carries the width.
    chars: Vec<(usize, u16, u16)>,
    /// Cells the whole line occupies.
    total: u16,
}

impl CellMap {
    pub(crate) fn new(text: &str) -> Self {
        let mut map = Self::default();
        map.build_into(text);
        map
    }

    /// Refill the map for `text`, reusing the allocation.
    ///
    /// The log painter maps every visible row on every frame; one map
    /// hoisted out of the row loop and refilled here costs nothing after
    /// the first frame, where a fresh `Vec` per row (sized by BYTE length,
    /// three times too large for the CJK lines this exists for) put
    /// hundreds of kilobytes of allocation on the redraw path. The same
    /// shape as `ansi_text::parse_into`, for the same reason.
    pub(crate) fn build_into(&mut self, text: &str) {
        self.chars.clear();
        let mut cell: u16 = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            let width = if grapheme.contains(char::is_control) {
                0
            } else {
                u16::try_from(grapheme.width()).unwrap_or(u16::MAX)
            };
            for (i, (offset, _)) in grapheme.char_indices().enumerate() {
                if i == 0 {
                    self.chars.push((byte + offset, cell, width));
                } else {
                    self.chars
                        .push((byte + offset, cell.saturating_add(width), 0));
                }
            }
            cell = cell.saturating_add(width);
        }
        self.total = cell;
    }

    /// The first cell character `col` occupies; the line's total width at or
    /// past the end, which is where a cursor after the last character sits.
    pub(crate) fn cell_of_char(&self, col: usize) -> u16 {
        self.chars.get(col).map_or(self.total, |c| c.1)
    }

    /// Cells character `col` occupies: 2 for a wide character, 0 for a
    /// combining mark or a control character.
    pub(crate) fn width_of_char(&self, col: usize) -> u16 {
        self.chars.get(col).map_or(0, |c| c.2)
    }

    /// The cell the character starting at byte offset `byte` occupies. Span
    /// boundaries are byte offsets, so this is how a span finds its column.
    pub(crate) fn cell_of_byte(&self, byte: usize) -> u16 {
        let col = self.chars.partition_point(|c| c.0 < byte);
        self.cell_of_char(col)
    }

    /// The character column a screen cell lands on: the character painted
    /// there, either half of a wide one, or the character count for a cell
    /// past the end of the line.
    pub(crate) fn char_at_cell(&self, cell: u16) -> usize {
        self.chars
            .iter()
            .position(|&(_, first, width)| width > 0 && cell >= first && cell < first + width)
            .unwrap_or(self.chars.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_cell_per_char() {
        let m = CellMap::new("abc");
        assert_eq!(
            (0..4).map(|c| m.cell_of_char(c)).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert_eq!(
            (0..4).map(|c| m.char_at_cell(c)).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert_eq!(m.cell_of_byte(2), 2);
    }

    #[test]
    fn a_wide_char_takes_two_cells_and_both_halves_map_back_to_it() {
        let m = CellMap::new("\u{4e2d}\u{6587} X");
        assert_eq!(m.cell_of_char(1), 2, "the second CJK char starts on cell 2");
        assert_eq!(m.cell_of_char(3), 5, "the X starts on cell 5");
        assert_eq!(m.width_of_char(0), 2);
        assert_eq!(
            m.char_at_cell(1),
            0,
            "the right half of the first char is still it"
        );
        assert_eq!(m.char_at_cell(5), 3);
        assert_eq!(m.char_at_cell(9), 4, "past the end is the char count");
        assert_eq!(m.cell_of_byte(6), 4, "byte 6 is the space, on cell 4");
    }

    #[test]
    fn a_combining_mark_occupies_no_cell_of_its_own() {
        // e + combining acute is one grapheme, one cell; the mark is a
        // second CHARACTER that the selection arithmetic still has to count.
        let m = CellMap::new("e\u{301}x");
        assert_eq!(m.char_at_cell(9), 3, "three characters, past the end");
        assert_eq!(m.cell_of_char(0), 0);
        assert_eq!(m.width_of_char(1), 0);
        assert_eq!(m.cell_of_char(2), 1, "x follows the cluster on cell 1");
        assert_eq!(m.char_at_cell(1), 2);
    }

    #[test]
    fn a_control_char_is_not_painted_so_takes_no_cell() {
        let m = CellMap::new("a\tb");
        assert_eq!(m.width_of_char(1), 0);
        assert_eq!(
            m.cell_of_char(2),
            1,
            "ratatui drops the tab, so b lands on cell 1"
        );
    }
}
