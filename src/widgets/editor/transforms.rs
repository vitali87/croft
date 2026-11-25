//! Text transforms: changing case, bumping the number at the caret, and transposing
//! characters.

use super::*;

/// The case transforms VS Code exposes as
/// `editor.action.transformTo{Upper,Lower,Title,Snake,Camel,Pascal,Kebab}case`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseTransform {
    Upper,
    Lower,
    Title,
    /// `user_name` (#1573).
    Snake,
    /// `userName` (#1573).
    Camel,
    /// `UserName` (#1573).
    Pascal,
    /// `user-name` (#1573).
    Kebab,
}

impl Editor {
    /// VS Code "Transform to Upper/Lower/Title Case": rewrite the selection
    /// (or, with no selection, the word under the cursor) in the requested
    /// case, and the same for every extra caret, keeping each selection.
    /// No-op when there is nothing to transform.
    pub fn transform_selection_case(&mut self, kind: CaseTransform) {
        let sels = self.selections_with_primary();
        let ranges: Vec<Option<(BPos, BPos)>> = sels
            .iter()
            .map(|s| {
                if s.has_area() {
                    Some(s.normalised())
                } else if matches!(
                    kind,
                    CaseTransform::Snake
                        | CaseTransform::Camel
                        | CaseTransform::Pascal
                        | CaseTransform::Kebab
                ) {
                    // Naming styles take the whole identifier run, hyphens
                    // included, so `user-name` converts as one identifier.
                    self.identifier_run_at(s.head.0, s.head.1)
                        .map(|(a, b)| ((s.head.0, a), (s.head.0, b)))
                } else {
                    self.word_at(s.head.0, s.head.1)
                        .map(|(a, b)| ((s.head.0, a), (s.head.0, b)))
                }
            })
            .collect();
        if ranges
            .iter()
            .flatten()
            .all(|&(start, end)| self.char_range_text(start, end).is_empty())
        {
            return;
        }
        self.push_undo(EditKind::TransformCase);
        // Overlapping ranges (two carets in one word, nested selections)
        // merge into one span, so each character is rewritten exactly once.
        let mut by_start: Vec<usize> = (0..sels.len()).filter(|&i| ranges[i].is_some()).collect();
        by_start.sort_by_key(|&i| ranges[i].map(|r| r.0));
        let mut spans: Vec<(BPos, BPos, Vec<usize>)> = Vec::new();
        for i in by_start {
            let Some((start, end)) = ranges[i] else {
                continue;
            };
            match spans.last_mut() {
                Some(span) if start < span.1 || start == span.0 => {
                    span.1 = span.1.max(end);
                    span.2.push(i);
                }
                _ => spans.push((start, end, vec![i])),
            }
        }
        // From the bottom up, so an edit never moves a span still to come.
        // A caret with no word is a span with nothing to rewrite.
        for i in 0..sels.len() {
            if ranges[i].is_none() {
                spans.push((sels[i].head, sels[i].head, vec![i]));
            }
        }
        spans.sort_by_key(|span| std::cmp::Reverse(span.0));
        let mut pinned = vec![((0, 0), (0, 0)); sels.len()];
        for (start, end, members) in spans {
            let text = self.char_range_text(start, end);
            // Each endpoint as a char offset into the span, so it lands on
            // the same character once the span is rewritten.
            let offset = |e: &Self, pos: BPos| {
                e.char_range_text(start, pos.max(start).min(end))
                    .chars()
                    .count()
            };
            let offsets: Vec<(usize, usize)> = members
                .iter()
                .map(|&i| (offset(self, sels[i].anchor), offset(self, sels[i].head)))
                .collect();
            let new = match kind {
                CaseTransform::Upper => text.to_uppercase(),
                CaseTransform::Lower => text.to_lowercase(),
                CaseTransform::Title => title_case(&text),
                CaseTransform::Snake
                | CaseTransform::Camel
                | CaseTransform::Pascal
                | CaseTransform::Kebab => identifier_case(&text, kind),
            };
            if !text.is_empty() {
                self.replace_char_range(start, end, &new);
            }
            let at = |k: usize| text_end(start, &new.chars().take(k).collect::<String>());
            for (&i, &(anchor, head)) in members.iter().zip(&offsets) {
                let after = if text.is_empty() {
                    sels[i]
                } else {
                    EditorSelection {
                        anchor: at(anchor),
                        head: at(head),
                    }
                };
                pinned[i] = (
                    self.pin_from_end(after.anchor),
                    self.pin_from_end(after.head),
                );
            }
        }
        let all: Vec<EditorSelection> = pinned
            .iter()
            .map(|&(anchor, head)| EditorSelection {
                anchor: self.unpin_from_end(anchor),
                head: self.unpin_from_end(head),
            })
            .collect();
        self.set_selections(&all);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
    }

