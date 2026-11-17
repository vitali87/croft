//! Notes pinned to output, and the text under a masked secret for click-to-reveal.

use super::*;

impl PtyTerminal {
    /// Pin a note to the span starting at grid `line`, columns
    /// `start..start+len` (Cmd+K N's commit). `clock` is the scroll-clock
    /// reading `line` was captured against ([`Self::scroll_clock`] at the
    /// prompt's OPEN) — sampling it here instead would mis-anchor the note
    /// by every row that streamed while the user typed it.
    pub fn add_annotation(&mut self, line: i32, clock: i64, start: u16, len: u16, text: String) {
        self.annotations.push(PaneAnnotation {
            line_rec: line,
            clock_rec: clock,
            start,
            len: len.max(1),
            text,
        });
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Every annotation with its span translated to current grid lines:
    /// `(line, start, len, text)`. Spans whose content fell off the
    /// scrollback are dropped for good.
    pub fn annotations_current(&mut self) -> Vec<(i32, u16, u16, String)> {
        let now = self.tick_clock();
        let top = self.term.lock().grid().topmost_line().0;
        self.annotations
            .retain(|a| a.line_rec - (now - a.clock_rec) as i32 >= top);
        self.annotations
            .iter()
            .map(|a| {
                (
                    a.line_rec - (now - a.clock_rec) as i32,
                    a.start,
                    a.len,
                    a.text.clone(),
                )
            })
            .collect()
    }

    /// Every annotation translated against an EXPLICIT clock reading — the
    /// caller's own snapshot — as `(line, start, len, text)`. The
    /// existing-note lookup in the annotate prompt compares these lines
    /// against a selection captured WITH that reading; translating with a
    /// fresh (later) clock instead let output that scrolled in between
    /// shift the annotations away from the still-fixed selection line, so
    /// annotating the same span opened an empty prompt and duplicated the
    /// note. Pure translation: the scrolled-off GC stays in
    /// [`Self::annotations_current`].
    pub fn annotations_at_clock(&self, clock: i64) -> Vec<(i32, u16, u16, String)> {
        self.annotations
            .iter()
            .map(|a| {
                (
                    a.line_rec - (clock - a.clock_rec) as i32,
                    a.start,
                    a.len,
                    a.text.clone(),
                )
            })
            .collect()
    }

    /// The real text under a masked (redacted) cell at screen `(col, row)`,
    /// for the click-to-reveal popup (#360). None off the pane, off a
    /// mask, or while an alt-screen app owns the viewport.
    pub fn redacted_at(&self, col: u16, row: u16) -> Option<String> {
        let (vr, vc) = self.cell_at(col, row)?;
        let set = self.triggers.lock().unwrap().clone();
        if !set.has_redactions() {
            return None;
        }
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let line = vr as i32 - term.grid().display_offset() as i32;
        let (_, colmap) = row_text_and_cols(&term, line);
        // Matched over the logical line, as the painter masks it: a secret
        // a soft wrap cut in two is found from a click on either half.
        let (text, offset) = logical_row_text(&term, line);
        drop(term);
        // The clicked column as a char index: a wide char's spacer column
        // resolves to the wide char itself (the `line_text_at` rule).
        let vc = vc as usize;
        // `row_text_and_cols` maps every non-spacer column, blanks
        // included, so a click on a blank cell resolves to that blank's own
        // char index - never back into a token. The `rposition` only ever
        // steps back for a wide char's spacer column, which is the wide
        // char itself.
        let ci = offset + colmap.iter().rposition(|&gc| gc <= vc)?;
        crate::triggers::redact_spans(&text, &set)
            .into_iter()
            .find(|s| ci >= s.start && ci < s.start + s.len)
            .map(|s| text.chars().skip(s.start).take(s.len).collect())
    }

    /// The annotation index + note under screen cell (col, row), if any.
    /// Annotations live on the primary screen; while an alt-screen app owns
    /// the viewport their lines describe rows the user cannot see, so no
    /// cell resolves.
    pub fn annotation_at(&self, col: u16, row: u16) -> Option<(usize, String)> {
        let (vr, vc) = self.cell_at(col, row)?;
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let off = term.grid().display_offset() as i32;
        let now = self.clock_now(&term);
        drop(term);
        let line = vr as i32 - off;
        self.annotations
            .iter()
            .enumerate()
            .find(|(_, a)| {
                a.line_rec - (now - a.clock_rec) as i32 == line
                    && vc >= a.start
                    && vc < a.start + a.len
            })
            .map(|(i, a)| (i, a.text.clone()))
    }

    pub fn remove_annotation(&mut self, idx: usize) {
        if idx < self.annotations.len() {
            self.annotations.remove(idx);
            self.pty_dirty.store(true, Ordering::Release);
        }
    }

    pub fn set_annotation_text(&mut self, idx: usize, text: String) {
        if let Some(a) = self.annotations.get_mut(idx) {
            a.text = text;
            self.pty_dirty.store(true, Ordering::Release);
        }
    }
}
