//! Several carets at once: adding carets, selecting occurrences, box and column
//! selection, and applying an edit at every caret.

use super::*;

/// A keyboard column selection (#1560): its corners as `(row, col)`, the
/// head's column free to run past a short line's end as VS Code's does,
/// and the carets and selections it left, to tell whether they are still
/// its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ColumnBox {
    anchor: (usize, usize),
    head: (usize, usize),
    left: ColumnBoxCarets,
}

/// The primary caret, primary selection and secondary carets a keyboard
/// column selection left behind.
pub(super) type ColumnBoxCarets = (
    (usize, usize),
    Option<EditorSelection>,
    Vec<EditorSelection>,
);

/// A single keystroke to fan out across every multi-cursor caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaretEdit {
    Insert(char),
    Backspace,
    DeleteForward,
}

/// Caret places, `(row, col)` each.
pub(super) type CaretPlaces = Vec<(usize, usize)>;

impl Editor {
    /// True while multi-cursor mode is active (one or more secondary carets
    /// in addition to the primary cursor).
    pub fn has_multi_cursor(&self) -> bool {
        !self.carets.is_empty()
    }

    /// Drop every secondary caret, leaving just the primary cursor. Called on
    /// Esc, mouse clicks, and any movement that isn't a multi-cursor edit.
    pub fn collapse_carets(&mut self) {
        self.multi_typing = None;
        self.carets.clear();
    }