    /// vim's `Ctrl-A` / `Ctrl-X` (`delta` of `+1` / `-1`), which Zed and the
    /// VS Code increment extensions all copy: bump the number under the
    /// cursor. When the cursor is not inside a number, the next one to its
    /// right on the same line is used, so the caret rarely needs positioning
    /// precisely. With several carets every caret bumps its own number, in
    /// one undo step, and each caret ends on its number (#1637). Returns
    /// false when no caret's line holds a number at or after it, leaving the
    /// buffer and the undo stack untouched.
    ///
    /// Formatting is preserved rather than normalised, because the point is
    /// to edit a literal in place: zero padding keeps its width (`007` ->
    /// `008`), and a hex or binary literal keeps its prefix and digit case
    /// (`0xFF` -> `0x100`, `0Xff` -> `0X100`). Width only grows when the
    /// value needs the room.
    pub fn bump_number(&mut self, delta: i64) -> bool {
        // Every caret's number, read off the buffer before anything changes.
        // Carets that share a number bump it once, so two carets on `19`
        // give `20`, not `21`.
        let all = self.selections_with_primary();
        let mut owned: Vec<Option<(usize, usize)>> = Vec::with_capacity(all.len());
        let mut bumps: Vec<(usize, NumberToken, String)> = Vec::new();
        for s in &all {
            let (row, col) = s.head;
            let found = self.lines.get(row).and_then(|line| {
                let tok = number_token_at_or_after(line, self.byte_index(row, col))?;
                Some((tok, tok.bumped(line, delta)?))
            });
            let Some((tok, replacement)) = found else {
                owned.push(None);
                continue;
            };
            owned.push(Some((row, tok.start)));
            if !bumps
                .iter()
                .any(|(r, t, _)| *r == row && t.start == tok.start)
            {
                bumps.push((row, tok, replacement));
            }
        }
        if bumps.is_empty() {
            return false;
        }
        // A clamped bump (`0x00` down) finds a number but changes nothing;
        // recording it would dirty a clean buffer and leave an undo step
        // that undoes nothing.
        bumps.retain(|(row, tok, rep)| *rep != self.lines[*row][tok.start..tok.end]);
        if bumps.is_empty() {
            return true;
        }

        // A real edit from here on: pin a preview tab, or the next preview
        // open reuses it and the bump is lost with it.
        // One undo step for every caret's bump.
        self.undoable_edit(EditKind::BumpNumber, |this| {
            // Never coalesce: a run of bumps must undo one at a time, the way
            // holding Ctrl-A in vim does.
            this.last_edit_kind = None;

            // Where every caret lands, worked out on the text before the edits.
            // Literals are ASCII, so a bump's growth in bytes is its growth in
            // chars, and a point moves by what the bumps left of it on its row
            // added.
            let char_of =
                |this: &Self, row: usize, byte: usize| this.lines[row][..byte].chars().count();
            let growth_before = |this: &Self, row: usize, col: usize| -> isize {
                bumps
                    .iter()
                    .filter(|(r, t, _)| *r == row && char_of(this, row, t.end) <= col)
                    .map(|(_, t, rep)| rep.len() as isize - (t.end - t.start) as isize)
                    .sum()
            };
            let shift = |this: &Self, (row, col): (usize, usize)| {
                (
                    row,
                    col.saturating_add_signed(growth_before(this, row, col)),
                )
            };
            let moved: Vec<EditorSelection> = all
                .iter()
                .zip(&owned)
                .map(|(s, own)| {
                    let bump = own.and_then(|(row, start)| {
                        bumps.iter().find(|(r, t, _)| *r == row && t.start == start)
                    });
                    match bump {
                        // vim leaves the caret on the number's last digit, which
                        // is what makes a repeated bump keep working on the same
                        // literal even as it grows a digit.
                        Some((row, tok, rep)) => {
                            let start = char_of(this, *row, tok.start);
                            let (_, start) = shift(this, (*row, start));
                            EditorSelection::new(*row, start + rep.len().saturating_sub(1))
                        }
                        None => EditorSelection {
                            anchor: shift(this, s.anchor),
                            head: shift(this, s.head),
                        },
                    }
                })
                .collect();

            // Right to left along each row, so an edit never moves a literal
            // still waiting for its own.
            bumps.sort_by(|(ra, ta, _), (rb, tb, _)| (rb, tb.start).cmp(&(ra, ta.start)));
            for (row, tok, replacement) in &bumps {
                this.lines[*row].replace_range(tok.start..tok.end, replacement);
            }
            this.set_selections(&moved);
        });
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Transpose Characters around the Cursor"
    /// (`editor.action.transpose`): swap the character before the cursor with
    /// the one after it and advance the cursor (emacs `transpose-chars`). At
    /// the end of a non-final line the last character moves across the line
    /// break to the start of the next line. At the end of the final line it is
    /// a no-op. With several carets each one swaps around itself, in one undo
    /// step (#1637).
    pub fn transpose_chars(&mut self) {
        let all = self.selections_with_primary();
        let edits = all.iter().any(|s| {
            let (row, col) = s.head;
            if col >= self.line_char_len(row) {
                row + 1 < self.lines.len()
            } else {
                col > 0
            }
        });
        // One undo step for every caret's swap.
        if edits {
            self.push_undo(EditKind::Transpose);
        }
        // Bottom up and right to left: a swap only touches its own row's
        // chars at its caret, or moves a row's last char to the start of the
        // next, so every caret still waiting is where it was, and every caret
        // already done is pinned from its line's end.
        let mut order: Vec<usize> = (0..all.len()).collect();
        order.sort_by(|&a, &b| all[b].head.cmp(&all[a].head));
        // Two carets whose swaps share a character (`ab` with carets after
        // `a` and after `b`) would each act on text the other already moved:
        // the second of them, in this order, stays where it is instead.
        let mut taken: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        let mut pinned = vec![(0, 0); all.len()];
        for i in order {
            let head = all[i].head;
            let cells = self.transpose_cells(head);
            if cells.iter().any(|c| taken.contains(c)) {
                pinned[i] = self.pin_from_end(head);
                continue;
            }
            taken.extend(cells);
            let at = self.transpose_at(head);
            pinned[i] = self.pin_from_end(at);
        }
        let moved: Vec<EditorSelection> = pinned
            .iter()
            .map(|&p| {
                let (row, col) = self.unpin_from_end(p);
                EditorSelection::new(row, col)
            })
            .collect();
        self.set_selections(&moved);
        if edits {
            self.mark_buffer_changed();
            self.recompute_highlights();
        }
        self.ensure_cursor_col_visible();
    }

    /// The `(row, char col)` cells one caret's Transpose Characters
    /// changes, in the buffer as it is: the two chars it swaps, or a line's
    /// last char and the line break it moves across (col = the line's
    /// length). Empty when it only steps the caret.
    pub(super) fn transpose_cells(&self, (row, col): (usize, usize)) -> Vec<(usize, usize)> {
        let len = self.line_char_len(row);
        if col >= len {
            if row + 1 >= self.lines.len() || len == 0 {
                return Vec::new();
            }
            return vec![(row, len - 1), (row, len)];
        }
        if col == 0 {
            return Vec::new();
        }
        vec![(row, col - 1), (row, col)]
    }

    /// One caret's Transpose Characters, returning where that caret goes.
    pub(super) fn transpose_at(&mut self, (row, col): (usize, usize)) -> (usize, usize) {
        let len = self.line_char_len(row);
        if col >= len {
            if row + 1 >= self.lines.len() {
                return (row, col);
            }
            if len == 0 {
                return (row + 1, 0);
            }
            let mut chars: Vec<char> = self.lines[row].chars().collect();
            let moved = chars.pop().unwrap();
            self.lines[row] = chars.into_iter().collect();
            self.lines[row + 1].insert(0, moved);
            return (row + 1, 1);
        }
        if col == 0 {
            // VS Code's single-character range at column 1: no swap, just step
            // the cursor right.
            return (row, 1);
        }
        let mut chars: Vec<char> = self.lines[row].chars().collect();
        chars.swap(col - 1, col);
        self.lines[row] = chars.into_iter().collect();
        (row, col + 1)
    }
}

/// Title-case a string: the first alphanumeric of each run is upper-cased,
/// the rest lower-cased, separators preserved. Used by Transform to Title
/// Case. An apostrophe (`'` or `’`) right after a letter stays inside the
/// word, so `don't` and `journey's` keep a lower-case tail (#1016); one at
/// the start of a word (`'tis`) still lets the next letter start it, as in
/// VS Code.
pub(super) fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alnum = false;
    let mut prev_letter = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if prev_alnum {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_alnum = true;
            prev_letter = c.is_alphabetic();
        } else if matches!(c, '\'' | '\u{2019}') && prev_letter {
            // `prev_alnum` stays set: the letter after it continues the word.
            out.push(c);
            prev_letter = false;
        } else {
            out.push(c);
            prev_alnum = false;
            prev_letter = false;
        }
    }
    out
}

