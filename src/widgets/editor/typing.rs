//! Typing and deleting: inserting and pasting text, overwrite mode, auto-closed pairs,
//! quotes and tags, and what completion needs to know about the caret.

use super::*;

/// What the auto-closing-pairs rules (#121) make of one typed character at
/// one caret, decided apart from doing it so a single caret and every caret
/// of a multi-cursor edit follow the same rules (#1597).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PairAction {
    /// Wrap the caret's selection in the opener or quote and this closer.
    Surround(char),
    /// Step over the same closer or quote already at the caret.
    TypeOver,
    /// Insert the opener with this closer after it, the caret between.
    InsertPair(char),
    /// An ordinary insert.
    Plain,
}

impl Editor {
    /// Identifier chars immediately to the left of the cursor on the
    /// current line. Used as the LSP completion popup's filter prefix
    /// so the menu narrows to items the user is actually typing toward.
    pub fn word_before_cursor(&self) -> String {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return String::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let end = self.cursor_col.min(chars.len());
        let mut start = end;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        chars[start..end].iter().collect()
    }

    /// Whether the caret is in code, where completion opens as a word is
    /// typed (#929): not in a string literal or a comment, nor in Markdown,
    /// all of which VS Code's `editor.quickSuggestions` leaves off too.
    pub fn caret_in_code(&self) -> bool {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return false;
        };
        let caret = char_byte(line, self.cursor_col);
        self.lang != Some(LangKind::Markdown)
            && self
                .earlier_construct_end(self.cursor_row, caret)
                .is_some_and(|from| !ends_in_string_or_comment(&line[from..caret], self.lang))
    }

    pub fn insert_char(&mut self, c: char) {
        if self.overwrite_applies() {
            self.overwrite_char(c);
            return;
        }
        if self.auto_close_tags && c == '>' && self.insert_closing_tag() {
            // Still a keystroke, like the pair path below: on-type
            // formatting (#254) keys off the typed character.
            self.last_typed = Some((c, self.edit_seq));
            return;
        }
        if self.auto_close_pairs && self.insert_char_with_pairs(c) {
            // Still a keystroke: on-type formatting (#254) keys off the
            // typed character, whether or not the pair machinery handled it.
            self.last_typed = Some((c, self.edit_seq));
            return;
        }
        self.pin_on_edit();
        // Selection-replace counts as one logical edit (Replace), not two.
        // Coalesce subsequent typed chars onto the same step only when the
        // previous edit was also a single-char insert with no selection.
        let had_selection = self.selection.map(|s| s.has_area()).unwrap_or(false);
        let kind = if had_selection {
            EditKind::DeleteSelection
        } else {
            EditKind::InsertChar
        };
        self.push_undo(kind);
        self.delete_selection_inner();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.mark_buffer_changed();
        self.recompute_highlights();
        // On-type formatting (#254) reads this to know the last edit was
        // this keystroke (seq must still match at the tick). Set here, not
        // in insert_char_raw: paste and snippet expansion go through the
        // raw path and must never count as typing.
        self.last_typed = Some((c, self.edit_seq));
    }

    /// Whether a typed character should overwrite rather than insert: the
    /// mode is on, there is a character to the right of the one caret on its
    /// line (at the end of a line typing appends, as in every editor) and no
    /// selection or extra caret is in play (those keep their insert semantics).
    pub(super) fn overwrite_applies(&self) -> bool {
        self.overwrite
            && self.carets.is_empty()
            && !self.selection.is_some_and(|s| s.has_area())
            && self
                .lines
                .get(self.cursor_row)
                .is_some_and(|l| self.cursor_col < l.chars().count())
    }

    /// Whether `c` typed at the caret joins the cluster just before it: a
    /// combining accent after its letter, a vowel sign after its consonant, a
    /// ZWJ or the next part of an emoji sequence.
    pub(super) fn joins_cluster_before_caret(&self, c: char) -> bool {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return false;
        };
        let byte = self.byte_index(self.cursor_row, self.cursor_col);
        let Some(prev) = line[..byte].graphemes(true).next_back() else {
            return false;
        };
        let mut joined = String::with_capacity(prev.len() + c.len_utf8());
        joined.push_str(prev);
        joined.push(c);
        joined.graphemes(true).nth(1).is_none()
    }

    /// Overwrite-mode typing: replace the character cluster under the caret
    /// with `c` and step past it. A `c` that completes the character before
    /// the caret (see [`Self::joins_cluster_before_caret`]) belongs to it, so
    /// it is added and nothing is replaced. One undo step per keystroke run,
    /// like ordinary typing. Auto-closing pairs and tags are bypassed: a
    /// typed bracket replaces, it does not grow a partner.
    pub(super) fn overwrite_char(&mut self, c: char) {
        self.pin_on_edit();
        self.push_undo(EditKind::InsertChar);
        // A click leaves an empty selection behind; drop it, as the insert
        // path does, or a later Shift+arrow would extend from the click.
        self.selection = None;
        let row = self.cursor_row;
        let from = self.byte_index(row, self.cursor_col);
        let to = if self.joins_cluster_before_caret(c) {
            from
        } else {
            let next = next_grapheme_col(&self.lines[row], self.cursor_col);
            self.byte_index(row, next)
        };
        let mut buf = [0u8; 4];
        self.lines[row].replace_range(from..to, c.encode_utf8(&mut buf));
        // The typed char can join the clusters around it into one (a ZWJ
        // between two emoji, a regional indicator before a flag); step past
        // the whole cluster then, so the next keystroke never lands inside it.
        let col = self.cursor_col + 1;
        let line = &self.lines[row];
        self.cursor_col = if floor_grapheme_col(line, col) == col {
            col
        } else {
            next_grapheme_col(line, col)
        };
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.last_typed = Some((c, self.edit_seq));
    }

    /// Tag auto-close: the `>` keystroke, when the text before the caret is
    /// an unterminated opening HTML/XML/JSX tag, types the `>` AND appends
    /// the matching `</tag>` after it, leaving the caret between the two —
    /// VS Code's `html.autoClosingTags` / `javascript.autoClosingTags`.
    /// Returns true when it handled the keystroke.
    pub(super) fn insert_closing_tag(&mut self) -> bool {
        if self.lines.is_empty() {
            return false;
        }
        // A selection-replace is the ordinary insert's job, not this one.
        if self.selection.is_some_and(|s| s.has_area()) {
            return false;
        }
        let byte = self.byte_index(self.cursor_row, self.cursor_col);
        // A Vue or Svelte component's `<script>` and `<style>` blocks are
        // script and CSS, not markup: a `>` there is a comparison, a generic
        // or a child combinator, never the end of a tag.
        if matches!(self.lang, Some(LangKind::Vue | LangKind::Svelte))
            && in_sfc_script_or_style(&self.lines, self.cursor_row, byte)
        {
            return false;
        }
        let name = match self.lines.get(self.cursor_row) {
            Some(line) => tag_auto_close_name(line, byte, self.lang).map(str::to_owned),
            None => None,
        };
        let Some(name) = name else {
            return false;
        };
        self.pin_on_edit();
        self.push_undo(EditKind::InsertChar);
        let row = self.cursor_row;
        self.lines[row].insert_str(byte, &format!("></{name}>"));
        // The caret lands just past the `>` the user typed, i.e. between the
        // opening and closing tags, which is where they will type next.
        self.cursor_col += 1;
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// The auto-closing-pairs behaviors, returning true when the keystroke
    /// was fully handled here (#121). VS Code's default semantics:
    /// type-over for closers/quotes, selection surround for openers/quotes,
    /// pair insertion guarded so a closer is never jammed into a following
    /// word (and a quote never pairs against a preceding word character —
    /// the apostrophe-in-a-word case).
    pub(super) fn insert_char_with_pairs(&mut self, c: char) -> bool {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        match self.pair_action(c) {
            PairAction::Plain => false,
            PairAction::TypeOver => {
                self.cursor_col += 1;
                self.ensure_cursor_col_visible();
                true
            }
            PairAction::Surround(close) => {
                self.pin_on_edit();
                self.push_undo(EditKind::Paste);
                self.surround_selection_raw(c, close);
                self.recompute_highlights();
                true
            }
            PairAction::InsertPair(close) => {
                self.pin_on_edit();
                self.push_undo(EditKind::InsertChar);
                self.insert_pair_raw(c, close);
                self.recompute_highlights();
                self.auto_pair_at = Some((self.cursor_row, self.cursor_col, self.edit_seq));
                true
            }
        }
    }

    /// The pair rules' decision for `c` typed at the caret and its
    /// selection, changing nothing.
    pub(super) fn pair_action(&self, c: char) -> PairAction {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return PairAction::Plain;
        };
        let close = auto_close_partner(c);
        let has_sel = self.selection.map(|s| s.has_area()).unwrap_or(false);
        // Selection surround: openers and quotes wrap, closers fall through
        // to the ordinary replace-selection insert.
        if has_sel {
            return match close {
                Some(close) if !is_pair_closer(c) => PairAction::Surround(close),
                _ => PairAction::Plain,
            };
        }
        let next = line.chars().nth(self.cursor_col);
        // Type-over: the exact closer/quote already sits at the caret.
        if (is_pair_closer(c) || is_pair_quote(c)) && next == Some(c) {
            return PairAction::TypeOver;
        }
        let Some(close) = close else {
            return PairAction::Plain;
        };
        if is_pair_closer(c) {
            return PairAction::Plain;
        }
        // Opener guard: VS Code's `autoCloseBefore`, so only before
        // whitespace, a closer or punctuation. Anything else, a string's
        // closing quote above all, means the opener is text: `"("` typed
        // inside a string left a stray `)` (#1183). Quote guard: never
        // before or after a word character or the same quote, unless the
        // word before is a Python string prefix, which is part of the opener
        // (`f"` pairs to `f""`, as VS Code's Python configuration lists it).
        let next_ok = match is_pair_quote(c) {
            true => next.is_none_or(|n| !n.is_alphanumeric() && n != '_' && n != c),
            false => next.is_none_or(|n| AUTO_CLOSE_BEFORE.contains(n)),
        };
        let before = &line[..self.byte_index(self.cursor_row, self.cursor_col)];
        let prev_ok = !is_pair_quote(c)
            || before
                .chars()
                .next_back()
                .is_none_or(|p| !p.is_alphanumeric() && p != '_' && p != c)
            || (self.lang == Some(LangKind::Python)
                && is_python_string_prefix(trailing_word(before)));
        // A Rust lifetime or loop label has no closing quote (#1163).
        let lifetime = c == '\'' && self.lang == Some(LangKind::Rust) && rust_lifetime_at(before);
        if !next_ok || !prev_ok || lifetime {
            return PairAction::Plain;
        }
        // A quote typed inside a string or comment is text there, or that
        // string's closing quote, never an opener: pairing the closing `"`
        // of `print(f"{x}` left a stray `")` behind (#844). One opened on an
        // earlier line (a docstring body, a block comment) comes from the
        // highlight pass; the line scan starts where it closes.
        if is_pair_quote(c) {
            let Some(from) = self.earlier_construct_end(self.cursor_row, before.len()) else {
                return PairAction::Plain;
            };
            if ends_in_string_or_comment(&before[from..], self.lang) {
                return PairAction::Plain;
            }
        }
        PairAction::InsertPair(close)
    }

    /// Wrap the selection in `open` and `close`, keeping the text inside
    /// selected and the caret at its end. No undo step, no re-highlight.
    pub(super) fn surround_selection_raw(&mut self, open: char, close: char) {
        let Some(sel) = self.selection else {
            return;
        };
        let ((sr, sc), (er, ec)) = sel.normalised();
        let close_byte = self.byte_index(er, ec);
        self.lines[er].insert(close_byte, close);
        let open_byte = self.byte_index(sr, sc);
        self.lines[sr].insert(open_byte, open);
        let inner_end = if sr == er { ec + 1 } else { ec };
        self.selection = Some(EditorSelection {
            anchor: (sr, sc + 1),
            head: (er, inner_end),
        });
        self.cursor_row = er;
        self.cursor_col = inner_end;
        self.mark_buffer_changed();
    }

    /// Insert `open` and `close` at the caret, the caret between them. No
    /// undo step, no re-highlight.
    pub(super) fn insert_pair_raw(&mut self, open: char, close: char) {
        let row = self.cursor_row;
        let byte = self.byte_index(row, self.cursor_col);
        self.lines[row].insert(byte, close);
        self.lines[row].insert(byte, open);
        self.cursor_col += 1;
        self.mark_buffer_changed();
    }

    /// Where line `row` leaves a string or comment that an earlier line
    /// opened, as a byte index into the line: `Some(0)` when none reaches the
    /// line, and `None` while one still covers `caret` (a byte index too).
    /// Read from the last highlight pass, since a scan of one line cannot
    /// know it starts inside a docstring or a block comment (#844). While an
    /// edit's pass is still out (#850) nothing is read: `Some(0)`, so the
    /// caret's line alone decides, as it did before #844.
    pub(super) fn earlier_construct_end(&self, row: usize, caret: usize) -> Option<usize> {
        if !self.protected_current {
            return Some(0);
        }
        let line_start: usize = self.lines[..row].iter().map(|l| l.len() + 1).sum();
        let first = self
            .protected_ranges
            .partition_point(|&(_, end)| end <= line_start);
        match self.protected_ranges.get(first) {
            Some(&(start, end)) if start < line_start => {
                if end > line_start + caret {
                    None
                } else {
                    Some(end - line_start)
                }
            }
            _ => Some(0),
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        self.insert_str_as(s, crate::provenance::Seat::Me);
    }

    /// A paste from the clipboard (#1218). The text goes in verbatim as one
    /// undo step; then, when `reindent` is on and it spans lines in a code
    /// buffer, a second step moves lines 2.. under the caret line's
    /// indentation, keeping their indentation relative to each other (Zed's
    /// `auto_indent_on_paste`). One Ctrl+Z therefore gives back the verbatim
    /// text. Pasting a loop into a nested Python block left its body at the
    /// clipboard's absolute indentation, outside the block.
    pub fn paste_str(&mut self, s: &str, reindent: bool) {
        let text = normalize_newlines(s);
        let (sr, sc) = match self.selection.filter(|sel| sel.has_area()) {
            Some(sel) => sel.normalised().0,
            None => (self.cursor_row, self.cursor_col),
        };
        let line = self.lines.get(sr).cloned().unwrap_or_default();
        let before: String = line.chars().take(sc).collect();
        // A caret inside a string or comment takes the paste as text: shifting
        // its later lines would change the string's value. Read before the
        // insert, while the highlight pass still describes this buffer.
        let protected = self
            .earlier_construct_end(sr, before.len())
            .is_none_or(|from| ends_in_string_or_comment(&before[from..], self.lang));
        self.insert_str(&text);
        let code = !matches!(self.lang, None | Some(LangKind::Markdown));
        // At column zero there is no caret indentation to fit, so the paste
        // keeps the clipboard's own indentation.
        if !reindent || !code || sc == 0 || protected || !text.contains('\n') {
            return;
        }
        let at_indent = before.chars().all(char::is_whitespace);
        let base = match at_indent {
            true => before,
            false => line.chars().take_while(|c| c.is_whitespace()).collect(),
        };
        let (tab, spaces) = self.indent_preference();
        let edits = paste_reindent_edits(&text, sr, sc, &base, at_indent, tab, spaces);
        if edits.is_empty() {
            return;
        }
        let last = sr + text.matches('\n').count();
        let shift: isize = edits
            .iter()
            .filter(|e| e.start.0 == last)
            .map(|e| e.new_text.chars().count() as isize - (e.end.1 - e.start.1) as isize)
            .sum();
        self.apply_span_edits(&edits);
        self.cursor_row = last;
        self.cursor_col = self.cursor_col.saturating_add_signed(shift);
        self.clamp_cursor();
    }

    /// [`Self::insert_str`], attributing the inserted lines to `seat` (#349).
    ///
    /// The seat is a parameter rather than editor state because the same
    /// buffer takes text from several of them — the person typing, the
    /// navigator's accepted stream, an agent's write — and which one is
    /// making THIS edit is known only at the call site.
    pub fn insert_str_as(&mut self, s: &str, seat: crate::provenance::Seat) {
        self.insert_str_as_in(s, seat, None);
    }

    /// [`Self::insert_str_as`] in the undo step `group`, joined as by
    /// [`Self::apply_span_edits_as_in`]: a snippet completion's body goes in
    /// the step of the edit that accepted it (#928).
    pub fn insert_str_as_in(&mut self, s: &str, seat: crate::provenance::Seat, group: Option<u64>) {
        self.pin_on_edit();
        self.push_undo_step(EditKind::Paste, group);
        // A selection is deleted FIRST, and the map has to learn about that
        // deletion separately: without this, the lines below a multi-line
        // selection keep their old indices while the buffer's lines shift up,
        // so a surviving line paints as whoever wrote a different one. That
        // is the module's one forbidden outcome, reachable by an ordinary
        // select-and-type.
        if let Some(sel) = self.selection {
            let ((sr, _), (er, _)) = sel.normalised();
            if self.delete_selection_inner() {
                // The selected span collapses to a single line.
                self.provenance.splice(sr, er.saturating_sub(sr) + 1, 1);
            }
        }
        let start_line = self.cursor_row;
        let before = self.lines.len();
        // One bookmark sync for the whole paste, diffed against the text
        // `push_undo` recorded above, not one per character.
        self.bookmark_sync_paused = true;
        // CRLF and bare CR are line breaks too: only `\n` was, so a paste
        // from Windows text (or a terminal sending CR) left a raw `\r` inside
        // lines, which a save then wrote as `\r\r\n`.
        for c in normalize_newlines(s).chars() {
            if c == '\n' {
                self.insert_newline_raw();
            } else {
                self.insert_char_raw(c);
            }
        }
        self.bookmark_sync_paused = false;
        self.sync_bookmarks_to_buffer();
        // The line the insertion started on is rewritten too, so it counts as
        // written by this seat — hence the `+ 1`. But clamp to the lines that
        // actually EXIST: inserting into an empty buffer pushes the first
        // line, so the count moves 0 -> 1 and a naive `added + 1` claims two
        // lines when there is one. Attributing a line that is not there is
        // precisely what the module's invariant forbids, so the clamp is the
        // invariant rather than defensiveness.
        let added = self.lines.len().saturating_sub(before);
        let end = (start_line + added + 1).min(self.lines.len());
        self.provenance
            .splice(start_line, 1, end.saturating_sub(start_line));
        self.provenance.record(start_line..end, seat);
        // "No key is ever past the end" as a structural guarantee rather than
        // an argument about arithmetic: cheap, and it holds across every path
        // into this function at once instead of one proof per path.
        self.provenance.truncate(self.lines.len());
        self.recompute_highlights();
    }

    pub(super) fn insert_char_raw(&mut self, c: char) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.mark_buffer_changed();
    }

    pub fn backspace(&mut self) {
        self.backspace_in(None);
    }

    /// [`Self::backspace`] in the undo step `group`: a grouped snippet's
    /// typed prefix goes in the step its body joins (#928).
    pub(super) fn backspace_in(&mut self, group: Option<u64>) {
        if self.at_symbol_edge(false) {
            return;
        }
        self.pin_on_edit();
        self.push_undo_step(EditKind::Backspace, group);
        // Between an empty auto-close pair, backspace eats both sides —
        // typing `(` then backspace must round-trip to nothing (#121).
        // ONLY for the pair the last auto-close inserted, still untouched
        // (position and edit_seq both match): a pre-existing `()` in the
        // file keeps its closer (#122 review).
        if self.auto_close_pairs
            && !self.selection.map(|s| s.has_area()).unwrap_or(false)
            && self.cursor_col > 0
            && self.auto_pair_at == Some((self.cursor_row, self.cursor_col, self.edit_seq))
        {
            let line = self.lines.get(self.cursor_row).cloned().unwrap_or_default();
            let prev = line.chars().nth(self.cursor_col - 1);
            let next = line.chars().nth(self.cursor_col);
            if let (Some(p), Some(n)) = (prev, next)
                && auto_close_partner(p) == Some(n)
                && !is_pair_closer(p)
            {
                let byte = self.byte_index(self.cursor_row, self.cursor_col);
                self.lines[self.cursor_row].remove(byte);
            }
        }
        self.backspace_raw();
        self.recompute_highlights();
    }

    /// Backspace body without snapshotting or re-highlighting. Callers that
    /// batch multiple edits (multi-cursor) snapshot once and recompute once.
    pub(super) fn backspace_raw(&mut self) {
        if self.delete_selection_inner() || self.at_symbol_edge(false) {
            return;
        }
        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let col = prev_grapheme_col(&self.lines[row], self.cursor_col);
            let from = self.byte_index(row, col);
            let to = self.byte_index(row, self.cursor_col);
            self.lines[row].replace_range(from..to, "");
            self.cursor_col = col;
            self.mark_buffer_changed();
        } else if self.cursor_row > 0 {
            let cur = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
            self.lines[self.cursor_row].push_str(&cur);
            self.mark_buffer_changed();
        }
    }

    pub fn delete_forward(&mut self) {
        if self.at_symbol_edge(true) {
            return;
        }
        self.pin_on_edit();
        self.push_undo(EditKind::DeleteForward);
        self.delete_forward_raw();
        self.recompute_highlights();
    }

    /// Forward-delete body without snapshotting or re-highlighting.
    pub(super) fn delete_forward_raw(&mut self) {
        if self.delete_selection_inner() || self.at_symbol_edge(true) {
            return;
        }
        let row = self.cursor_row;
        let len = self.line_char_len(row);
        if self.cursor_col < len {
            let from = self.byte_index(row, self.cursor_col);
            let next = next_grapheme_col(&self.lines[row], self.cursor_col);
            let to = self.byte_index(row, next);
            self.lines[row].replace_range(from..to, "");
            self.mark_buffer_changed();
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
            self.mark_buffer_changed();
        }
    }

    /// VS Code's Delete Word Left (`Ctrl+Backspace`, `Opt+Backspace` on
    /// macOS): delete back to where `Ctrl+Left` stops on this line, or the
    /// selection when there is one. At the line's start it joins the line
    /// above, as Backspace does.
    pub fn delete_word_left(&mut self) {
        let col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        if col == 0 || self.selection.is_some_and(|s| s.has_area()) {
            self.backspace();
            return;
        }
        let start = word_start_before(&self.lines[self.cursor_row], col);
        self.delete_cols_on_line(EditKind::Backspace, start, col);
    }

    /// VS Code's Delete Word Right (`Ctrl+Delete`, `Opt+Delete` on macOS):
    /// delete forward to where `Ctrl+Right` stops on this line, or the
    /// selection. At the line's end it joins the line below, as Delete does.
    pub fn delete_word_right(&mut self) {
        let len = self.line_char_len(self.cursor_row);
        if self.cursor_col >= len || self.selection.is_some_and(|s| s.has_area()) {
            self.delete_forward();
            return;
        }
        let end = word_end_after(&self.lines[self.cursor_row], self.cursor_col);
        self.delete_cols_on_line(EditKind::DeleteForward, self.cursor_col, end);
    }

    /// Remove chars `from..to` of the caret's line as one undo step, leaving
    /// the caret at `from`.
    pub(super) fn delete_cols_on_line(&mut self, kind: EditKind, from: usize, to: usize) {
        self.pin_on_edit();
        self.push_undo(kind);
        let row = self.cursor_row;
        let (a, b) = (self.byte_index(row, from), self.byte_index(row, to));
        self.lines[row].replace_range(a..b, "");
        self.cursor_col = from;
        self.selection = None;
        self.mark_buffer_changed();
        self.recompute_highlights();
    }
}

