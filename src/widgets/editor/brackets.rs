//! Brackets: matching pairs, jumping and selecting to the matching bracket, and
//! highlighting the pair at the caret.

use super::*;

impl Editor {
    /// The character at `(row, char_col)`, or None when out of range. Used by
    /// the bracket-matching helpers, which work in char indices.
    pub(super) fn char_at(&self, row: usize, col: usize) -> Option<char> {
        self.lines.get(row)?.chars().nth(col)
    }

    /// Which string or comment each bracket lies in (#1634), from the last
    /// highlight pass. `None` while the pass is stale or the buffer has no
    /// grammar: every bracket then counts, as before.
    pub(super) fn bracket_zones(&self) -> Option<BracketZones<'_>> {
        if !self.protected_current || self.protected_ranges.is_empty() {
            return None;
        }
        let mut starts = Vec::with_capacity(self.lines.len());
        let mut at = 0;
        for line in &self.lines {
            starts.push(at);
            at += line.len() + 1;
        }
        Some(BracketZones {
            lines: &self.lines,
            starts,
            ranges: &self.protected_ranges,
        })
    }

    /// Forward-scan from the opening bracket at `open` to its matching close,
    /// counting nesting of the SAME pair type. Returns the `(row, col)` of the
    /// closing bracket, or None when unbalanced. Brackets in strings and
    /// comments are text (#1634): they are passed over, and one of them as
    /// `open` matches nothing, even a balanced `"()"`.
    pub(super) fn match_bracket_forward(
        &self,
        open: BPos,
        zones: Option<&BracketZones>,
    ) -> Option<BPos> {
        let oc = self.char_at(open.0, open.1)?;
        let cc = matching_close(oc)?;
        if zones.is_some_and(|z| z.zone(open).is_some()) {
            return None;
        }
        let mut depth = 0i32;
        // One pass along each line. `char_at` walks a line from its start,
        // so stepping a scan with it was quadratic in the line's length: a
        // minified line's opening bracket froze the first frame (#959).
        for (row, line) in self.lines.iter().enumerate().skip(open.0) {
            let from = if row == open.0 { open.1 } else { 0 };
            for (col, (byte, ch)) in line.char_indices().enumerate().skip(from) {
                if (ch != oc && ch != cc) || zones.is_some_and(|z| z.zone_at(row, byte).is_some()) {
                    continue;
                }
                if ch == oc {
                    depth += 1;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some((row, col));
                    }
                }
            }
        }
        None
    }

    /// Backward-scan from the closing bracket at `close` to its matching open,
    /// passing over brackets in strings and comments, as
    /// [`Self::match_bracket_forward`] does.
    pub(super) fn match_bracket_backward(
        &self,
        close: BPos,
        zones: Option<&BracketZones>,
    ) -> Option<BPos> {
        let cc = self.char_at(close.0, close.1)?;
        let oc = matching_open(cc)?;
        if zones.is_some_and(|z| z.zone(close).is_some()) {
            return None;
        }
        let mut depth = 0i32;
        for row in (0..=close.0).rev() {
            let end = (row == close.0).then_some(close.1 + 1);
            for (col, byte, ch) in chars_back(&self.lines[row], end) {
                if (ch != oc && ch != cc) || zones.is_some_and(|z| z.zone_at(row, byte).is_some()) {
                    continue;
                }
                if ch == cc {
                    depth += 1;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some((row, col));
                    }
                }
            }
        }
        None
    }

    /// The innermost bracket pair enclosing the cursor, found by walking
    /// backwards for the first unmatched opening bracket and matching it
    /// forwards. Mirrors VS Code's `findEnclosingBrackets`. Brackets in
    /// strings and comments are passed over, so from inside a string this
    /// finds the pair in code around it.
    pub(super) fn enclosing_brackets(&self, zones: Option<&BracketZones>) -> Option<(BPos, BPos)> {
        let mut expected: Vec<char> = Vec::new();
        for row in (0..=self.cursor_row).rev() {
            let end = (row == self.cursor_row).then_some(self.cursor_col);
            for (col, byte, ch) in chars_back(self.lines.get(row)?, end) {
                let pos = (row, col);
                let in_code = || zones.is_none_or(|z| z.zone_at(row, byte).is_none());
                if is_close_bracket(ch) && in_code() {
                    expected.push(matching_open(ch)?);
                } else if is_open_bracket(ch) && in_code() {
                    match expected.last() {
                        Some(&want) if want == ch => {
                            expected.pop();
                        }
                        _ => {
                            let close = self.match_bracket_forward(pos, zones)?;
                            return Some((pos, close));
                        }
                    }
                }
            }
        }
        None
    }

    /// The first bracket pair at or after the cursor (`findNextBracket`),
    /// passing over brackets in strings and comments.
    pub(super) fn next_bracket_pair(&self, zones: Option<&BracketZones>) -> Option<(BPos, BPos)> {
        for (row, line) in self.lines.iter().enumerate().skip(self.cursor_row) {
            let from = if row == self.cursor_row {
                self.cursor_col
            } else {
                0
            };
            for (col, (byte, ch)) in line.char_indices().enumerate().skip(from) {
                let in_code = zones.is_none_or(|z| z.zone_at(row, byte).is_none());
                if is_open_bracket(ch) && in_code {
                    let open = (row, col);
                    return self.match_bracket_forward(open, zones).map(|c| (open, c));
                }
                if is_close_bracket(ch) && in_code {
                    let close = (row, col);
                    return self
                        .match_bracket_backward(close, zones)
                        .map(|o| (o, close));
                }
            }
        }
        None
    }

    /// Resolve the bracket pair relevant to the cursor, returning
    /// `(open, close, anchored_on_close)`. `anchored_on_close` is true when the
    /// cursor sits on the closing bracket, so a jump should target the open
    /// bracket instead. Adjacency priority mirrors VS Code's `matchBracket`:
    /// a close to the left wins, then an open to the right, then an open to the
    /// left, then a close to the right; failing all four it falls back to the
    /// enclosing pair, then the next bracket forward.
    pub(super) fn find_bracket_pair(&self) -> Option<(BPos, BPos, bool)> {
        let zones = self.bracket_zones();
        if let Some(pair) = self.adjacent_bracket_pair(zones.as_ref()) {
            return Some(pair);
        }
        if let Some((open, close)) = self.enclosing_brackets(zones.as_ref()) {
            return Some((open, close, false));
        }
        self.next_bracket_pair(zones.as_ref())
            .map(|(o, c)| (o, c, false))
    }

    /// The bracket pair whose bracket sits immediately to the left or right of
    /// the caret, as `(open, close, anchored_on_close)`. `None` when the caret
    /// touches no bracket. Adjacency priority mirrors VS Code's `matchBracket`:
    /// a close to the left wins, then an open to the right, then an open to the
    /// left, then a close to the right. A bracket in a string or comment
    /// pairs with nothing.
    pub(super) fn adjacent_bracket_pair(
        &self,
        zones: Option<&BracketZones>,
    ) -> Option<(BPos, BPos, bool)> {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let left = col.checked_sub(1).and_then(|c| self.char_at(row, c));
        let right = self.char_at(row, col);
        if left.is_some_and(is_close_bracket) {
            let close = (row, col - 1);
            return self
                .match_bracket_backward(close, zones)
                .map(|o| (o, close, true));
        }
        if right.is_some_and(is_open_bracket) {
            let open = (row, col);
            return self
                .match_bracket_forward(open, zones)
                .map(|c| (open, c, false));
        }
        if left.is_some_and(is_open_bracket) {
            let open = (row, col - 1);
            return self
                .match_bracket_forward(open, zones)
                .map(|c| (open, c, false));
        }
        if right.is_some_and(is_close_bracket) {
            let close = (row, col);
            return self
                .match_bracket_backward(close, zones)
                .map(|o| (o, close, true));
        }
        None
    }

    /// The bracket pair to highlight (VS Code's `editorBracketMatch`): the
    /// bracket adjacent to the caret and its match, as `(open, close)`. Unlike
    /// [`Self::find_bracket_pair`] it never falls back to the enclosing or next
    /// pair, so the highlight appears only when the caret is beside a bracket.
    pub fn bracket_match_pair(&self) -> Option<(BPos, BPos)> {
        let zones = self.bracket_zones();
        self.adjacent_bracket_pair(zones.as_ref())
            .map(|(o, c, _)| (o, c))
    }

    /// [`Self::bracket_match_pair`], worked out once per edit, highlight
    /// pass and caret place. Every frame asks, and a partner far along a
    /// long line costs a scan of that line, which kept a core busy while
    /// croft sat idle on a minified file (#959). A pass can move the strings
    /// and comments without an edit (Change Language Mode), so it counts.
    pub(super) fn cached_bracket_match_pair(&mut self) -> Option<(BPos, BPos)> {
        let key = (
            self.edit_seq,
            self.highlight_generation,
            (self.cursor_row, self.cursor_col),
            self.protected_current,
        );
        if let Some((at, pair)) = self.bracket_pair_cache
            && at == key
        {
            return pair;
        }
        let pair = self.bracket_match_pair();
        self.bracket_pair_cache = Some((key, pair));
        pair
    }

    /// VS Code "Go to Bracket" (`editor.action.jumpToBracket`, Cmd+Shift+\):
    /// move the cursor to the matching bracket. From an opening bracket it
    /// lands on the closing one and vice versa; from inside a pair it lands on
    /// the enclosing closing bracket. Returns false (a no-op) when no bracket
    /// is in play.
    pub fn jump_to_matching_bracket(&mut self) -> bool {
        let Some((open, close, anchored_on_close)) = self.find_bracket_pair() else {
            return false;
        };
        let target = if anchored_on_close { open } else { close };
        self.clear_selection();
        self.cursor_row = target.0;
        self.cursor_col = target.1;
        self.end_caret_action();
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Select to Bracket" (`editor.action.selectToBracket`): select
    /// the region between the matching brackets, including the brackets
    /// themselves (from the opening bracket's start to the closing bracket's
    /// end). Returns false (a no-op) when no bracket pair is in play.
    pub fn select_to_matching_bracket(&mut self) -> bool {
        let Some((open, close, _)) = self.find_bracket_pair() else {
            return false;
        };
        let head = (close.0, close.1 + 1);
        self.selection = Some(EditorSelection { anchor: open, head });
        self.cursor_row = head.0;
        self.cursor_col = head.1;
        self.end_caret_action();
        self.ensure_cursor_col_visible();
        true
    }
}