/// Rewrite every identifier in `s` in the naming style `kind` names, Snake,
/// Camel, Pascal or Kebab (#1573). An identifier is a run of letters,
/// digits, `_` and `-`; everything between runs (spaces, punctuation, line
/// breaks) is kept, as are a run's leading and trailing `_` and `-`
/// (`_private`). A run with no letter is left alone.
pub(super) fn identifier_case(s: &str, kind: CaseTransform) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if !run.is_empty() {
            out.push_str(&case_run(run, kind));
            run.clear();
        }
    };
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// One identifier run in `kind`'s style; see [`identifier_case`].
pub(super) fn case_run(run: &str, kind: CaseTransform) -> String {
    let edge = |c: char| c == '_' || c == '-';
    let core = run.trim_matches(edge);
    if !core.chars().any(char::is_alphabetic) {
        return run.to_string();
    }
    let lead = &run[..run.len() - run.trim_start_matches(edge).len()];
    let trail = &run[run.trim_end_matches(edge).len()..];
    let words = case_words(core);
    let cap = |w: &str| {
        let mut chars = w.chars();
        chars.next().map_or_else(String::new, |f| {
            f.to_uppercase()
                .chain(chars.flat_map(char::to_lowercase))
                .collect()
        })
    };
    let body = match kind {
        CaseTransform::Snake | CaseTransform::Kebab => {
            let sep = if kind == CaseTransform::Snake {
                "_"
            } else {
                "-"
            };
            words
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<Vec<_>>()
                .join(sep)
        }
        CaseTransform::Camel => words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.to_lowercase() } else { cap(w) })
            .collect(),
        _ => words.iter().map(|w| cap(w)).collect(),
    };
    format!("{lead}{body}{trail}")
}

