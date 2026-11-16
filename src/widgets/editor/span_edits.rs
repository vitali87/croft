//! Edits that arrive from elsewhere: language servers, formatters and peers, applied as
//! text spans that keep the caret, the view and the undo history in step.

use super::*;

/// Apply `edits` to `lines` (char-indexed coords), bottom-to-top so an
/// earlier replacement never shifts the coordinates of a later one. Returns
/// the number of edits applied. Out-of-range edits are skipped.
pub fn apply_span_edits_to_lines(lines: &mut Vec<String>, edits: &[TextSpanEdit]) -> usize {
    let converted = char_span_edits(lines, edits);
    apply_char_span_edits(lines, converted)
        .iter()
        .map(|(_, count)| count)
        .sum()
}

/// Apply char-column `edits` in [`in_application_order`] and return the
/// ones that landed, in the order applied, each with the number of edits it
/// stands for; out-of-range edits are skipped and left out, so the caret is
/// mapped only through text that actually changed.
pub(super) fn apply_char_span_edits(
    lines: &mut Vec<String>,
    edits: Vec<TextSpanEdit>,
) -> Vec<(TextSpanEdit, usize)> {
    in_application_order(edits)
        .into_iter()
        .filter(|(e, _)| replace_span(lines, e))
        .collect()
}

/// `edits` with their UTF-16 columns read as chars. Every edit names the
/// ORIGINAL text, so the columns convert against `lines` as they are before
/// any edit lands.
pub(super) fn char_span_edits(lines: &[String], edits: &[TextSpanEdit]) -> Vec<TextSpanEdit> {
    let col = |row: usize, c: usize| lines.get(row).map_or(c, |l| utf16_to_char_col(l, c as u32));
    edits
        .iter()
        .map(|e| match e.utf16 {
            true => TextSpanEdit {
                start: (e.start.0, col(e.start.0, e.start.1)),
                end: (e.end.0, col(e.end.0, e.end.1)),
                new_text: e.new_text.clone(),
                utf16: false,
            },
            false => e.clone(),
        })
        .collect()
}

/// `edits` bottom-up, the order they apply in, each with the number of edits
/// it stands for. Edits that start at one position are joined into one, their
/// texts in the order sent: the LSP spec puts them in array order, and
/// applying them one by one bottom-up reversed them, so vtsls's `"\n"` then
/// `"  "` landed as trailing spaces above an unindented line (#1633).
pub(super) fn in_application_order(mut edits: Vec<TextSpanEdit>) -> Vec<(TextSpanEdit, usize)> {
    edits.sort_by_key(|e| e.start);
    let mut joined: Vec<(TextSpanEdit, usize)> = Vec::with_capacity(edits.len());
    for e in edits {
        match joined.last_mut() {
            Some((last, count)) if last.start == e.start => {
                last.end = last.end.max(e.end);
                last.new_text.push_str(&e.new_text);
                *count += 1;
            }
            _ => joined.push((e, 1)),
        }
    }
    joined.reverse();
    joined
}

/// Where the text `e` writes ends, in the coordinates after it lands.
pub(super) fn span_new_end(e: &TextSpanEdit) -> (usize, usize) {
    let rows = e.new_text.matches('\n').count();
    let last = e.new_text.rsplit('\n').next().unwrap_or("").chars().count();
    match rows {
        0 => (e.start.0, e.start.1 + last),
        _ => (e.start.0 + rows, last),
    }
}

/// `pos` moved past an edit that ends at or before it: by the rows the edit
/// added or removed and, when it ends on `pos`'s row, by the change in that
/// row's text before `pos`.
pub(super) fn shift_past_span(
    pos: (usize, usize),
    e: &TextSpanEdit,
    new_end: (usize, usize),
) -> (usize, usize) {
    if pos.0 == e.end.0 {
        (new_end.0, new_end.1 + pos.1 - e.end.1)
    } else {
        (pos.0 + new_end.0 - e.end.0, pos.1)
    }
}

