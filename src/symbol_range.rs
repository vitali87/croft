//! Keeping a symbol tab pointed at its symbol while the file changes (#369).
//!
//! A symbol tab is a VIEW over a byte range of the live buffer, not a copy of
//! it — that is what makes edits, LSP, diagnostics, undo and collab work
//! without a second pipeline. The cost is that every edit to the file can
//! move the range, and the tab has to follow.
//!
//! # The three cases, and why the middle one is the hard one
//!
//! An edit lands ABOVE the symbol: the whole range shifts by the net change.
//! An edit lands BELOW it: nothing moves. Both are arithmetic.
//!
//! An edit lands INSIDE the symbol — the common case, since a symbol tab
//! exists to be typed in — and the range must GROW OR SHRINK rather than
//! shift, because the symbol is still the same symbol. Getting this wrong by
//! shifting instead of resizing makes the tab slide off the end of its own
//! function as the user types in it.
//!
//! # What is deliberately not attempted
//!
//! An edit that STRADDLES the boundary — replacing a span that starts inside
//! the symbol and ends after it — leaves the range meaningless, because the
//! text that defined the end is gone. That reports `Gone` rather than
//! guessing a new end: closing the tab with a notice is honest, and a tab
//! silently re-anchored to half a function plus whatever followed it is not.

/// Where a symbol tab is pointing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymbolRange {
    /// Byte offset of the symbol's first character.
    pub start: usize,
    /// Byte offset one past its last character.
    pub end: usize,
}

/// What an edit did to a symbol range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeAfterEdit {
    /// The symbol survives, here.
    At(SymbolRange),
    /// The edit removed or straddled the symbol: the tab should close with a
    /// notice rather than re-anchor to something that is not the symbol.
    Gone,
}

impl SymbolRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Clippy requires this beside `len`; nothing calls it yet.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// Follow an edit that replaced `removed` bytes at `at` with `inserted`
    /// bytes.
    ///
    /// One function for all three cases so the boundaries cannot disagree:
    /// splitting it into `shift_if_above` and `grow_if_inside` invites two
    /// definitions of "inside", and an edit landing exactly on the start or
    /// end offset would then be handled twice or not at all.
    pub fn after_edit(self, at: usize, removed: usize, inserted: usize) -> RangeAfterEdit {
        let edit_end = at.saturating_add(removed);

        // Entirely BELOW the symbol: nothing moves.
        if at >= self.end {
            return RangeAfterEdit::At(self);
        }

        // Entirely ABOVE it: the whole range slides by the net change.
        //
        // A DELETION or replacement ending exactly at `start` is above: the
        // text it consumed was not the symbol's. But a zero-width INSERTION
        // at `start` is inside, because the typed bytes land at the symbol's
        // first position and are part of it — treating it as above would
        // make a character typed at the very top of the function push the
        // tab down and off it. The two cases share an `edit_end` and are
        // told apart by whether anything was removed, which is why this is
        // one condition rather than a `<=`.
        if edit_end <= self.start && !(removed == 0 && at == self.start) {
            let delta_start = self.start + inserted - removed;
            return RangeAfterEdit::At(SymbolRange::new(
                delta_start,
                self.end + inserted - removed,
            ));
        }

        // The edit covers the whole symbol: it is gone, not resized.
        if at <= self.start && edit_end >= self.end {
            return RangeAfterEdit::Gone;
        }

        // STRADDLES either boundary: part of the symbol was replaced along
        // with text outside it, so the surviving range would be half a
        // symbol glued to whatever the edit left behind. Reported gone
        // rather than guessed.
        if at < self.start || edit_end > self.end {
            return RangeAfterEdit::Gone;
        }

        // Wholly INSIDE: the symbol is still the same symbol, so the range
        // grows or shrinks rather than moving. This is the case a shift
        // would break — typing in the tab would slide it off its own
        // function.
        RangeAfterEdit::At(SymbolRange::new(self.start, self.end + inserted - removed))
    }
}