/// The words of an identifier: split at `_` and `-`, and where the case
/// changes as VS Code's transforms read it: a lower-case letter before an
/// upper-case one (`foo|Bar`), and before the last capital of a run of
/// capitals or digits that a lower-case letter follows (`HTTP|Server`,
/// `Base64|Encoder`).
pub(super) fn case_words(core: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in core.split(['_', '-']).filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1).copied();
            let boundary = c.is_uppercase()
                && prev.is_some_and(|p| {
                    p.is_lowercase()
                        || ((p.is_uppercase() || p.is_numeric())
                            && next.is_some_and(char::is_lowercase))
                });
            if boundary && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c);
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// The base a numeric literal is written in. Octal is deliberately absent:
/// vim reads a bare leading zero as octal, which silently turns `019` into
/// an error and `010` into `011` meaning nine, and every modern editor that
/// copied Ctrl-A dropped that behaviour with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NumberRadix {
    Dec,
    Hex,
    Bin,
}

/// One numeric literal located on a line: the byte range it occupies and how
/// to read it. `start` includes a `-` sign and a `0x` / `0b` prefix, so the
/// range is exactly the text a bump replaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct NumberToken {
    start: usize,
    end: usize,
    radix: NumberRadix,
}

impl NumberToken {
    /// The literal rewritten with `delta` added, preserving how it was
    /// written. `None` when the result cannot be represented — an overflow,
    /// or a negative hex/binary value, which is clamped rather than wrapped
    /// (vim turns `0x0` into `0xffffffffffffffff`, which is never what the
    /// keystroke meant).
    pub(super) fn bumped(&self, line: &str, delta: i64) -> Option<String> {
        let text = &line[self.start..self.end];
        match self.radix {
            NumberRadix::Dec => {
                let value: i64 = text.parse().ok()?;
                let next = value.checked_add(delta)?;
                // Zero padding is a fact about the literal, not about its
                // value: `007` is a version segment or a port, and widening
                // it on every bump would be wrong.
                let digits = text.strip_prefix('-').unwrap_or(text).len();
                let body = next.unsigned_abs();
                let sign = if next < 0 { "-" } else { "" };
                Some(format!("{sign}{body:0digits$}"))
            }
            NumberRadix::Hex | NumberRadix::Bin => {
                let (prefix, digits) = text.split_at(2);
                let radix = if self.radix == NumberRadix::Hex {
                    16
                } else {
                    2
                };
                let value = u64::from_str_radix(digits, radix).ok()?;
                let next = match value.checked_add_signed(delta) {
                    Some(v) => v,
                    // Underflow clamps at zero rather than wrapping: vim
                    // turns `0x0` into `0xffffffffffffffff`, which is never
                    // what the keystroke meant.
                    None if delta < 0 => 0,
                    None => return None,
                };
                let width = digits.len();
                let body = match self.radix {
                    NumberRadix::Bin => format!("{next:0width$b}"),
                    // Digit case follows the literal: a file that writes
                    // `0xFF` keeps writing uppercase.
                    _ if digits.chars().any(|c| c.is_ascii_uppercase()) => {
                        format!("{next:0width$X}")
                    }
                    _ => format!("{next:0width$x}"),
                };
                Some(format!("{prefix}{body}"))
            }
        }
    }
}

