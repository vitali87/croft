//! Selecting text: making, extending and clearing a selection, words and blocks, keeping it
//! on its text as output scrolls, alt-screen selections, the copy cursor, and the text a
//! selection holds.

use super::*;

/// A terminal text selection, anchored to *absolute* alacritty grid lines
/// rather than viewport rows. `line` is the grid `Line` index: `0..rows`
/// is the live screen, negative values are scrollback history. Storing
/// absolute lines (instead of `row - inner.y`) is what lets a selection
/// be taller than the visible pane and survive scrolling — the highlight
/// and the extracted text both track content, not screen position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (i32, u16),
    pub head: (i32, u16),
    /// Rectangular (column-block) selection: the highlight and the copied
    /// text cover the same column slice on every row between the endpoints
    /// (copy mode's Ctrl+V), instead of running row-major to the ends.
    pub block: bool,
}

impl Selection {
    pub fn new(line: i32, col: u16) -> Self {
        Self {
            anchor: (line, col),
            head: (line, col),
            block: false,
        }
    }

    pub fn normalised(&self) -> (i32, u16, i32, u16) {
        let (a_l, a_c) = self.anchor;
        let (b_l, b_c) = self.head;
        if (a_l, a_c) <= (b_l, b_c) {
            (a_l, a_c, b_l, b_c)
        } else {
            (b_l, b_c, a_l, a_c)
        }
    }

    /// Rectangle bounds with rows and columns min/maxed independently
    /// (row-major normalisation would pair the wrong corners when the head
    /// sits below-left of the anchor): (row_lo, col_lo, row_hi, col_hi).
    pub fn block_bounds(&self) -> (i32, u16, i32, u16) {
        let (a_l, a_c) = self.anchor;
        let (b_l, b_c) = self.head;
        (a_l.min(b_l), a_c.min(b_c), a_l.max(b_l), a_c.max(b_c))
    }

    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

impl PtyTerminal {
    /// Stamp freshly-set selection / copy-cursor coordinates against a
    /// fresh clock tick; a selection set on the alternate screen also
    /// captures its content anchor (see `AltSelAnchor`).
    pub(super) fn stamp_selection_clock(&mut self) {
        self.sel_scrolled = self.tick_clock();
        let term = self.term.lock();
        self.alt_sel = if term.mode().contains(TermMode::ALT_SCREEN) {
            self.selection.and_then(|s| capture_alt_anchor(&term, s))
        } else {
            None
        };
    }