/// HTML void elements: they never take a closing tag, so typing `>` after
/// one must not produce `</br>` and friends.
pub(super) const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Languages where `>` closes a tag. TypeScript (`.ts`) is deliberately
/// absent — it has no JSX, so every `<` there is a comparison or a generic.
pub(super) fn lang_auto_closes_tags(lang: Option<LangKind>) -> bool {
    matches!(
        lang,
        Some(
            LangKind::Html
                | LangKind::Xml
                | LangKind::Tsx
                | LangKind::JavaScript
                | LangKind::Vue
                | LangKind::Svelte
        )
    )
}

/// Whether the caret at (`row`, `byte`) of a Vue or Svelte component sits
/// inside a `<script>` or `<style>` block: some earlier opening tag of one,
/// with its `>` typed, has no matching closing tag before the caret. A
/// lexical scan, like the rest of tag auto-close, so it errs toward "inside"
/// only where markup is unlikely anyway.
pub(super) fn in_sfc_script_or_style(lines: &[String], row: usize, byte: usize) -> bool {
    let mut inside = false;
    for (r, line) in lines.iter().enumerate().take(row + 1) {
        let text = if r == row {
            line.get(..byte).unwrap_or(line)
        } else {
            line.as_str()
        };
        let lower = text.to_ascii_lowercase();
        let mut from = 0;
        while let Some(off) = lower[from..].find('<') {
            let at = from + off;
            let tail = &lower[at + 1..];
            let (closing, name_part) = match tail.strip_prefix('/') {
                Some(t) => (true, t),
                None => (false, tail),
            };
            let named = ["script", "style"].iter().any(|n| {
                name_part.strip_prefix(n).is_some_and(|after| {
                    after.is_empty() || after.starts_with(|c: char| c == '>' || c.is_whitespace())
                })
            });
            if named {
                if closing {
                    inside = false;
                } else if tail.contains('>') {
                    // Only a finished opening tag starts the block; the one
                    // still being typed (`<script lang="ts"`) does not.
                    inside = true;
                }
            }
            from = at + 1;
        }
    }
    inside
}

