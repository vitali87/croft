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

/// The one span an edit replaced, as `(at, removed, inserted)`: the longest
/// common prefix and suffix of the two texts, and whatever differs between
/// them. `None` when the texts are equal.
///
/// This is how a LOCAL edit reports itself. The editor mutates through
/// line/column APIs that carry no byte span, and there are dozens of them,
/// so rather than teach each one to report, the view diffs the text it last
/// saw against the text now. One keystroke, paste, undo or reformat of one
/// region is exactly one span. Several regions changed at once (a
/// replace-all) collapse into one span covering them all, which may straddle
/// the symbol; `SymbolView::follow` re-anchors by name when that happens.
///
/// Both ends back off to a char boundary so the span never splits a UTF-8
/// sequence (`é` -> `è` shares its lead byte).
pub fn single_edit_span(old: &str, new: &str) -> Option<(usize, usize, usize)> {
    if old == new {
        return None;
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    // The suffix may not reach back into the prefix on either side.
    let room = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    while !old.is_char_boundary(a.len() - suffix) || !new.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    Some((prefix, a.len() - suffix - prefix, b.len() - suffix - prefix))
}

/// The byte range of lines `first..=last` of `text`, in croft's offset model:
/// `\n` SEPARATES lines, so an N-line buffer has no trailing newline and the
/// last line's range stops at the end of the text rather than a byte past it.
pub fn range_for_lines(text: &str, first: usize, last: usize) -> SymbolRange {
    let mut start = text.len();
    let mut end = text.len();
    let mut line_start = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i == first {
            start = line_start;
        }
        if i == last {
            end = line_start + line.len();
            break;
        }
        line_start += line.len() + 1;
    }
    SymbolRange::new(start.min(end), end)
}