    /// VS Code "Change All Occurrences" (Cmd+F2): select every whole-word,
    /// case-sensitive textual match of the identifier under the cursor in the
    /// current file and turn each into a caret. The match at the cursor (or
    /// the first, if the cursor isn't on one) becomes the primary selection;
    /// the rest become secondary carets. Returns the number of occurrences
    /// selected (0 when the cursor isn't on a word, leaving state untouched).
    pub fn select_all_occurrences_of_word_at_cursor(&mut self) -> usize {
        self.multi_typing = None;
        let Some((start, end)) = self.word_at(self.cursor_row, self.cursor_col) else {
            return 0;
        };
        let word: Vec<char> = self.lines[self.cursor_row]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        if word.is_empty() {
            return 0;
        }
        let mut occ: Vec<EditorSelection> = Vec::new();
        // A symbol tab matches only inside its symbol.
        let (lo, hi) = self.symbol_clip().unwrap_or((0, self.lines.len()));
        for (row, line) in self.lines.iter().enumerate().take(hi).skip(lo) {
            let chars: Vec<char> = line.chars().collect();
            for col in find_word_occurrences(&chars, &word) {
                occ.push(EditorSelection {
                    anchor: (row, col),
                    head: (row, col + word.len()),
                });
            }
        }
        if occ.is_empty() {
            return 0;
        }
        // The occurrence containing the cursor becomes primary so typing
        // continues where the user is looking; otherwise fall back to the
        // first match in the file.
        let primary_idx = occ
            .iter()
            .position(|s| {
                s.anchor.0 == self.cursor_row
                    && self.cursor_col >= s.anchor.1
                    && self.cursor_col <= s.head.1
            })
            .unwrap_or(0);
        let primary = occ[primary_idx];
        self.selection = Some(primary);
        self.cursor_row = primary.head.0;
        self.cursor_col = primary.head.1;
        self.carets = occ
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i != primary_idx)
            .map(|(_, s)| s)
            .collect();
        self.carets.len() + 1
    }

    /// The start of every occurrence of `word` in the buffer (a symbol tab's
    /// symbol only), in document order: whole words when `whole_word`, else
    /// every case-sensitive plain-text match, left to right without overlap.
    pub(super) fn occurrence_starts(&self, word: &[char], whole_word: bool) -> Vec<(usize, usize)> {
        let (lo, hi) = self.symbol_clip().unwrap_or((0, self.lines.len()));
        let mut occ = Vec::new();
        for (row, line) in self.lines.iter().enumerate().take(hi).skip(lo) {
            let chars: Vec<char> = line.chars().collect();
            let cols = if whole_word {
                find_word_occurrences(&chars, word)
            } else {
                find_text_occurrences(&chars, word)
            };
            occ.extend(cols.into_iter().map(|col| (row, col)));
        }
        occ
    }

    /// VS Code "Select All Occurrences of Find Match" (`Cmd+Shift+L`,
    /// #1558): a selection on every match of the selected text, as plain
    /// text for a hand-made selection and as whole words for one Add
    /// Selection to Next Find Match took from the caret; with no selection,
    /// every whole-word match of the word at the caret (Change All
    /// Occurrences). The selection the caret is in stays primary. Returns
    /// the number of selections, 0 when there is nothing to match.
    pub fn select_all_occurrences(&mut self) -> usize {
        self.multi_typing = None;
        let Some(sel) = self.selection.filter(|s| s.anchor != s.head) else {
            return self.select_all_occurrences_of_word_at_cursor();
        };
        let (start, end) = sel.normalised();
        if start.0 != end.0 {
            // A match across lines is not looked for: the selection stays.
            return 0;
        }
        let word: Vec<char> = self.lines[start.0]
            .chars()
            .skip(start.1)
            .take(end.1 - start.1)
            .collect();
        self.sync_occurrence_word_sel();
        let whole_word = self.occurrence_word_sel.is_some();
        let occ = self.occurrence_starts(&word, whole_word);
        if occ.is_empty() {
            return 0;
        }
        let primary_idx = occ.iter().position(|&o| o == start).unwrap_or(0);
        let as_sel = |(row, col): (usize, usize)| EditorSelection {
            anchor: (row, col),
            head: (row, col + word.len()),
        };
        let primary = as_sel(occ[primary_idx]);
        self.selection = Some(primary);
        (self.cursor_row, self.cursor_col) = primary.head;
        self.carets = occ
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != primary_idx)
            .map(|(_, &o)| as_sel(o))
            .collect();
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// VS Code "Add Cursors to Line Ends" (`Shift+Alt+I`, #1558): a caret at
    /// the end of every line the selection covers, the selection dropped.
    /// A last line the selection only reaches at column 0 is left out, as
    /// VS Code does. The last line's caret is the primary. Returns the
    /// number of carets, 0 with no selection.
    pub fn add_cursors_to_line_ends(&mut self) -> usize {
        self.multi_typing = None;
        let Some(sel) = self.selection.filter(|s| s.anchor != s.head) else {
            return 0;
        };
        let (start, end) = sel.normalised();
        let last = if end.1 == 0 && end.0 > start.0 {
            end.0 - 1
        } else {
            end.0
        };
        let ends: Vec<(usize, usize)> = (start.0..=last)
            .map(|row| (row, self.line_char_len(row)))
            .collect();
        let Some((&primary, rest)) = ends.split_last() else {
            return 0;
        };
        self.selection = None;
        (self.cursor_row, self.cursor_col) = primary;
        self.carets = rest
            .iter()
            .map(|&(row, col)| EditorSelection::new(row, col))
            .collect();
        self.occurrence_word_sel = None;
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// VS Code "Add Selection to Next Find Match" (`Cmd+D`): the first press
    /// selects the word under the cursor; each further press finds the next
    /// occurrence of the selected text (document order, wrapping) that is not
    /// already a caret and adds it as a secondary caret, moving the primary to
    /// it so the view follows. Returns the number of cursors (0 when the first
    /// press lands off a word, leaving state untouched).
    pub fn select_next_occurrence(&mut self) -> usize {
        self.multi_typing = None;
        // First press with no selection: select the word under the cursor.
        let Some(sel) = self.selection else {
            let Some((s, e)) = self.word_at(self.cursor_row, self.cursor_col) else {
                return 0;
            };
            let word = EditorSelection {
                anchor: (self.cursor_row, s),
                head: (self.cursor_row, e),
            };
            self.selection = Some(word);
            self.occurrence_word_sel = Some((word, self.edit_seq));
            self.cursor_col = e;
            self.ensure_cursor_col_visible();
            return 1;
        };
        // Only single-line word selections drive the incremental match search.
        let (start, end) = sel.normalised();
        if start.0 != end.0 {
            return self.carets.len() + 1;
        }
        let word: Vec<char> = self.lines[start.0]
            .chars()
            .skip(start.1)
            .take(end.1 - start.1)
            .collect();
        if word.is_empty() {
            return self.carets.len() + 1;
        }
        // Every occurrence in document order, and the set already selected
        // (the primary plus each existing caret), keyed by its start.
        self.sync_occurrence_word_sel();
        let whole_word = self.occurrence_word_sel.is_some();
        let occ = self.occurrence_starts(&word, whole_word);
        let mut taken: Vec<(usize, usize)> = self.carets.iter().map(|c| c.normalised().0).collect();
        taken.push(start);
        // Scan forward from the primary, wrapping, for the first match not yet
        // taken.
        let from = occ.iter().position(|&o| o == start).unwrap_or(0);
        let n = occ.len();
        let next = (1..=n)
            .map(|k| occ[(from + k) % n])
            .find(|o| !taken.contains(o));
        let Some((row, col)) = next else {
            return self.carets.len() + 1;
        };
        // Demote the current primary to a caret, promote the new match.
        self.carets.push(sel);
        let head = (row, col + word.len());
        let found = EditorSelection {
            anchor: (row, col),
            head,
        };
        self.selection = Some(found);
        if whole_word {
            self.occurrence_word_sel = Some((found, self.edit_seq));
        }
        self.cursor_row = head.0;
        self.cursor_col = head.1;
        self.ensure_cursor_col_visible();
        self.carets.len() + 1
    }

    /// Insert a character at every caret. Consecutive characters typed
    /// with the carets left where they were form one undo step (#913).
    ///
    /// Deliberately does not set `last_typed`: an on-type formatting reply
    /// (#254) is computed at ONE position and would be wrong at the other
    /// carets, so multi-cursor typing never arms the trigger.
    pub fn multi_insert_char(&mut self, c: char) {
        self.multi_apply(CaretEdit::Insert(c));
    }

    /// Backspace at every caret. One undo step.
    pub fn multi_backspace(&mut self) {
        self.multi_apply(CaretEdit::Backspace);
    }

    /// Forward-delete at every caret. One undo step.
    pub fn multi_delete_forward(&mut self) {
        self.multi_apply(CaretEdit::DeleteForward);
    }

    /// Apply `edit` at the primary caret and every secondary caret as a
    /// single undo step, which a typed character extends when it continues
    /// a typing burst. Carets are processed bottom-to-top so an edit never
    /// shifts the coordinates of a caret still to be processed.
    pub(super) fn multi_apply(&mut self, edit: CaretEdit) {
        // A Backspace right after a keystroke that paired at every caret
        // takes both sides of each empty pair, as one caret's does (#1597).
        let armed: Vec<(usize, usize)> = match self.multi_auto_pair.take() {
            Some((seq, places, paired))
                if self.auto_close_pairs
                    && edit == CaretEdit::Backspace
                    && seq == self.edit_seq
                    && places == self.caret_places() =>
            {
                paired
            }
            _ => Vec::new(),
        };
        // A typed character joins the open typing burst only while the
        // carets sit where its last keystroke left them, with no edit since.
        let burst = self.multi_typing.take() == Some((self.edit_seq, self.caret_selections()));
        // A closer or quote stepped over at every caret edits nothing, so it
        // records no undo step, as one caret's type-over does.
        if let CaretEdit::Insert(c) = edit
            && self.auto_close_pairs
            && self.types_over_at_every_caret(c)
        {
            self.selection = None;
            self.cursor_col += 1;
            for s in &mut self.carets {
                s.head.1 += 1;
                s.anchor = s.head;
            }
            self.ensure_cursor_col_visible();
            if burst {
                self.multi_typing = Some((self.edit_seq, self.caret_selections()));
            }
            return;
        }
        self.pin_on_edit();
        let kind = match edit {
            CaretEdit::Insert(_) => EditKind::MultiInsertChar,
            CaretEdit::Backspace | CaretEdit::DeleteForward => EditKind::MultiEdit,
        };
        if !burst {
            self.break_undo_coalescing();
        }
        self.push_undo(kind);

        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        let mut items: Vec<(bool, EditorSelection)> = Vec::with_capacity(self.carets.len() + 1);
        items.push((true, primary));
        for s in &self.carets {
            items.push((false, *s));
        }
        // Descending by start so the lowest caret is edited first.
        items.sort_by_key(|item| std::cmp::Reverse(item.1.normalised().0));
        // Clear so the per-caret raw ops can't see stale carets.
        self.carets.clear();

        // Each caret's place, kept as (rows from the bottom, chars from its
        // line's end): the later edits happen above or to the left of it, and
        // leave everything after it untouched, so those two stay true where a
        // plain (row, col) goes stale on its row (`abcd` with carets at 1 and
        // 3, typing X then Y, gave `aXYbcYXd`).
        // A selection a surround kept (#1597) has its anchor placed the same
        // way: it sits after the later edits too.
        type Place = (usize, usize);
        let mut placed: Vec<(bool, Place, Option<Place>, bool)> = Vec::new();
        for (is_primary, sel) in items {
            if sel.has_area() {
                self.selection = Some(sel);
            } else {
                self.selection = None;
            }
            let last = self.lines.len().saturating_sub(1);
            self.cursor_row = sel.head.0.min(last);
            self.cursor_col = sel.head.1.min(self.line_char_len(self.cursor_row));
            let paired = self.apply_caret_edit_raw(edit, armed.contains(&sel.head));
            let place = |this: &Self, (row, col): (usize, usize)| {
                (
                    this.lines.len().saturating_sub(1) - row,
                    this.line_char_len(row).saturating_sub(col),
                )
            };
            let anchor = self
                .selection
                .filter(|s| s.has_area())
                .map(|s| place(self, s.anchor));
            placed.push((
                is_primary,
                place(self, (self.cursor_row, self.cursor_col)),
                anchor,
                paired,
            ));
        }

        self.selection = None;
        let last = self.lines.len().saturating_sub(1);
        let resolve = |this: &Self, (from_bottom, from_end): (usize, usize)| {
            let row = last.saturating_sub(from_bottom);
            (row, this.line_char_len(row).saturating_sub(from_end))
        };
        let mut new_primary = EditorSelection::new(self.cursor_row, self.cursor_col);
        let mut primary_kept = false;
        let mut new_secondary: Vec<EditorSelection> = Vec::new();
        let mut paired_places: Vec<(usize, usize)> = Vec::new();
        for (is_primary, head, anchor, paired) in placed {
            let head = resolve(self, head);
            if paired {
                paired_places.push(head);
            }
            let sel = EditorSelection {
                anchor: anchor.map_or(head, |a| resolve(self, a)),
                head,
            };
            if is_primary {
                new_primary = sel;
                primary_kept = anchor.is_some();
            } else {
                new_secondary.push(sel);
            }
        }
        self.cursor_row = new_primary.head.0.min(last);
        self.cursor_col = new_primary.head.1.min(self.line_char_len(self.cursor_row));
        if primary_kept {
            self.selection = Some(new_primary);
        }
        // Carets an edit brought together are one caret from here on: kept
        // apart, the next key edited the same spot twice (one Backspace
        // deleting two line breaks).
        let primary = (self.cursor_row, self.cursor_col);
        new_secondary.sort_unstable_by_key(|s| (s.head, s.anchor));
        new_secondary.dedup();
        new_secondary.retain(|s| s.head != primary);
        self.carets = new_secondary;
        self.recompute_highlights();
        if !paired_places.is_empty() {
            self.multi_auto_pair = Some((self.edit_seq, self.caret_places(), paired_places));
        }
        // A surround keeps its selections and is its own step, as one
        // caret's is: the next keystroke opens a new one.
        let carets = self.caret_selections();
        if kind == EditKind::MultiInsertChar && !carets.iter().any(|s| s.has_area()) {
            self.multi_typing = Some((self.edit_seq, carets));
        }
    }

    /// True when `c` typed at every caret only steps over the same closer
    /// or quote: no caret has a selection and each sits right before `c`.
    pub(super) fn types_over_at_every_caret(&self, c: char) -> bool {
        if !(is_pair_closer(c) || is_pair_quote(c)) {
            return false;
        }
        let primary_sel = self.selection.is_some_and(|s| s.has_area());
        !primary_sel
            && self.carets.iter().all(|s| !s.has_area())
            && self
                .caret_places()
                .into_iter()
                .all(|(row, col)| self.lines.get(row).and_then(|l| l.chars().nth(col)) == Some(c))
    }

    /// Every caret's place, the primary first, the extra carets in order.
    pub(super) fn caret_places(&self) -> Vec<(usize, usize)> {
        std::iter::once((self.cursor_row, self.cursor_col))
            .chain(self.carets.iter().map(|c| c.head))
            .collect()
    }

    /// Every caret's selection, the primary first, the extra carets in
    /// order; the primary's is empty at the cursor when it has none.
    pub(super) fn caret_selections(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// One caret's worth of a multi-edit: no snapshot, no re-highlight (the
    /// batching caller in `multi_apply` does both once).
    ///
    /// A typed character follows the same pair rules as one caret's
    /// (#1597): surround, type-over and auto-close; with them skipped, a
    /// quote replaced every Ctrl+D selection and `)` doubled the closer.
    /// `pairs_armed` lets Backspace take both sides of an empty pair the
    /// last keystroke inserted. Returns true when a pair was inserted.
    pub(super) fn apply_caret_edit_raw(&mut self, edit: CaretEdit, pairs_armed: bool) -> bool {
        match edit {
            CaretEdit::Insert(c) => {
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                let action = match self.auto_close_pairs {
                    true => self.pair_action(c),
                    false => PairAction::Plain,
                };
                match action {
                    PairAction::Surround(close) => self.surround_selection_raw(c, close),
                    PairAction::TypeOver => self.cursor_col += 1,
                    PairAction::InsertPair(close) => {
                        self.insert_pair_raw(c, close);
                        return true;
                    }
                    PairAction::Plain => {
                        self.delete_selection_inner();
                        self.insert_char_raw(c);
                    }
                }
            }
            CaretEdit::Backspace => {
                if pairs_armed
                    && !self.selection.is_some_and(|s| s.has_area())
                    && self.cursor_col > 0
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
            }
            CaretEdit::DeleteForward => self.delete_forward_raw(),
        }
        false
    }

    /// Forget the Add Selection to Next Find Match word marker (#1558) once
    /// the selection or the buffer has moved on from it. Called before each
    /// input event and by the occurrence commands, so a selection that left
    /// the marked one and came back (Esc, then the same word selected by
    /// hand) matches as plain text again.
    pub fn sync_occurrence_word_sel(&mut self) {
        if self
            .occurrence_word_sel
            .is_some_and(|(sel, seq)| self.selection != Some(sel) || seq != self.edit_seq)
        {
            self.occurrence_word_sel = None;
        }
    }

    /// The primary selection (a bare caret when there is none) followed by
    /// every extra caret.
    pub(super) fn selections_with_primary(&self) -> Vec<EditorSelection> {
        let primary = self
            .selection
            .unwrap_or_else(|| EditorSelection::new(self.cursor_row, self.cursor_col));
        std::iter::once(primary)
            .chain(self.carets.iter().copied())
            .collect()
    }

    /// Move the primary caret and selection and every extra caret through
    /// `f`, clamp each onto its line, and merge carets that landed together.
    pub(super) fn remap_carets(&mut self, f: impl Fn((usize, usize)) -> (usize, usize)) {
        let last = self.lines.len().saturating_sub(1);
        let clamp = |this: &Self, p: (usize, usize)| {
            let (r, c) = f(p);
            let r = r.min(last);
            (r, c.min(this.line_char_len(r)))
        };
        let map = |this: &Self, s: EditorSelection| EditorSelection {
            anchor: clamp(this, s.anchor),
            head: clamp(this, s.head),
        };
        self.selection = self.selection.map(|s| map(self, s));
        (self.cursor_row, self.cursor_col) = clamp(self, (self.cursor_row, self.cursor_col));
        self.carets = self.carets.iter().map(|&s| map(self, s)).collect();
        self.dedup_carets();
    }

    /// Make `all[0]` the primary selection (or bare caret) and the rest
    /// extra carets.
    pub(super) fn set_selections(&mut self, all: &[EditorSelection]) {
        let Some((&primary, rest)) = all.split_first() else {
            return;
        };
        self.selection = primary.has_area().then_some(primary);
        (self.cursor_row, self.cursor_col) = primary.head;
        self.carets = rest.to_vec();
        self.dedup_carets();
    }

    /// Drop extra carets that sit on the primary or on an earlier caret, and
    /// merge selections that overlap into one, as VS Code does (#1570):
    /// kept apart, the next key would edit the same spot twice, and the
    /// edit at the lower one left the upper one's end stale, so typing over
    /// two overlapping line selections panicked or deleted lines nobody
    /// selected. Selections that only touch stay apart, as does a caret at a
    /// selection's edge. A merged selection keeps the place in the order and
    /// the direction of its first member, so the primary stays the primary.
    pub(super) fn dedup_carets(&mut self) {
        let all = self.selections_with_primary();
        let mut by_start: Vec<usize> = (0..all.len()).collect();
        by_start.sort_by_key(|&i| all[i].normalised());
        // Each run of overlapping selections: (first member, start, end).
        let mut runs: Vec<(usize, BPos, BPos)> = Vec::with_capacity(all.len());
        for i in by_start {
            let (start, end) = all[i].normalised();
            match runs.last_mut() {
                Some(run) if start < run.2 || (start, end) == (run.1, run.2) => {
                    run.0 = run.0.min(i);
                    run.2 = run.2.max(end);
                }
                _ => runs.push((i, start, end)),
            }
        }
        runs.sort_unstable_by_key(|run| run.0);
        let mut merged = runs.into_iter().map(|(i, start, end)| {
            let (anchor, head) = if all[i].anchor <= all[i].head {
                (start, end)
            } else {
                (end, start)
            };
            EditorSelection { anchor, head }
        });
        let primary = merged.next().expect("the primary is always first");
        if primary != all[0] {
            self.selection = primary.has_area().then_some(primary);
            (self.cursor_row, self.cursor_col) = primary.head;
        }
        self.carets = merged.collect();
    }

    /// A position kept as (rows from the last line, chars from its line's
    /// end). Edits above it, or left of it on its line, leave both true, so
    /// a command that edits from the bottom of the buffer up pins each
    /// caret as it passes and finds it again at the end.
    pub(super) fn pin_from_end(&self, (row, col): (usize, usize)) -> (usize, usize) {
        let last = self.lines.len().saturating_sub(1);
        (
            last.saturating_sub(row),
            self.line_char_len(row).saturating_sub(col),
        )
    }

    /// The position `pin_from_end` kept, in the buffer as it is now.
    pub(super) fn unpin_from_end(&self, (from_bottom, from_end): (usize, usize)) -> (usize, usize) {
        let row = self
            .lines
            .len()
            .saturating_sub(1)
            .saturating_sub(from_bottom);
        (row, self.line_char_len(row).saturating_sub(from_end))
    }

    /// Put a bare caret at each pinned position, the one marked primary as
    /// the primary caret.
    pub(super) fn place_pinned_carets(&mut self, placed: &[(bool, (usize, usize))]) {
        let mut all: Vec<EditorSelection> = placed
            .iter()
            .map(|&(_, pin)| {
                let (r, c) = self.unpin_from_end(pin);
                EditorSelection::new(r, c)
            })
            .collect();
        if let Some(i) = placed.iter().position(|&(primary, _)| primary) {
            all.swap(0, i);
        }
        self.set_selections(&all);
    }

    /// VS Code "Add Cursor Below" (Cmd+Alt+Down): drop a secondary caret on
    /// the row below the lowest current caret, at the same column (clamped to
    /// that line's length). No-op at the last row.
    pub fn add_cursor_below(&mut self) {
        self.multi_typing = None;
        let max_row = self
            .carets
            .iter()
            .map(|c| c.head.0)
            .chain(std::iter::once(self.cursor_row))
            .max()
            .unwrap_or(self.cursor_row);
        let target = max_row + 1;
        if target >= self.symbol_clip().map_or(self.lines.len(), |(_, end)| end) {
            return;
        }
        let col = self.cursor_col.min(self.line_char_len(target));
        self.carets.push(EditorSelection {
            anchor: (target, col),
            head: (target, col),
        });
    }

    /// VS Code Alt+click: add a caret at the screen cell `(col, row)`, keeping
    /// the existing cursor(s). The old primary (with any selection) becomes a
    /// caret and the clicked point becomes the new primary. Clicking an
    /// existing zero-width caret removes it (toggle). Returns false when the
    /// click misses the text area.
    pub fn add_caret_at_screen(&mut self, col: u16, row: u16) -> bool {
        self.multi_typing = None;
        let Some((r, c)) = self.buffer_pos_at(col, row) else {
            return false;
        };
        // Clicking the current primary is a no-op (nothing to add).
        if (r, c) == (self.cursor_row, self.cursor_col) {
            return true;
        }
        // Clicking an existing zero-width caret removes it (toggle off).
        if let Some(idx) = self
            .carets
            .iter()
            .position(|s| !s.has_area() && s.head == (r, c))
        {
            self.carets.remove(idx);
            return true;
        }
        // Demote the current primary (preserving any selection) to a caret and
        // promote the clicked point.
        let primary = self.selection.unwrap_or(EditorSelection {
            anchor: (self.cursor_row, self.cursor_col),
            head: (self.cursor_row, self.cursor_col),
        });
        self.carets.push(primary);
        self.selection = None;
        self.cursor_row = r;
        self.cursor_col = c;
        self.ensure_cursor_col_visible();
        true
    }

    /// Begin a column (box) selection anchored at the screen cell `(col, row)`
    /// (VS Code's Shift+Alt+drag). Clears any existing selection/carets and
    /// parks the primary at the anchor. Returns false if the cell misses text.
    pub fn begin_box_select(&mut self, col: u16, row: u16) -> bool {
        self.multi_typing = None;
        let Some((r, c)) = self.buffer_pos_at(col, row) else {
            return false;
        };
        self.box_anchor = Some((r, c));
        self.carets.clear();
        self.selection = None;
        self.cursor_row = r;
        self.cursor_col = c;
        true
    }

    /// Whether a column (box) selection drag is in progress.
    pub fn box_selecting(&self) -> bool {
        self.box_anchor.is_some()
    }

    /// Extend the live column selection to the screen cell `(col, row)`,
    /// rebuilding one caret per spanned row. No-op if no box drag is active.
    pub fn box_drag_to_screen(&mut self, col: u16, row: u16) {
        if self.box_anchor.is_none() {
            return;
        }
        if let Some((r, c)) = self.buffer_pos_at(col, row) {
            self.box_select_to(r, c);
        }
    }

    /// Rebuild the column selection so it spans the rectangle from the box
    /// anchor to `(head_row, head_col)`: every row in the range gets a caret
    /// over the shared column span (clamped to that line's length); the head
    /// row is the primary. Zero-width columns become bare carets.
    pub(super) fn box_select_to(&mut self, head_row: usize, head_col: usize) {
        self.multi_typing = None;
        let Some((ar, ac)) = self.box_anchor else {
            return;
        };
        let (r0, r1) = (ar.min(head_row), ar.max(head_row));
        let (c0, c1) = (ac.min(head_col), ac.max(head_col));
        self.carets.clear();
        self.selection = None;
        for r in r0..=r1 {
            let len = self.line_char_len(r);
            let s = c0.min(len);
            let e = c1.min(len);
            let sel = EditorSelection {
                anchor: (r, s),
                head: (r, e),
            };
            if r == head_row {
                self.selection = (e > s).then_some(sel);
                self.cursor_row = r;
                self.cursor_col = e;
            } else {
                self.carets.push(sel);
            }
        }
        self.ensure_cursor_col_visible();
    }

    /// End the column-selection drag, keeping the carets it produced.
    pub fn end_box_select(&mut self) {
        self.box_anchor = None;
    }

    /// Grow the column (box) selection from the keyboard (#1560), VS
    /// Code's Ctrl+Shift+Alt+arrows: move its head `rows` rows and `cols`
    /// columns, then rebuild one selection per spanned row, as the
    /// Shift+Alt+drag does. A box starts at the caret (or the one-line
    /// selection's anchor) unless the carets are still the ones the last
    /// column-select key made. The head stays on the buffer's rows and,
    /// like VS Code's, may sit past a short line's end, up to the longest
    /// line in the box.
    pub fn column_select(&mut self, rows: isize, cols: isize) {
        let current = self.column_box_carets();
        let (anchor, head) = match &self.column_box {
            Some(b) if b.left == current => (b.anchor, b.head),
            _ => {
                let caret = (self.cursor_row, self.cursor_col);
                let anchor = match self.selection {
                    Some(sel) if self.carets.is_empty() && sel.anchor.0 == sel.head.0 => sel.anchor,
                    _ => caret,
                };
                (anchor, caret)
            }
        };
        let last = self.lines.len().saturating_sub(1);
        let row = head.0.saturating_add_signed(rows).min(last);
        let (r0, r1) = (anchor.0.min(row), anchor.0.max(row));
        let widest = (r0..=r1).map(|r| self.line_char_len(r)).max().unwrap_or(0);
        let col = head.1.saturating_add_signed(cols).min(widest.max(anchor.1));
        self.box_anchor = Some(anchor);
        self.box_select_to(row, col);
        self.box_anchor = None;
        self.column_box = Some(ColumnBox {
            anchor,
            head: (row, col),
            left: self.column_box_carets(),
        });
    }

    /// The carets and selections a keyboard column selection compares
    /// against what it left, so any edit or move starts a fresh box.
    pub(super) fn column_box_carets(&self) -> ColumnBoxCarets {
        (
            (self.cursor_row, self.cursor_col),
            self.selection,
            self.carets.clone(),
        )
    }

    /// Rows a column-select PageUp/PageDown moves: one screen of text rows,
    /// as the plain page motions use.
    pub fn column_select_page(&self) -> isize {
        self.page_size().max(1) as isize
    }

    /// VS Code "Add Cursor Above" (Cmd+Alt+Up): mirror of `add_cursor_below`.
    /// No-op at the first row.
    pub fn add_cursor_above(&mut self) {
        self.multi_typing = None;
        let min_row = self
            .carets
            .iter()
            .map(|c| c.head.0)
            .chain(std::iter::once(self.cursor_row))
            .min()
            .unwrap_or(self.cursor_row);
        if min_row <= self.symbol_clip().map_or(0, |(first, _)| first) {
            return;
        }
        let target = min_row - 1;
        let col = self.cursor_col.min(self.line_char_len(target));
        self.carets.push(EditorSelection {
            anchor: (target, col),
            head: (target, col),
        });
    }
}

/// Start char-indices of every whole-word, case-sensitive occurrence of
/// `word` within `chars`. "Whole-word" means the char immediately before and
/// after the match is not a word char, so `foo` does not match inside
/// `foobar`. Mirrors VS Code's "Change All Occurrences" matching.
pub(super) fn find_word_occurrences(chars: &[char], word: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if word.is_empty() || word.len() > chars.len() {
        return out;
    }
    let last_start = chars.len() - word.len();
    let mut i = 0;
    while i <= last_start {
        if chars[i..i + word.len()] == *word {
            let before_ok = i == 0 || !is_word_char(chars[i - 1]);
            let after_idx = i + word.len();
            let after_ok = after_idx >= chars.len() || !is_word_char(chars[after_idx]);
            if before_ok && after_ok {
                out.push(i);
                i += word.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Start char-indices of every case-sensitive occurrence of `text` within
/// `chars`, as plain text and without overlap (#1558): what VS Code matches
/// for a selection made by hand, so `oo` is found inside `foo`.
pub(super) fn find_text_occurrences(chars: &[char], text: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if text.is_empty() || text.len() > chars.len() {
        return out;
    }
    let mut i = 0;
    while i + text.len() <= chars.len() {
        if chars[i..i + text.len()] == *text {
            out.push(i);
            i += text.len();
        } else {
            i += 1;
        }
    }
    out
}