/// The tag name a `>` typed at `byte` in `line` should close, or `None` when
/// the text before the caret is not an unterminated opening tag.
///
/// Heuristic, not a parse: the editor decides this on one keystroke against
/// one line, so every rule below errs toward NOT closing — a missed close
/// costs one typed `</tag>`, a spurious one corrupts code. In particular the
/// "`<` may not follow a word character" rule is what keeps generic calls
/// (`foo<T>`, `Array<string>`) and comparisons (`a<b`) from sprouting closing
/// tags. What one line cannot show (a string or block comment opened on an
/// earlier line) is not seen.
pub(super) fn tag_auto_close_name(line: &str, byte: usize, lang: Option<LangKind>) -> Option<&str> {
    if !lang_auto_closes_tags(lang) {
        return None;
    }
    let before = line.get(..byte)?;
    let lt = before.rfind('<')?;
    let inner = &before[lt + 1..];
    // A closing tag (`</p`), a comment or doctype (`<!`), or an XML
    // processing instruction (`<?`) closes nothing.
    if inner.starts_with(['/', '!', '?']) {
        return None;
    }
    // The author is self-closing it themselves: `<br/`, `<Foo /`.
    if inner.ends_with('/') {
        return None;
    }
    // `<` directly after a word character is a comparison or a generic
    // argument list, never markup: `a<b`, `foo<T`, `Array<string`.
    if before[..lt]
        .chars()
        .next_back()
        .is_some_and(|p| p.is_alphanumeric() || p == '_')
    {
        return None;
    }
    // In script, a `<` inside a string literal or a comment is text, not
    // JSX: `"<div"`, `// renders <div`.
    if matches!(lang, Some(LangKind::Tsx | LangKind::JavaScript))
        && ends_in_script_string_or_comment(&before[..lt])
    {
        return None;
    }
    let name_len = inner
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')))
        .unwrap_or(inner.len());
    let name = &inner[..name_len];
    if !name
        .chars()
        .next()
        .is_some_and(|f| f.is_alphabetic() || f == '_')
    {
        return None;
    }
    // Whatever follows the name must look like the inside of a tag: nothing
    // at all, or attributes after whitespace. This rejects the standalone
    // generic declaration `<T,>(x: T) => x`, whose `,` is not tag syntax.
    let rest = &inner[name.len()..];
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    // The generic arrow's other two forms, `<T extends U>` and `<T = U>`,
    // also put whitespace after the name. No attribute list opens with `=`,
    // and `extends` is TypeScript's constraint keyword, not a JSX attribute.
    if matches!(lang, Some(LangKind::Tsx | LangKind::JavaScript)) {
        let attrs = rest.trim_start();
        let first = attrs
            .split(|c: char| c.is_whitespace() || c == '=')
            .next()
            .unwrap_or("");
        if attrs.starts_with('=') || first == "extends" {
            return None;
        }
    }
    // A `>` typed inside a quoted attribute value (`title="1`) or a `{…}`
    // expression (`onClick={() =`, the arrow) belongs to that value, not to
    // the tag. Track quote and brace state across the attribute text and
    // close only when the caret sits at the top level of the tag.
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    for c in rest.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            },
        }
    }
    if quote.is_some() || depth > 0 {
        return None;
    }
    // Already closed: the `>` was deleted and is being retyped before an
    // existing one (`<div|>`), or the end tag is already there on the line
    // (`<div|</div>`). Closing again would double it.
    let after = &line[byte..];
    if after.trim_start().starts_with('>') || after.contains(&format!("</{name}>")) {
        return None;
    }
    // A `>` already inside means that `<` is closed and the caret is past
    // the tag (or inside an attribute value holding a `>`); either way this
    // keystroke is not closing THIS tag.
    if rest.contains('>') {
        return None;
    }
    if matches!(
        lang,
        Some(LangKind::Html | LangKind::Vue | LangKind::Svelte)
    ) && VOID_ELEMENTS.iter().any(|v| v.eq_ignore_ascii_case(name))
    {
        return None;
    }
    Some(name)
}