/// The literal the cursor sits inside, or failing that the next one to its
/// right on the same line — vim's rule, and the reason Ctrl-A is usable
/// without placing the caret exactly.
pub(super) fn number_token_at_or_after(line: &str, caret: usize) -> Option<NumberToken> {
    // Tokens come out left to right and never overlap, so the first one
    // ending at or after the caret is the one under it, or the next one
    // along when the caret is between literals.
    number_tokens(line).into_iter().find(|t| t.end >= caret)
}

/// Every numeric literal on the line, left to right and non-overlapping.
pub(super) fn number_tokens(line: &str) -> Vec<NumberToken> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        // A digit that continues an identifier (`utf8`, `sha256`) is still a
        // number to vim, and bumping it is usually what was meant. What must
        // NOT happen is reading the `8` of `utf8` as the start of a fresh
        // token when we are already inside one, which the scan below avoids
        // by consuming each token whole.
        if let Some(tok) = radix_prefixed_token(b, i) {
            i = tok.end;
            out.push(tok);
            continue;
        }
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            // A `-` directly before the digits is the sign, as in vim, but
            // only when it is not itself preceded by a digit or an
            // identifier character: in `a-5` the `-` is a sign, in `x1-5`
            // it is a subtraction and `5` is positive.
            let signed = start > 0
                && b[start - 1] == b'-'
                && !line[..start - 1]
                    .chars()
                    .next_back()
                    .is_some_and(is_word_or_digit);
            out.push(NumberToken {
                start: if signed { start - 1 } else { start },
                end: i,
                radix: NumberRadix::Dec,
            });
            continue;
        }
        i += 1;
    }
    out
}

/// `0x…` / `0b…` starting exactly at `i`, or None.
pub(super) fn radix_prefixed_token(b: &[u8], i: usize) -> Option<NumberToken> {
    if b[i] != b'0' || i + 2 >= b.len() {
        return None;
    }
    let (radix, valid): (NumberRadix, fn(u8) -> bool) = match b[i + 1] {
        b'x' | b'X' => (NumberRadix::Hex, |c: u8| c.is_ascii_hexdigit()),
        b'b' | b'B' => (NumberRadix::Bin, |c: u8| c == b'0' || c == b'1'),
        _ => return None,
    };
    let mut j = i + 2;
    while j < b.len() && valid(b[j]) {
        j += 1;
    }
    if j == i + 2 {
        return None;
    }
    Some(NumberToken {
        start: i,
        end: j,
        radix,
    })
}

/// Checked on the character, not the byte: before the `-` in `λ-5` sits a
/// UTF-8 continuation byte, which no byte test reads as a letter.
pub(super) fn is_word_or_digit(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