    /// Re-anchor an alternate-screen selection to its content after the
    /// app repainted (see `AltSelAnchor`). Finds the vertical shift whose
    /// on-grid rows all match the remembered ones — requiring at least one
    /// non-blank matched row so a blank fingerprint can't latch anywhere —
    /// preferring the smallest movement; no qualifying shift parks the
    /// selection dormant until its content scrolls back into view. A
    /// selection created on the primary screen has no anchor and simply
    /// stays frozen for the duration of the alt-screen trip.
    pub(super) fn rebase_alt_selection(&mut self) {
        if self.selection.is_none() {
            self.alt_sel = None;
            return;
        }
        let Some(mut anchor) = self.alt_sel.take() else {
            return;
        };
        let term = self.term.lock();
        let rows_vis = term.screen_lines() as i32;
        let old_top = anchor.top;
        let k = anchor.rows.len() as i32;
        // One grid snapshot up front (text + char→column maps): the
        // matcher probes every candidate shift, so per-candidate row reads
        // would re-extract (and re-allocate) the same rows dozens of times
        // a frame.
        let grid_rows: Vec<(String, Vec<usize>)> =
            (0..rows_vis).map(|l| row_text_and_cols(&term, l)).collect();
        // Per candidate shift, how many block rows the grid matches
        // exactly at that offset. Rows can go missing not just off-grid:
        // app chrome (Claude Code's input box, its floating pills, an
        // animated status row) overdraws rows anywhere in the block, so
        // partial survival anchors by however many exact rows remain — at
        // least two (when the block has two) including a non-blank one,
        // so a lone repeated divider row can't latch the highlight onto an
        // unrelated copy of itself. More exact rows win; ties go to the
        // shift whose captured neighbour rows also match (a ONE-row block
        // has a one-row fingerprint, and full-screen apps repeat rows
        // verbatim — the neighbours tell its copy from the lookalikes),
        // then to the smallest movement.
        let k_rows = anchor.rows.len();
        let min_exact = k_rows.min(2);
        // (top, exact-row count, matched-neighbour count)
        let mut best: Option<(i32, usize, usize)> = None;
        for top in 1 - k..rows_vis {
            let (mut exact, mut nonblank) = (0usize, false);
            for (i, want) in anchor.rows.iter().enumerate() {
                let line = top + i as i32;
                if (0..rows_vis).contains(&line) && grid_rows[line as usize].0 == *want {
                    exact += 1;
                    nonblank |= !want.trim().is_empty();
                }
            }
            let mut ctx = 0usize;
            for (want, line) in [
                (anchor.ctx_above.as_ref(), top - 1),
                (anchor.ctx_below.as_ref(), top + k),
            ] {
                if let Some(want) = want
                    && (0..rows_vis).contains(&line)
                    && grid_rows[line as usize].0 == *want
                {
                    ctx += 1;
                }
            }
            if exact >= min_exact
                && nonblank
                && best.is_none_or(|(btop, bexact, bctx)| {
                    exact > bexact
                        || (exact == bexact && ctx > bctx)
                        || (exact == bexact
                            && ctx == bctx
                            && (top - old_top).abs() < (btop - old_top).abs())
                })
            {
                best = Some((top, exact, ctx));
            }
        }
        match best {
            Some((top, _, _)) => {
                let d = top - old_top;
                anchor.top = top;
                if d != 0
                    && let Some(s) = self.selection.as_mut()
                {
                    s.anchor.0 += d;
                    s.head.0 += d;
                }
                anchor.dormant = false;
                // The clip: every block row still on screen, whole when its
                // text matches exactly, or just the surviving prefix/suffix
                // columns when app chrome (a floating pill) overdraws part
                // of it. The shift is already pinned by the exact run, so
                // partial credit here can't drag the highlight elsewhere.
                let mut clips: Vec<(i32, u16, u16)> = Vec::new();
                let mut all_exact = true;
                for (i, want) in anchor.rows.iter().enumerate() {
                    let line = top + i as i32;
                    if !(0..rows_vis).contains(&line) {
                        all_exact = false;
                        continue;
                    }
                    let (got, colmap) = &grid_rows[line as usize];
                    if got == want {
                        clips.push((line, 0, u16::MAX));
                    } else {
                        all_exact = false;
                        for (lo, hi) in partial_overlap_cols(want, got, colmap) {
                            clips.push((line, lo, hi));
                        }
                    }
                }
                anchor.visible = (!all_exact).then_some(clips);
                // Refresh the anchor — this is also what folds in a user
                // extension of the selection — but only when every row is
                // in view and intact, so the remembered block never loses
                // a covered or off-screen part mid-scroll.
                if all_exact && let Some(fresh) = capture_alt_anchor(&term, self.selection.unwrap())
                {
                    anchor = fresh;
                }
            }
            // Nothing matched anywhere. Mid-drag that must NOT hide the
            // selection — the drag may have started on a blank row or an
            // animated status line, and the user is pointing at what they
            // see right now; the real anchor is captured on release.
            None if self.drag_selecting => {
                anchor.dormant = false;
                anchor.visible = None;
                if let Some(fresh) = capture_alt_anchor(&term, self.selection.unwrap()) {
                    anchor = fresh;
                }
            }
            None => anchor.dormant = true,
        }
        drop(term);
        self.alt_sel = Some(anchor);
    }