/// Whether the end of `prefix`, one line of JavaScript/TSX, sits inside a
/// string literal (`'`, `"` or a template literal), a `//` comment or an
/// unclosed `/* */` comment. A backslash escapes the next character inside a
/// string. JSX text containing an apostrophe (`<p>Don't <b`) reads as an open
/// string, which only ever suppresses a close — the safe direction.
pub(super) fn ends_in_script_string_or_comment(prefix: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut block_comment = false;
    let mut chars = prefix.chars().peekable();
    while let Some(c) = chars.next() {
        if block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_comment = false;
            }
        } else if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                '"' | '\'' | '`' => quote = Some(c),
                '/' if chars.peek() == Some(&'/') => return true,
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    block_comment = true;
                }
                _ => {}
            }
        }
    }
    quote.is_some() || block_comment
}

/// Whether the end of `prefix`, the current line up to the caret, sits inside
/// a string literal or a comment of `lang`, where a typed quote is never
/// paired (#844). A lexical scan of the one line rather than the syntax tree:
/// the string being typed is unterminated, which is exactly where a parser's
/// error recovery is least trustworthy. A string or block comment opened on
/// an earlier line goes unseen, which leaves pairing there as it always was.
pub(super) fn ends_in_string_or_comment(prefix: &str, lang: Option<LangKind>) -> bool {
    let line_comment = line_comment_token(lang);
    // Python's "block comment" is a triple-quoted string, scanned as one.
    let block_comment = block_comment_tokens(lang).filter(|_| lang != Some(LangKind::Python));
    // A shell or YAML `#` opens a comment only at the start of a word:
    // `${#a[@]}` and `a#b` are not comments.
    let hash_needs_space = matches!(lang, Some(LangKind::Bash | LangKind::Yaml));
    let mut quote: Option<(char, bool)> = None; // (delimiter, triple-quoted)
    let mut in_block = false;
    let mut i = 0;
    while let Some(c) = prefix[i..].chars().next() {
        let rest = &prefix[i..];
        let mut step = c.len_utf8();
        if in_block {
            if let Some((_, close)) = block_comment
                && rest.starts_with(close)
            {
                in_block = false;
                step = close.len();
            }
        } else if let Some((q, triple)) = quote {
            if c == '\\' && backslash_escapes(q, lang) {
                step += rest[step..].chars().next().map_or(0, char::len_utf8);
            } else if c == q && (!triple || rest.chars().take(3).eq([q, q, q])) {
                quote = None;
                if triple {
                    step = 3;
                }
            }
        } else if let Some((open, _)) = block_comment
            && rest.starts_with(open)
        {
            in_block = true;
            step = open.len();
        } else if line_comment.is_some_and(|t| rest.starts_with(t))
            && (!hash_needs_space
                || prefix[..i]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace))
        {
            return true;
        } else if is_pair_quote(c) && quote_opens_string(&prefix[..i], rest, lang) {
            let triple =
                lang == Some(LangKind::Python) && c != '`' && rest.chars().take(3).eq([c, c, c]);
            quote = Some((c, triple));
            if triple {
                step = 3;
            }
        }
        i += step;
    }
    quote.is_some() || in_block
}