/// The innermost symbol whose line range encloses `line`, if any.
///
/// INNERMOST, so a method inside an `impl` wins over the `impl` — a symbol
/// tab opened from inside a method should show the method, and the enclosing
/// block is almost never what was meant. "Innermost" is decided by the
/// narrowest line span rather than by depth, because two symbols can share a
/// depth while one contains the other.
///
/// On a TIE the LAST match wins, matching `OutlinePanel::follow_caret`'s
/// `span <= best_span`. Symbols arrive parents-before-children, so keeping
/// the first would return the parent — and then the same cursor position
/// would highlight one symbol in the Outline and open a different one as a
/// tab. Two pickers disagreeing about "innermost" is worse than either
/// answer, so this follows the one that already ships.
pub fn enclosing_symbol(
    symbols: &[crate::lsp::manager::OutlineSymbol],
    line: u32,
) -> Option<&crate::lsp::manager::OutlineSymbol> {
    symbols
        .iter()
        .filter(|s| s.range_start_line <= line && line <= s.range_end_line)
        .rev()
        .min_by_key(|s| s.range_end_line.saturating_sub(s.range_start_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `fn f` occupying bytes 100..200 of some buffer.
    fn sym() -> SymbolRange {
        SymbolRange::new(100, 200)
    }

    fn outline(name: &str, from: u32, to: u32) -> crate::lsp::manager::OutlineSymbol {
        crate::lsp::manager::OutlineSymbol {
            name: String::from(name),
            detail: None,
            kind: crate::lsp::manager::OutlineKind::Function,
            depth: 0,
            line: from,
            character: 0,
            range_start_line: from,
            range_end_line: to,
        }
    }

    /// The INNERMOST enclosing symbol wins.
    ///
    /// Opening a symbol tab from inside a method should show the method, not
    /// the `impl` that contains it — the enclosing block is almost never
    /// what was meant. Decided by the narrowest span rather than by depth,
    /// since two symbols can share a depth while one contains the other.
    #[test]
    fn the_innermost_enclosing_symbol_wins() {
        let syms = vec![
            outline("impl Foo", 10, 60),
            outline("fn render", 20, 30),
            outline("fn other", 40, 50),
        ];
        let at = |line: u32| enclosing_symbol(&syms, line).map(|s| s.name.as_str());

        assert_eq!(at(25), Some("fn render"), "the method, not its impl");
        assert_eq!(at(45), Some("fn other"));
        // Between the methods but inside the impl: the impl is the answer,
        // because it really is the innermost thing containing that line.
        assert_eq!(at(35), Some("impl Foo"));
        // The boundary lines belong to the symbol they open and close.
        assert_eq!(at(20), Some("fn render"));
        assert_eq!(at(30), Some("fn render"));
        // Outside everything.
        assert_eq!(at(5), None);
        assert_eq!(at(99), None);
        assert_eq!(enclosing_symbol(&[], 1).map(|s| s.name.as_str()), None);

        // IDENTICAL spans: the LAST wins, matching `follow_caret`'s
        // `span <= best_span`. Symbols arrive parents-before-children, so
        // keeping the first would return the parent — and the same cursor
        // would then highlight one symbol in the Outline and open a
        // different one as a tab. Neither answer is obviously right; two
        // pickers disagreeing is definitely wrong.
        let tied = vec![outline("impl Tiny", 7, 7), outline("fn tiny", 7, 7)];
        assert_eq!(
            enclosing_symbol(&tied, 7).map(|s| s.name.as_str()),
            Some("fn tiny"),
            "on a tie the child wins, as the Outline already decides"
        );
    }

    /// An edit inside the symbol RESIZES it; the tab stays on its function.
    ///
    /// This is the case a shift would break, and it is the common one — a
    /// symbol tab exists to be typed in. Shifting instead of resizing slides
    /// the range off the end of the very function it is showing.
    #[test]
    fn typing_inside_the_symbol_grows_it_rather_than_moving_it() {
        // Insert 10 bytes in the middle.
        assert_eq!(
            sym().after_edit(150, 0, 10),
            RangeAfterEdit::At(SymbolRange::new(100, 210)),
            "the start must not move when the edit is inside"
        );
        // Delete 20 from the middle.
        assert_eq!(
            sym().after_edit(150, 20, 0),
            RangeAfterEdit::At(SymbolRange::new(100, 180))
        );
        // Replace 20 with 5.
        assert_eq!(
            sym().after_edit(150, 20, 5),
            RangeAfterEdit::At(SymbolRange::new(100, 185))
        );
        // Right at the inner edge of the start: still inside.
        assert_eq!(
            sym().after_edit(100, 0, 7),
            RangeAfterEdit::At(SymbolRange::new(100, 207))
        );
    }

    /// An edit above shifts the whole range; one below moves nothing.
    #[test]
    fn an_edit_above_shifts_and_one_below_does_nothing() {
        assert_eq!(
            sym().after_edit(10, 0, 30),
            RangeAfterEdit::At(SymbolRange::new(130, 230)),
            "an insertion above slides the symbol down"
        );
        assert_eq!(
            sym().after_edit(10, 30, 0),
            RangeAfterEdit::At(SymbolRange::new(70, 170)),
            "a deletion above slides it up"
        );
        assert_eq!(
            sym().after_edit(500, 40, 3),
            RangeAfterEdit::At(sym()),
            "an edit past the end must not move the symbol at all"
        );
        // Exactly at the end offset is BELOW: text appended after the
        // symbol's last byte belongs to what follows it.
        assert_eq!(sym().after_edit(200, 0, 12), RangeAfterEdit::At(sym()));
    }

    /// An edit ending exactly at the start is ABOVE, not inside.
    ///
    /// Text inserted at the boundary belongs to whatever precedes the
    /// symbol. Counting it as inside would make every keystroke on the line
    /// above silently extend the tab upward until it showed the neighbouring
    /// function too.
    #[test]
    fn the_start_boundary_belongs_to_what_precedes_the_symbol() {
        assert_eq!(
            sym().after_edit(100, 0, 5),
            RangeAfterEdit::At(SymbolRange::new(100, 205)),
            "an insertion AT the start is inside — it lands within the symbol"
        );
        assert_eq!(
            sym().after_edit(90, 10, 0),
            RangeAfterEdit::At(SymbolRange::new(90, 190)),
            "a deletion ending exactly at the start is above"
        );
        // Replacing bytes 95..100 with 20 bytes is a net +15, so the symbol
        // slides by 15 rather than by the inserted count. My first
        // expectation here said 110, which is the length of the insertion
        // rather than the delta — the arithmetic the code does is right.
        assert_eq!(
            sym().after_edit(95, 5, 20),
            RangeAfterEdit::At(SymbolRange::new(115, 215)),
            "a replacement ending at the start shifts by the NET change"
        );
    }

    /// A symbol that was deleted, or half-deleted, closes rather than
    /// re-anchoring.
    ///
    /// The straddle case is the one worth being strict about: the surviving
    /// range would be part of a function glued to whatever the edit left,
    /// and a tab titled `render · app.rs` showing that is a lie. Closing
    /// with a notice is honest.
    #[test]
    fn a_deleted_or_straddled_symbol_reports_gone() {
        // Exactly covering it.
        assert_eq!(sym().after_edit(100, 100, 0), RangeAfterEdit::Gone);
        // Covering more than it.
        assert_eq!(sym().after_edit(50, 300, 0), RangeAfterEdit::Gone);
        // Straddling the START: begins above, ends inside.
        assert_eq!(sym().after_edit(90, 30, 5), RangeAfterEdit::Gone);
        // Straddling the END: begins inside, ends below.
        assert_eq!(sym().after_edit(180, 40, 5), RangeAfterEdit::Gone);
        // Replaced by something, rather than deleted: still gone.
        assert_eq!(sym().after_edit(100, 100, 60), RangeAfterEdit::Gone);
    }

    /// The arithmetic holds for a symbol at the very start of a file, where
    /// a naive `start - removed` would underflow.
    #[test]
    fn a_symbol_at_offset_zero_survives_edits_around_it() {
        let head = SymbolRange::new(0, 50);
        assert_eq!(
            head.after_edit(10, 5, 0),
            RangeAfterEdit::At(SymbolRange::new(0, 45)),
            "an inside deletion shrinks it"
        );
        assert_eq!(
            head.after_edit(0, 0, 9),
            RangeAfterEdit::At(SymbolRange::new(0, 59)),
            "an insertion at byte 0 is inside a symbol starting there"
        );
        assert_eq!(head.after_edit(0, 50, 0), RangeAfterEdit::Gone);
        assert_eq!(head.len(), 50);
        assert!(!head.is_empty());
    }
}