    /// Shift the stored selection and copy-cursor down-anchored coordinates
    /// by the scroll-clock movement since they were recorded, so they keep
    /// naming the same content while output streams. Endpoints whose
    /// content fell off the scrollback pin to the oldest line; a selection
    /// entirely off the buffer is dropped for good. No-op (and no lock)
    /// when neither exists — setters stamp `sel_scrolled` themselves.
    pub(super) fn rebase_selection(&mut self) {
        if self.selection.is_none() && self.copy_cursor.is_none() {
            return;
        }
        if self.term.lock().mode().contains(TermMode::ALT_SCREEN) {
            self.rebase_alt_selection();
            return;
        }
        if self.alt_sel.take().is_some() {
            // The selection was made on the alternate screen and the app
            // has left it: the alternate grid — and the content the
            // coordinates named — is gone with it.
            self.selection = None;
            self.copy_cursor = None;
            return;
        }
        let now = self.tick_clock();
        let top = self.term.lock().grid().topmost_line().0;
        let delta = (now - self.sel_scrolled) as i32;
        self.sel_scrolled = now;
        if delta != 0 {
            if let Some(sel) = self.selection.as_mut() {
                sel.anchor.0 -= delta;
                sel.head.0 -= delta;
            }
            if let Some(cur) = self.copy_cursor.as_mut() {
                cur.0 -= delta;
            }
        }
        if self
            .selection
            .is_some_and(|s| s.anchor.0 < top && s.head.0 < top)
        {
            self.selection = None;
        } else if let Some(sel) = self.selection.as_mut() {
            sel.anchor.0 = sel.anchor.0.max(top);
            sel.head.0 = sel.head.0.max(top);
        }
        if let Some(cur) = self.copy_cursor.as_mut() {
            cur.0 = cur.0.max(top);
        }
    }

    pub fn start_selection_at(&mut self, col: u16, row: u16) {
        if let Some((r, c)) = self.cell_at(col, row) {
            let line = r as i32 - self.display_offset();
            self.selection = Some(Selection::new(line, c));
            self.drag_selecting = true;
            self.stamp_selection_clock();
        }
    }

    /// Test-only: the raw state behind the corrected selection accessors —
    /// (stored selection, sel_scrolled, clock_base, alt anchor top, alt
    /// mode) — for diagnosing drift math from app-level tests.
    #[cfg(test)]
    pub fn debug_sel_state(&self) -> (Option<Selection>, i64, i64, Option<i32>, bool) {
        (
            self.selection,
            self.sel_scrolled,
            self.clock.lock().unwrap().base,
            self.alt_sel.as_ref().map(|a| a.top),
            self.term.lock().mode().contains(TermMode::ALT_SCREEN),
        )
    }

    /// The mouse button came up: the drag-selection is final. Capture the
    /// definitive content anchor now — during the drag the selection
    /// followed the pointer over whatever the grid showed.
    pub fn end_drag(&mut self) {
        if self.drag_selecting {
            self.drag_selecting = false;
            self.stamp_selection_clock();
        }
    }

    pub fn extend_selection_to(&mut self, col: u16, row: u16) {
        self.drag_selecting = true;
        self.rebase_selection();
        let cell = self.cell_at(col, row);
        let off = self.display_offset();
        if let (Some(sel), Some((r, c))) = (self.selection.as_mut(), cell) {
            sel.head = (r as i32 - off, c);
        }
    }