/// Whether the quote that starts `rest` opens a string, given the line text
/// `before` it. A `"` always does. A `'` or backtick after a word character is
/// an apostrophe (`don't`, C++'s `1'000`) unless that word is a Python string
/// prefix, and a Rust `'` opens a char literal, never a lifetime or a label.
pub(super) fn quote_opens_string(before: &str, rest: &str, lang: Option<LangKind>) -> bool {
    let mut chars = rest.chars();
    let q = chars.next();
    if q == Some('"') {
        return true;
    }
    let word = trailing_word(before);
    let prefixed = lang == Some(LangKind::Python) && is_python_string_prefix(word);
    if !word.is_empty() && !prefixed {
        return false;
    }
    if lang == Some(LangKind::Rust) && q == Some('\'') {
        return matches!(
            (chars.next(), chars.next()),
            (None, _) | (Some('\\'), _) | (Some(_), Some('\''))
        );
    }
    true
}

/// Whether a `'` typed after `before` on a Rust line starts a lifetime or a
/// loop label, which take no closing quote (#1163): after `<` or `&`
/// (`impl<'a>`, `&'a str`), `+` (`T + 'static`), `break`, `continue` or
/// `where`, or after a `,` inside an unclosed `<` (`<'a, 'b>`). Anywhere
/// else it may open a char literal (`f('x')`, `['a', 'b']`), so it pairs.
pub(super) fn rust_lifetime_at(before: &str) -> bool {
    let before = before.trim_end();
    match before.chars().next_back() {
        Some('<') => before[..before.len() - 1]
            .chars()
            .next_back()
            .is_some_and(opens_generics_after),
        Some('&' | '+') => true,
        Some(',') => inside_angle_brackets(before),
        _ => matches!(trailing_word(before), "break" | "continue" | "where"),
    }
}