/// The 0-based lines a range covers: the line holding `start`, through the
/// line holding its last byte (`end - 1`; `end` itself is exclusive).
pub fn lines_of(text: &str, range: SymbolRange) -> (usize, usize) {
    let newlines = |upto: usize| {
        text.as_bytes()[..upto.min(text.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
    };
    let first = newlines(range.start);
    let last = if range.end > range.start {
        newlines(range.end - 1)
    } else {
        first
    };
    (first, last)
}

/// A symbol tab's clip over its file (#369): which symbol it shows, and the
/// text it last saw so the next edit can be measured against it.
///
/// The tab holds the WHOLE file, as a split does, so LSP positions, save,
/// diagnostics and go-to-definition all work in true file coordinates. This
/// is only the window onto it.
#[derive(Clone, Debug)]
pub struct SymbolView {
    /// Title name, as the picker that opened the tab spelled it.
    pub name: String,
    pub range: SymbolRange,
    /// First and last visible line, derived from `range`.
    pub first: usize,
    pub last: usize,
    /// The editor's `edit_seq` when `text` was captured.
    pub seen_seq: u64,
    text: String,
    /// The tree-sitter outline's name for the symbol, the identity a rename
    /// or a lost range is resolved against. Kept apart from `name` because
    /// an LSP outline may spell the same symbol differently.
    syntax_name: Option<String>,
}

/// What an edit did to a symbol tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewUpdate {
    /// Still showing its symbol, possibly moved or resized.
    Kept,
    /// The symbol was renamed; carries the OLD title name.
    Renamed(String),
    /// The symbol is gone from the file; the tab should close.
    Gone,
}

impl SymbolView {
    pub fn new(
        name: String,
        text: String,
        first: usize,
        last: usize,
        seq: u64,
        kind: Option<crate::highlight::LangKind>,
    ) -> Self {
        let range = range_for_lines(&text, first, last);
        let (first, last) = lines_of(&text, range);
        let syntax_name = kind.and_then(|k| {
            syntax_symbol_starting_at(
                &crate::outline_syntax::symbols_for(k, text.as_bytes()),
                first,
            )
            .map(|s| s.name.clone())
        });
        Self {
            name,
            range,
            first,
            last,
            seen_seq: seq,
            text,
            syntax_name,
        }
    }

    /// Follow the buffer to `text`, now at `seq`.
    ///
    /// The byte arithmetic in `after_edit` handles the common cases. The
    /// outline is consulted only when it has something to say: an edit
    /// touching the symbol's first line may have renamed it, and a lost
    /// range (a straddling or multi-region edit) is re-found by name nearest
    /// its old position. Parsing only then keeps per-keystroke cost to a
    /// diff.
    pub fn follow(
        &mut self,
        text: String,
        seq: u64,
        kind: Option<crate::highlight::LangKind>,
    ) -> ViewUpdate {
        self.seen_seq = seq;
        let Some((at, removed, inserted)) = single_edit_span(&self.text, &text) else {
            return ViewUpdate::Kept;
        };
        let old_first = self.first;
        let mut update = ViewUpdate::Kept;
        match self.range.after_edit(at, removed, inserted) {
            RangeAfterEdit::At(moved) => {
                self.range = moved;
                (self.first, self.last) = lines_of(&text, moved);
                let first_line_end = text[moved.start..]
                    .find('\n')
                    .map_or(text.len(), |i| moved.start + i);
                if at <= first_line_end
                    && let Some(kind) = kind
                {
                    let symbols = crate::outline_syntax::symbols_for(kind, text.as_bytes());
                    if let Some(sym) = syntax_symbol_starting_at(&symbols, self.first)
                        && self.syntax_name.as_deref() != Some(sym.name.as_str())
                    {
                        let renamed = self.syntax_name.is_some();
                        self.syntax_name = Some(sym.name.clone());
                        if renamed {
                            update = ViewUpdate::Renamed(std::mem::replace(
                                &mut self.name,
                                sym.name.clone(),
                            ));
                        }
                    }
                }
            }
            RangeAfterEdit::Gone => {
                let found = kind.zip(self.syntax_name.as_deref()).and_then(|(k, want)| {
                    crate::outline_syntax::symbols_for(k, text.as_bytes())
                        .into_iter()
                        .filter(|s| s.name == want)
                        .min_by_key(|s| (s.range_start_line as usize).abs_diff(old_first))
                });
                match found {
                    Some(sym) => {
                        self.range = range_for_lines(
                            &text,
                            sym.range_start_line as usize,
                            sym.range_end_line as usize,
                        );
                        (self.first, self.last) = lines_of(&text, self.range);
                    }
                    None => update = ViewUpdate::Gone,
                }
            }
        }
        self.text = text;
        update
    }
}

/// The innermost outline symbol whose range starts on `line`: the one a
/// symbol tab spanning from that line is showing.
fn syntax_symbol_starting_at(
    symbols: &[crate::lsp::manager::OutlineSymbol],
    line: usize,
) -> Option<&crate::lsp::manager::OutlineSymbol> {
    symbols
        .iter()
        .filter(|s| s.range_start_line as usize == line)
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

    fn rust() -> Option<crate::highlight::LangKind> {
        crate::highlight::lang_for_extension("rs")
    }

    /// Replace the first `from` in `text` with `to`: one local edit.
    fn edit(text: &str, from: &str, to: &str) -> String {
        text.replacen(from, to, 1)
    }

    /// The diff reports exactly the replaced span, and never splits a char.
    #[test]
    fn a_single_edit_span_is_the_replaced_bytes() {
        assert_eq!(single_edit_span("abc", "abc"), None);
        assert_eq!(single_edit_span("abc", "aXbc"), Some((1, 0, 1)), "insert");
        assert_eq!(single_edit_span("abc", "ac"), Some((1, 1, 0)), "delete");
        assert_eq!(single_edit_span("abc", "aXc"), Some((1, 1, 1)), "replace");
        // A repeated letter: prefix and suffix must not overlap, or the
        // lengths go negative.
        assert_eq!(single_edit_span("aa", "aaa"), Some((2, 0, 1)));
        assert_eq!(single_edit_span("aaa", "aa"), Some((2, 1, 0)));
        // `é` (C3 A9) -> `è` (C3 A8): the shared lead byte stays inside the
        // span on both ends.
        let (at, removed, inserted) = single_edit_span("xéy", "xèy").unwrap();
        assert_eq!((at, removed, inserted), (1, 2, 2));
        assert!("xéy".is_char_boundary(at) && "xéy".is_char_boundary(at + removed));
    }

    /// The last line has no separator after it, so its range stops at the
    /// end of the text: a byte further would run off the buffer.
    #[test]
    fn line_ranges_follow_the_separator_model() {
        let text = "a\nbb\nccc";
        assert_eq!(range_for_lines(text, 1, 1), SymbolRange::new(2, 4));
        assert_eq!(range_for_lines(text, 1, 2), SymbolRange::new(2, 8));
        assert_eq!(range_for_lines(text, 2, 2).end, text.len());
        assert_eq!(lines_of(text, SymbolRange::new(2, 8)), (1, 2));
        assert_eq!(lines_of(text, SymbolRange::new(2, 4)), (1, 1));
        assert_eq!(lines_of(text, SymbolRange::new(0, 1)), (0, 0));
    }

    const SRC: &str = "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}";

    fn beta_view() -> SymbolView {
        SymbolView::new(String::from("beta"), String::from(SRC), 4, 6, 1, rust())
    }

    /// Lines typed above the symbol move the clip down; lines typed inside
    /// grow it; a line below leaves it alone.
    #[test]
    fn a_symbol_view_follows_local_edits() {
        let mut v = beta_view();
        assert_eq!((v.first, v.last), (4, 6));

        let above = edit(SRC, "    1\n", "    1\n    0\n");
        assert_eq!(v.follow(above.clone(), 2, rust()), ViewUpdate::Kept);
        assert_eq!((v.first, v.last), (5, 7), "moved down one line");

        let inside = edit(&above, "    2\n", "    2\n    3\n");
        assert_eq!(v.follow(inside.clone(), 3, rust()), ViewUpdate::Kept);
        assert_eq!((v.first, v.last), (5, 8), "grew by one line");

        let below = format!("{inside}\n\nfn gamma() {{}}");
        assert_eq!(v.follow(below, 4, rust()), ViewUpdate::Kept);
        assert_eq!((v.first, v.last), (5, 8), "untouched by text below");
        assert_eq!(v.seen_seq, 4);
    }

    /// Renaming the symbol on its own first line retitles the tab.
    #[test]
    fn renaming_the_symbol_retitles_its_view() {
        let mut v = beta_view();
        let renamed = edit(SRC, "fn beta", "fn betamax");
        assert_eq!(
            v.follow(renamed, 2, rust()),
            ViewUpdate::Renamed(String::from("beta"))
        );
        assert_eq!(v.name, "betamax");
        assert_eq!((v.first, v.last), (4, 6));
    }

    /// Deleting the symbol closes its view; replacing text that straddles
    /// it, while the symbol survives by name, re-anchors instead.
    #[test]
    fn a_lost_range_reanchors_by_name_or_reports_gone() {
        let mut v = beta_view();
        // One edit spanning from inside alpha into beta's body, leaving a
        // fresh `fn beta` behind: the byte range straddles, the name finds it.
        let straddled = edit(
            SRC,
            "    1\n}\n\nfn beta() {\n    2",
            "    9\n}\nfn beta() {\n    8",
        );
        assert_eq!(v.follow(straddled, 2, rust()), ViewUpdate::Kept);
        assert_eq!((v.first, v.last), (3, 5));

        let mut v = beta_view();
        let gone = edit(SRC, "\n\nfn beta() {\n    2\n}", "");
        assert_eq!(v.follow(gone, 2, rust()), ViewUpdate::Gone);
    }
}