/// Where `pos` lands once char-column `edits` are applied (#1603): the
/// caret follows its text. An edit ending at or before `pos` shifts it past
/// the edit; a `pos` inside a replaced range keeps its row and column,
/// clamped to the end of the new text, so a whole-document format leaves the
/// caret on its line number as before and a deletion pulls it to where the
/// range began; edits after `pos` leave it alone. Mapped bottom-up, the
/// order the edits are applied in, so each edit's coordinates still name the
/// text it was written against.
pub fn map_through_span_edits(pos: (usize, usize), edits: &[TextSpanEdit]) -> (usize, usize) {
    map_through_edit_refs(pos, edits.iter().collect())
}

/// Where the viewport's top line lands once char-column `edits` are applied
/// (#918): lines added or removed above the view, by a peer's typing or an Add
/// import, move it with them, so the text on screen stays put. Only
/// edits that start above the top line's start count: text inserted right at
/// it shows at the top of the view, and the top of the file stays the top.
pub(super) fn view_top_after_span_edits(top: usize, edits: &[TextSpanEdit]) -> usize {
    let start = (top, 0);
    map_through_edit_refs(start, edits.iter().filter(|e| e.start < start).collect()).0
}

/// [`map_through_span_edits`] over borrowed edits.
pub(super) fn map_through_edit_refs(
    pos: (usize, usize),
    mut order: Vec<&TextSpanEdit>,
) -> (usize, usize) {
    order.retain(|e| e.end >= e.start);
    order.sort_by_key(|e| std::cmp::Reverse(e.start));
    order.into_iter().fold(pos, |p, e| {
        if p < e.end && p <= e.start {
            return p;
        }
        let new_end = span_new_end(e);
        match p < e.end {
            true => p.min(new_end),
            false => shift_past_span(p, e, new_end),
        }
    })
}

/// Where `pos` (char columns) lands once `edits` apply to `lines`: moved by
/// the text every edit before it adds or removes. A position inside an edit
/// lands at the end of its new text, unlike [`map_through_span_edits`]:
/// format on type rewrites the line just typed, and the caret belongs after
/// the text it typed, not at its old column (#1633).
pub fn position_after_span_edits(
    lines: &[String],
    pos: (usize, usize),
    edits: &[TextSpanEdit],
) -> (usize, usize) {
    let mut pos = pos;
    for (e, _) in in_application_order(char_span_edits(lines, edits)) {
        if pos < e.start || (pos == e.start && e.start != e.end) {
            continue;
        }
        let new_end = span_new_end(&e);
        pos = match pos < e.end {
            true => new_end,
            false => shift_past_span(pos, &e, new_end),
        };
    }
    pos
}

pub(super) fn replace_span(lines: &mut Vec<String>, e: &TextSpanEdit) -> bool {
    let (sr, sc) = e.start;
    let (er, ec) = e.end;
    if sr >= lines.len() || er >= lines.len() || (er, ec) < (sr, sc) {
        return false;
    }
    let from = char_byte(&lines[sr], sc);
    let to = char_byte(&lines[er], ec);
    let head = lines[sr][..from].to_string();
    let tail = lines[er][to..].to_string();
    // Build the replacement text in full, then split it back on '\n' so a
    // multi-line `new_text` (e.g. ruff's "Organize Imports", a whole-document
    // reformat) becomes separate rows instead of one row with embedded newline
    // bytes. A single-line edit (rename) yields exactly one row, unchanged.
    let combined = format!("{head}{}{tail}", e.new_text);
    let new_lines: Vec<String> = combined.split('\n').map(str::to_string).collect();
    let _ = lines.splice(sr..=er, new_lines);
    true
}

/// Carry a peer's `edit`, written against the `live` text, into every state
/// of an undo or redo `stack` (#919). Each state is one step from the one
/// nearer the live text, so the edit is rebased state by state from the
/// top, and stops at the first state it leaves alone.
pub(super) fn rebase_history(stack: &mut [Snapshot], live: &str, edit: crate::undo_rebase::Splice) {
    let mut edits = Vec::new();
    let mut source = live;
    let mut edit = edit;
    for snap in stack.iter().rev() {
        let Some(next) = crate::undo_rebase::rebase(snap.lines.as_str(), source, &edit) else {
            break;
        };
        edits.push(next.clone());
        edit = next;
        source = snap.lines.as_str();
    }
    for (snap, edit) in stack.iter_mut().rev().zip(&edits) {
        snap.take_peer_splice(edit);
    }
}