/// Whether a `<` right after `prev` opens generics (`foo<`, `impl<`,
/// `Vec::<`) rather than comparing (`a < b`): a generic `<` sits right
/// against a name or `::`.
pub(super) fn opens_generics_after(prev: char) -> bool {
    prev.is_alphanumeric() || prev == '_' || prev == ':'
}

/// Whether `s` ends inside an unclosed generic `<`, counting `->` and `=>`
/// as arrows rather than closers and a spaced-out `<` as a comparison.
pub(super) fn inside_angle_brackets(s: &str) -> bool {
    let mut depth = 0i32;
    let mut prev = ' ';
    for c in s.chars() {
        match c {
            '<' if opens_generics_after(prev) => depth += 1,
            '>' if prev != '-' && prev != '=' => depth = (depth - 1).max(0),
            _ => {}
        }
        prev = c;
    }
    depth > 0
}

/// Whether a backslash escapes the next character inside a `q`-quoted string
/// of `lang`. Shell, YAML and TOML single quotes and Go's backtick raw strings
/// take every character literally.
pub(super) fn backslash_escapes(q: char, lang: Option<LangKind>) -> bool {
    !matches!(
        (q, lang),
        ('\'', Some(LangKind::Bash | LangKind::Yaml | LangKind::Toml)) | ('`', Some(LangKind::Go))
    )
}