    /// Extend a drag-selection toward a pointer that may have left the
    /// pane. Columns and rows are clamped to the inner content rect; the
    /// returned value is the auto-scroll direction the caller should keep
    /// applying while the pointer stays past an edge: `-1` (pointer above
    /// the top, scroll into history), `+1` (below the bottom, scroll
    /// toward live), or `0` (inside the pane, no auto-scroll). Mirrors
    /// the click-drag-past-the-edge selection growth in iTerm2/VS Code.
    pub fn drag_select_to(&mut self, col: u16, row: u16) -> i32 {
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return 0;
        }
        self.drag_selecting = true;
        let top = inner.y;
        let bottom = inner.y + inner.height - 1;
        let (vp_row, dir) = if row < top {
            (0u16, -1)
        } else if row > bottom {
            (inner.height - 1, 1)
        } else {
            (row - inner.y, 0)
        };
        let max_x = inner.x + inner.width - 1;
        let c = col.clamp(inner.x, max_x) - inner.x;
        self.rebase_selection();
        let off = self.display_offset();
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (vp_row as i32 - off, c);
        }
        dir
    }

    /// One step of edge auto-scroll while a drag-selection is held past
    /// the top (`dir < 0`) or bottom (`dir > 0`) edge. Scrolls the
    /// viewport by one row and re-pins the selection head to the new edge
    /// line at the last-known drag column. On the alternate screen the
    /// inner program owns scrolling, so a mouse-tracking app (Claude Code,
    /// a pager) gets a wheel report at the edge cell instead — it scrolls
    /// its own content, the content anchor drags the selection along, and
    /// the head re-pins each tick.
    pub fn autoscroll_select(&mut self, dir: i32, col: u16) {
        if dir == 0 {
            return;
        }
        if self.term.lock().mode().contains(TermMode::ALT_SCREEN) {
            let inner = self.last_inner;
            if inner.width == 0 || inner.height == 0 || !self.mouse_reporting() {
                return;
            }
            let edge_row = if dir < 0 {
                inner.y
            } else {
                inner.y + inner.height - 1
            };
            let ccol = col.clamp(inner.x, inner.x + inner.width - 1);
            let button = if dir < 0 {
                MouseButtonKind::WheelUp
            } else {
                MouseButtonKind::WheelDown
            };
            self.report_mouse(
                button,
                MouseAction::Press,
                ccol,
                edge_row,
                MouseMods::default(),
            );
            self.rebase_selection();
            if let Some(sel) = self.selection.as_mut() {
                sel.head = (i32::from(edge_row - inner.y), ccol - inner.x);
            }
            return;
        }
        let scrolled = if dir < 0 {
            self.scroll_up(1)
        } else {
            self.scroll_down(1)
        };
        if !scrolled {
            return;
        }
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let max_x = inner.x + inner.width - 1;
        let c = col.clamp(inner.x, max_x) - inner.x;
        let vp_row = if dir < 0 { 0u16 } else { inner.height - 1 };
        self.rebase_selection();
        let off = self.display_offset();
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (vp_row as i32 - off, c);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.alt_sel = None;
    }

    /// Double-click word-select: expand the selection to cover the word
    /// under the screen coordinate `(col, row)`. No-op when the click
    /// lands on whitespace / punctuation or outside the live area.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        let Some((r, c)) = self.cell_at(col, row) else {
            return;
        };
        let term = self.term.lock();
        let display_offset = term.grid().display_offset();
        let Some((anchor, head)) =
            select_word_at_in_term(&term, display_offset, r as usize, c as usize)
        else {
            return;
        };
        drop(term);
        // `select_word_at_in_term` reports viewport rows; anchor them to
        // absolute grid lines so the selection stays put when scrolled.
        let off = display_offset as i32;
        let to_abs = |(vr, vc): (u16, u16)| (vr as i32 - off, vc);
        self.selection = Some(Selection {
            anchor: to_abs(anchor),
            head: to_abs(head),
            block: false,
        });
        self.stamp_selection_clock();
    }

    /// The selection with its endpoints corrected for the scroll-clock
    /// movement since they were recorded, so callers always see coordinates
    /// that name the content the user selected (see `rebase_selection`).
    pub fn selection(&self) -> Option<Selection> {
        let mut sel = self.selection?;
        let delta = (self.clock_now(&self.term.lock()) - self.sel_scrolled) as i32;
        sel.anchor.0 -= delta;
        sel.head.0 -= delta;
        Some(sel)
    }

    /// Replace (or clear) the selection wholesale. Copy mode drives the
    /// selection from the keyboard through this, bypassing the mouse path.
    pub fn set_selection(&mut self, sel: Option<Selection>) {
        self.rebase_selection();
        self.selection = sel;
        self.stamp_selection_clock();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Show / move / hide the copy-mode cursor block.
    pub fn set_copy_cursor(&mut self, cur: Option<(i32, u16)>) {
        self.rebase_selection();
        self.copy_cursor = cur;
        self.stamp_selection_clock();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// The shell cursor's cell in absolute grid coords (line can be
    /// negative only transiently; the live screen is 0-based): where copy
    /// mode starts.
    pub fn cursor_line_col(&self) -> (i32, u16) {
        let term = self.term.lock();
        let p = term.grid().cursor.point;
        (p.line.0, p.column.0 as u16)
    }

    /// The drift-corrected selection together with the scroll-clock reading
    /// it is valid against, from ONE terminal snapshot. Callers that pair a
    /// grid coordinate with a clock (the annotation prompt) must use this:
    /// reading them through separate locks lets PTY output land in between,
    /// pairing a pre-output line with a post-output clock and anchoring to
    /// the wrong content row.
    pub fn selection_and_clock(&mut self) -> (Option<Selection>, i64) {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let sel = self.selection.map(|mut s| {
            let delta = (now - self.sel_scrolled) as i32;
            s.anchor.0 -= delta;
            s.head.0 -= delta;
            s
        });
        (sel, now)
    }

    /// Cursor cell, grid bounds, and the scroll-clock reading from ONE
    /// terminal snapshot (copy mode's open — same atomicity argument as
    /// [`Self::selection_and_clock`]).
    #[allow(clippy::type_complexity)]
    pub fn cursor_bounds_and_clock(&mut self) -> ((i32, u16), (i32, i32, u16, u16), i64) {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let p = term.grid().cursor.point;
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        (
            (p.line.0, p.column.0 as u16),
            (top, bottom, self.cols, self.rows),
            now,
        )
    }

    pub fn selection_text(&self) -> String {
        // An alt-screen selection scrolled (partly) out of the app's view
        // names content the grid no longer fully holds — dormant, or
        // clipped by app chrome. The anchor remembered the text, so copy
        // still yields the whole highlighted block.
        if let Some(anchor) = self
            .alt_sel
            .as_ref()
            .filter(|a| a.dormant || a.visible.is_some())
        {
            return anchor.text.clone();
        }
        let Some(mut sel) = self.selection else {
            return String::new();
        };
        let term = self.term.lock();
        // Same scroll-clock correction as `selection()`, computed under
        // the lock already held for extraction.
        let delta = (self.clock_now(&term) - self.sel_scrolled) as i32;
        sel.anchor.0 -= delta;
        sel.head.0 -= delta;
        if sel.block {
            let (rl, cl, rh, ch) = sel.block_bounds();
            return block_selection_text(&term, rl, cl as usize, rh, ch as usize);
        }
        let (sr, sc, er, ec) = sel.normalised();
        extract_selection_text(&term, sr, sc as usize, er, ec as usize)
    }
}

/// The surviving column spans of a block row that app chrome partially
/// overdrew: the longest common prefix and suffix between the remembered
/// row and what the grid shows. Returns the grid-column ranges to keep
/// highlighted — a pill floating over the middle of a row leaves BOTH
/// intact ends lit.
pub(super) fn partial_overlap_cols(want: &str, got: &str, colmap: &[usize]) -> Vec<(u16, u16)> {
    // Row text is full grid width: strip the trailing blank run or it
    // counts as a huge shared suffix between any two rows.
    let want = want.trim_end();
    let got = got.trim_end();
    let want_n = want.chars().count();
    let got_n = got.chars().count();
    if want_n == 0 || got_n == 0 {
        return Vec::new();
    }
    let prefix = want
        .chars()
        .zip(got.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = want
        .chars()
        .rev()
        .zip(got.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    // Each surviving end must carry real content, not just indentation.
    // No minimum share of the row: a pill over the MIDDLE leaves both
    // ends under half, and this credit is paint-only — the shift was
    // already anchored by exact rows, so it can't move the highlight.
    let solid =
        |chars: &mut dyn Iterator<Item = char>| chars.filter(|c| !c.is_whitespace()).count() >= 4;
    let good_prefix = prefix > 0 && solid(&mut want.chars().take(prefix));
    let good_suffix = suffix > 0 && solid(&mut want.chars().rev().take(suffix));
    let mut spans = Vec::new();
    if good_prefix && good_suffix && prefix + suffix >= got_n {
        // The ends overlap: the row differs by a few mid-row cells (an
        // animated counter ticking inside the selection) — keep it whole.
        spans.push((colmap[0] as u16, colmap[got_n - 1] as u16));
    } else {
        if good_prefix {
            spans.push((colmap[0] as u16, colmap[prefix - 1] as u16));
        }
        if good_suffix {
            spans.push((colmap[got_n - suffix] as u16, colmap[got_n - 1] as u16));
        }
    }
    spans
}

/// Capture the content anchor for an alternate-screen selection: its row
/// text and extracted text as the grid shows them now. `None` when any
/// selected line is outside the grid (a partially scrolled-out block keeps
/// its previous, fuller anchor instead).
pub(super) fn capture_alt_anchor(
    term: &Term<VoidListener>,
    sel: Selection,
) -> Option<AltSelAnchor> {
    let rows_vis = term.screen_lines() as i32;
    let (lo, hi) = (sel.anchor.0.min(sel.head.0), sel.anchor.0.max(sel.head.0));
    if lo < 0 || hi >= rows_vis {
        return None;
    }
    let rows = (lo..=hi).map(|l| row_text_and_cols(term, l).0).collect();
    let ctx_above = (lo > 0).then(|| row_text_and_cols(term, lo - 1).0);
    let ctx_below = (hi + 1 < rows_vis).then(|| row_text_and_cols(term, hi + 1).0);
    let text = if sel.block {
        let (rl, cl, rh, ch) = sel.block_bounds();
        block_selection_text(term, rl, cl as usize, rh, ch as usize)
    } else {
        let (sr, sc, er, ec) = sel.normalised();
        extract_selection_text(term, sr, sc as usize, er, ec as usize)
    };
    Some(AltSelAnchor {
        rows,
        top: lo,
        text,
        dormant: false,
        visible: None,
        ctx_above,
        ctx_below,
    })
}

pub fn extract_selection_text(
    term: &Term<VoidListener>,
    sr: i32,
    sc: usize,
    er: i32,
    ec: usize,
) -> String {
    let cols = term.columns();
    // Clamp the range to the grid: the newest live line is
    // `screen_lines - 1`; the oldest readable line is the grid's topmost
    // (scrollback floor). Reading outside that panics alacritty's index.
    let max_line = term.screen_lines() as i32 - 1;
    let min_line = term.grid().topmost_line().0;
    let sr = sr.max(min_line);
    let er = er.min(max_line);
    let mut out = String::new();
    let mut line_idx = sr;
    while line_idx <= er {
        let row_start = if line_idx == sr { sc } else { 0 };
        let row_end = if line_idx == er {
            ec.min(cols.saturating_sub(1))
        } else {
            cols.saturating_sub(1)
        };
        let mut line = String::new();
        for col in row_start..=row_end {
            let p = Point::new(Line(line_idx), Column(col));
            let cell = &term.grid()[p];
            // A wide char's spacer cell holds no glyph of its own — skip it
            // so copied CJK/emoji text comes out contiguous.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let c = cell.c;
            if c == '\0' {
                line.push(' ');
            } else {
                line.push(c);
            }
            // Combining marks ride on the cell after its base char: `café`
            // typed as `e` + U+0301 copied as `cafe` without them.
            if let Some(marks) = cell.zerowidth() {
                line.extend(marks.iter());
            }
        }
        // A soft-wrapped row (WRAPLINE on its last cell) continues the
        // same logical line: no separator, so copied text, the durable
        // command history, and the sticky header all see the line the user
        // actually typed — a '\n' here corrupted stored commands and re-ran
        // only their first fragment on paste. A hard row break keeps the
        // newline. Only a hard break is trimmed: a space in a wrapped row's
        // last column is part of the line (`echo abcd efg` in ten columns
        // copied as `echo abcdefg`).
        let wrapped = line_idx != er
            && cols > 0
            && term.grid()[Point::new(Line(line_idx), Column(cols - 1))]
                .flags
                .contains(Flags::WRAPLINE);
        out.push_str(if wrapped { &line } else { line.trim_end() });
        if line_idx != er && !wrapped {
            out.push('\n');
        }
        line_idx += 1;
    }
    out
}

/// Inspect a row of `term` around `(row, col)` (both viewport-relative)
/// and return the anchor/head pair that brackets the contiguous run of
/// word characters covering the pivot. `display_offset` is the
/// alacritty grid's current scroll-back offset so the lookup
/// `Line(row - display_offset)` resolves to the scrollback cell when
/// the user has scrolled into history. Without this, a double-click
/// on text the user sees on screen but lives in scrollback would read
/// the live grid at the same viewport y, which is usually blank — so
/// `is_terminal_word_char` returns false and the function returns
/// `None`, giving the user-visible symptom "double-click doesn't
/// auto-select."
///
/// Returns `None` when the pivot sits on a non-word character
/// (whitespace, punctuation), so a double click between words is a
/// no-op rather than a spurious selection. Word semantics match
/// `widgets::editor::is_word_char`: alphanumeric + underscore. Pure
/// function so the test suite can exercise it without spawning a PTY.
pub fn select_word_at_in_term(
    term: &Term<VoidListener>,
    display_offset: usize,
    row: usize,
    col: usize,
) -> Option<((u16, u16), (u16, u16))> {
    let cols = term.columns();
    if col >= cols {
        return None;
    }
    let row_idx = row as i32 - display_offset as i32;
    let cell_char = |c: usize| -> char {
        let p = Point::new(Line(row_idx), Column(c));
        let ch = term.grid()[p].c;
        if ch == '\0' { ' ' } else { ch }
    };
    if !is_terminal_word_char(cell_char(col)) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_terminal_word_char(cell_char(start - 1)) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cols && is_terminal_word_char(cell_char(end + 1)) {
        end += 1;
    }
    Some(((row as u16, start as u16), (row as u16, end as u16)))
}

pub fn is_terminal_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The text a rectangular selection covers: the same inclusive column
/// slice `[cl..=ch]` from every row `[rl..=rh]`, one line per row (what
/// vim's Ctrl+V yank produces). Rows are clamped to the readable grid the
/// same way `extract_selection_text` clamps.
pub fn block_selection_text(
    term: &Term<VoidListener>,
    rl: i32,
    cl: usize,
    rh: i32,
    ch: usize,
) -> String {
    let max_line = term.screen_lines() as i32 - 1;
    let min_line = term.grid().topmost_line().0;
    let rl = rl.max(min_line);
    let rh = rh.min(max_line);
    let mut rows = Vec::new();
    for line in rl..=rh {
        rows.push(extract_selection_text(term, line, cl, line, ch));
    }
    rows.join("\n")
}

/// True iff (row, col) is inside the inclusive rectangle whose corners are
/// (rl, cl) and (rh, ch) — [`Selection::block_bounds`] output. Public for
/// unit testing.
pub fn cell_in_block_selection(row: i32, col: u16, rl: i32, cl: u16, rh: i32, ch: u16) -> bool {
    row >= rl && row <= rh && col >= cl && col <= ch
}

/// True iff (row, col) is inside the inclusive row-major range
/// [(sr,sc)..=(er,ec)]. Public for unit testing.
pub fn cell_in_selection(row: i32, col: u16, sr: i32, sc: u16, er: i32, ec: u16) -> bool {
    if row < sr || row > er {
        return false;
    }
    if row == sr && col < sc {
        return false;
    }
    if row == er && col > ec {
        return false;
    }
    true
}