/// The string and comment byte ranges of a buffer, with each line's start
/// offset, so a bracket scan can tell code from text (#1634).
pub(super) struct BracketZones<'a> {
    lines: &'a [String],
    starts: Vec<usize>,
    ranges: &'a [(usize, usize)],
}

impl BracketZones<'_> {
    /// The string or comment `(row, char col)` lies in, as an index into
    /// the ranges; `None` in code.
    pub(super) fn zone(&self, (row, col): BPos) -> Option<usize> {
        self.zone_at(row, char_byte(self.lines.get(row)?, col))
    }

    /// [`Self::zone`] of the char at `byte` on `row`, for a scan that
    /// already has the byte offset.
    pub(super) fn zone_at(&self, row: usize, byte: usize) -> Option<usize> {
        let byte = self.starts.get(row)? + byte;
        let i = self.ranges.partition_point(|&(_, end)| end <= byte);
        self.ranges
            .get(i)
            .filter(|&&(start, _)| start <= byte)
            .map(|_| i)
    }
}

/// A `(row, char_col)` buffer position: a bracket character's in the
/// bracket matching helpers, a selection end's in the line commands.
pub(super) type BPos = (usize, usize);

/// `Editor::bracket_pair_cache`: the key it was worked out at, and the pair.
pub(super) type BracketPairCache = ((u64, u64, BPos, bool), Option<(BPos, BPos)>);

/// The bracket pairs croft matches, mirroring VS Code's default language
/// brackets `()`, `[]`, `{}`.
pub(super) const BRACKET_PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

pub(super) fn is_open_bracket(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(o, _)| o == c)
}

pub(super) fn is_close_bracket(c: char) -> bool {
    BRACKET_PAIRS.iter().any(|&(_, cl)| cl == c)
}

pub(super) fn matching_close(c: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|&&(o, _)| o == c)
        .map(|&(_, cl)| cl)
}

pub(super) fn matching_open(c: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|&&(_, cl)| cl == c)
        .map(|&(o, _)| o)
}

/// Tint the single bracket cell on cell `col` of row `y`'s text with the
/// theme's bracket-match background, leaving the glyph's foreground intact so
/// the bracket stays readable. No-op when the cell is past the row's edge.
pub(super) fn paint_bracket_match(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col: usize,
    theme: crate::theme::Theme,
) {
    if col >= usize::from(text_width) {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    cell.set_style(cell.style().bg(theme.bracket_match_bg()));
}