/// Python's string prefixes (`f"…"`, `rb'…'`, in any case): letters that
/// belong to the string's opening quote rather than being a word before it.
pub(super) fn is_python_string_prefix(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "f" | "r" | "b" | "u" | "rb" | "br" | "fr" | "rf"
    )
}

/// The run of word characters that ends `s`.
pub(super) fn trailing_word(s: &str) -> &str {
    let start = s
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_word_char(c))
        .last()
        .map_or(s.len(), |(i, _)| i);
    &s[start..]
}

/// Apply the selection background colour to columns `[start_char..end_char)`
/// of row `y`, where columns are character indices within the editor's text
/// area.  Clamps to the visible width.
/// The background tint for a row inside a merge-conflict block, or `None`
/// outside one. Header / footer marker rows tint stronger than their
/// content, mirroring VS Code's current(green) / incoming(blue) scheme; the
/// diff3 base section and the `=======` separator sit on a neutral grey.
/// Fixed dark tints, like the selection band: both bundled themes are dark,
/// and the hues are semantic (green = yours, blue = theirs), not accents.
/// VS Code's default auto-closing set: the partner a typed opener/quote
/// pairs with, `None` for everything else (closers included — they only
/// type over).
pub(super) fn auto_close_partner(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

/// The characters an opener may auto-close in front of: VS Code's default
/// `autoCloseBefore` (#1183).
pub(super) const AUTO_CLOSE_BEFORE: &str = ";:.,=}])> \t";

pub(super) fn is_pair_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '}')
}

pub(super) fn is_pair_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}