impl Editor {
    /// [`Self::apply_span_edits`] for text another seat wrote (#349): the line
    /// provenance map is spliced edit by edit, bottom-up like the edits, so
    /// rows below a multi-line insertion keep their own seats, and the rows
    /// each edit wrote are credited to `seat`. One undo step.
    pub fn apply_span_edits_as(
        &mut self,
        edits: &[TextSpanEdit],
        seat: crate::provenance::Seat,
    ) -> usize {
        self.apply_span_edits_as_in(edits, seat, None)
    }

    /// [`Self::apply_span_edits_as`] in the undo step `group`, joined as by
    /// [`Self::apply_remote_span_edits`]: a completion and the auto-import
    /// its server resolves after it went in undo as one (#928).
    pub fn apply_span_edits_as_in(
        &mut self,
        edits: &[TextSpanEdit],
        seat: crate::provenance::Seat,
        group: Option<u64>,
    ) -> usize {
        if edits.is_empty() {
            return 0;
        }
        self.pin_on_edit();
        self.push_undo_step(EditKind::Paste, group);
        let applied = self.apply_document_edits(edits);
        let n = applied.iter().map(|(_, count)| count).sum();
        if n > 0 {
            self.follow_span_edits(&applied);
            for (e, _) in &applied {
                let removed = e.end.0.saturating_sub(e.start.0) + 1;
                let added = e.new_text.matches('\n').count() + 1;
                self.provenance.splice(e.start.0, removed, added);
                self.provenance
                    .record(e.start.0..e.start.0 + added, seat.clone());
            }
            self.provenance.truncate(self.lines.len());
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        n
    }

    /// [`Self::apply_span_edits`], the caret carried through the edits so it
    /// stays beside the same text: format on type rewrites the line just
    /// typed, and a clamp left the caret on the row above the `;` (#1633).
    pub fn apply_span_edits_moving_caret(&mut self, edits: &[TextSpanEdit]) -> usize {
        let caret = (self.cursor_row, self.cursor_col);
        let moved = position_after_span_edits(&self.lines, caret, edits);
        let n = self.apply_span_edits(edits);
        if n > 0 {
            (self.cursor_row, self.cursor_col) = moved;
        }
        self.clamp_cursor();
        n
    }

    /// Apply LSP rename edits to this buffer in-memory as a single undo step,
    /// marking the tab dirty (the user saves to persist). Returns the number
    /// of edits applied.
    pub fn apply_span_edits(&mut self, edits: &[TextSpanEdit]) -> usize {
        self.apply_span_edits_grouped(edits, None)
    }

    /// [`Self::apply_span_edits`] for text an AI stream wrote over collab
    /// (#1207). Every call with the same `group` lands in one undo step
    /// while no other step opens in between: the fragments of one stream,
    /// which arrive one span at a time. Undo then restores the text from
    /// before the group, never a mix of halves.
    pub fn apply_remote_span_edits(&mut self, edits: &[TextSpanEdit], group: u64) -> usize {
        self.apply_span_edits_grouped(edits, Some(group))
    }

    /// Apply a collab peer's edit without an undo step of its own (#919):
    /// every undo and redo state takes the edit instead, so Ctrl+Z reverts
    /// only this user's steps and keeps the peer's text, as in Zed or Live
    /// Share. Returns the number of edits applied.
    pub fn apply_peer_span_edit(&mut self, edit: &TextSpanEdit) -> usize {
        let edit = char_span_edits(&self.lines, std::slice::from_ref(edit)).remove(0);
        let (start, end) = (edit.start, edit.end);
        if end < start || end.0 >= self.lines.len() {
            return 0;
        }
        if !self.undo_stack.is_empty() || !self.redo_stack.is_empty() {
            let offset = |(row, col): (usize, usize)| {
                let above: usize = self.lines[..row].iter().map(|l| l.len() + 1).sum();
                above + char_byte(&self.lines[row], col)
            };
            let at = offset(start);
            let splice = crate::undo_rebase::Splice {
                at,
                deleted: offset(end) - at,
                inserted: edit.new_text.clone(),
            };
            let live = self.lines.join("\n");
            rebase_history(&mut self.undo_stack, &live, splice.clone());
            rebase_history(&mut self.redo_stack, &live, splice);
            // The peer's text now sits in every state held: a large paste
            // over a deep history must not lift it past the cap.
            self.trim_undo_stack();
            self.trim_redo_stack();
        }
        self.pin_on_edit();
        // The pre-edit text, for bookmarks to be diffed against afterwards.
        self.seed_bookmark_shadow();
        let applied = apply_char_span_edits(&mut self.lines, vec![edit]);
        let n = applied.iter().map(|(_, count)| count).sum();
        if n > 0 {
            self.follow_span_edits(&applied);
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        n
    }

    pub(super) fn apply_span_edits_grouped(
        &mut self,
        edits: &[TextSpanEdit],
        group: Option<u64>,
    ) -> usize {
        if edits.is_empty() {
            return 0;
        }
        self.pin_on_edit();
        self.push_undo_step(EditKind::Paste, group);
        let applied = self.apply_document_edits(edits);
        let n = applied.iter().map(|(_, count)| count).sum();
        if n > 0 {
            self.follow_span_edits(&applied);
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        n
    }

    /// Apply `edits`, positioned in [`Self::document_text`], to the lines.
    /// The final newline that text ends in opens an empty last row there,
    /// which a language server's edit may reach (#911): a formatter that
    /// rewrites the whole file replaces up to it. The row is added for the
    /// edits and folded back into `final_newline` after, so a newline the
    /// edits keep never stays as a blank row the save writes a second
    /// newline after; edits that remove it clear `final_newline`.
    pub(super) fn apply_document_edits(
        &mut self,
        edits: &[TextSpanEdit],
    ) -> Vec<(TextSpanEdit, usize)> {
        let newline_row = self.sends_final_newline();
        if newline_row {
            self.lines.push(String::new());
        }
        let edits = char_span_edits(&self.lines, edits);
        let applied = apply_char_span_edits(&mut self.lines, edits);
        if newline_row {
            if self.lines.len() > 1 && self.lines.last().is_some_and(String::is_empty) {
                self.lines.pop();
            } else {
                self.final_newline = false;
            }
        }
        applied
    }

    /// Move the caret, the selection and every extra caret with the text
    /// they were on through char-column `edits` just applied (#1603): an
    /// Add import above the caret left it on its old line number, inside
    /// the grown import block, where the next keystrokes landed. The view's
    /// top line follows its text too (#918), or a peer's lines added above
    /// it pushed the text being read down the pane.
    pub(super) fn follow_span_edits(&mut self, applied: &[(TextSpanEdit, usize)]) {
        let edits: Vec<TextSpanEdit> = applied.iter().map(|(e, _)| e.clone()).collect();
        let map = |p: (usize, usize)| map_through_span_edits(p, &edits);
        (self.cursor_row, self.cursor_col) = map((self.cursor_row, self.cursor_col));
        let last = self.lines.len().saturating_sub(1);
        self.scroll = view_top_after_span_edits(self.scroll, &edits).min(last);
        let lines = &self.lines;
        let clamp = |(r, c): (usize, usize)| {
            let r = r.min(last);
            (r, c.min(lines.get(r).map_or(0, |l| l.chars().count())))
        };
        for sel in self.selection.iter_mut().chain(self.carets.iter_mut()) {
            sel.anchor = clamp(map(sel.anchor));
            sel.head = clamp(map(sel.head));
        }
        // A live snippet's stops move with their text too: an import a
        // completion's server resolved after it expanded (#928) would
        // otherwise send the next Tab into the import block.
        if let Some(session) = self.snippet.as_mut() {
            session.anchor = map(session.anchor);
            for stop in &mut session.stops {
                (stop.0, stop.1) = map((stop.0, stop.1));
            }
        }
        self.clamp_cursor();
    }

    /// Close a remote edit group (#1207): later spans open a new undo step,
    /// and a group that left the text as it found it (a cancelled AI stream
    /// whose revert undid every fragment) leaves no step behind.
    pub fn end_remote_undo_group(&mut self, group: u64) {
        if self.remote_undo_group != Some(group) {
            return;
        }
        self.remote_undo_group = None;
        if self
            .undo_stack
            .last()
            .is_some_and(|s| s.lines == self.lines[..])
        {
            self.undo_stack.pop();
        }
    }
}
