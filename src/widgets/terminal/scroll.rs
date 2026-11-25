//! Scrolling and resizing: the scrollback offset, scrolling to a line, clearing the screen
//! and scrollback, and reflowing the grid on resize.

use super::*;

impl PtyTerminal {
    /// The viewport's scroll offset into history (0 = live bottom). Public
    /// so the app can map an anchor's grid line to a viewport row.
    pub fn scroll_display_offset(&self) -> i32 {
        self.display_offset()
    }

    /// Scroll the viewport so absolute grid line `abs_line` sits near the
    /// middle of the pane, clamped to the scrollback range. No-op in
    /// alternate screen, where there is no scrollback to move through and the
    /// whole grid is already on screen.
    pub fn scroll_to_line(&mut self, abs_line: i32) {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let rows = term.screen_lines() as i32;
        let max_off = (-term.grid().topmost_line().0).max(0);
        let cur = term.grid().display_offset() as i32;
        let desired = scroll_offset_for_line(rows, max_off, abs_line);
        let delta = desired - cur;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// The viewport-top grid line: what a prompt jump navigates from.
    pub fn viewport_top_line(&self) -> i32 {
        -(self.term.lock().grid().display_offset() as i32)
    }

    /// Current scrollback offset: how many rows the viewport is scrolled
    /// up from the live bottom. Viewport row `r` maps to absolute grid
    /// line `r - display_offset`.
    pub(super) fn display_offset(&self) -> i32 {
        self.term.lock().grid().display_offset() as i32
    }

    /// Scrollback rows held, the live screen's rows, and the grid's width,
    /// for the memory report (#694).
    pub fn grid_extent(&self) -> (usize, usize, usize) {
        let term = self.term.lock();
        (
            term.grid().history_size(),
            term.screen_lines(),
            term.columns(),
        )
    }

    /// The readable grid range and viewport size for keyboard navigation:
    /// (oldest line, newest line, columns, viewport rows).
    pub fn grid_bounds(&self) -> (i32, i32, u16, u16) {
        let term = self.term.lock();
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        (top, bottom, self.cols, self.rows)
    }

    /// Scroll the visible viewport up by `n` rows into scrollback.
    /// Returns false if the inner program is in alternate-screen mode
    /// (apps like vim/htop/less manage their own scrolling).
    pub fn scroll_up(&mut self, n: usize) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Delta(n as i32));
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    pub fn scroll_down(&mut self, n: usize) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Delta(-(n as i32)));
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    /// Jump the viewport to the oldest scrollback line (Shift+Home). No-op
    /// in the alternate screen, which has no scrollback.
    pub fn scroll_to_top(&mut self) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Top);
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    /// Jump the viewport back to the live bottom (Shift+End). Returns false
    /// in the alternate screen — no scrollback there, so the chord belongs
    /// to the program, exactly like its three scrolling siblings.
    pub fn reset_scrollback(&mut self) -> bool {
        let mut term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        term.scroll_display(Scroll::Bottom);
        self.pty_dirty.store(true, Ordering::Release);
        true
    }

    /// Clear the visible screen and scrollback history (VS Code's terminal
    /// "Clear"), homing the cursor. Feeds the standard erase sequences into the
    /// grid (`ED 3` wipes scrollback, `ED 2` the screen); the shell redraws its
    /// prompt on the next keystroke. Does not touch the running program.
    pub fn clear_screen_and_scrollback(&mut self) {
        let mut processor = Processor::<StdSyncHandler>::new();
        {
            let mut term = self.term.lock();
            // ED 2 BEFORE ED 3 (xterm.js / VS Code's clear() order):
            // alacritty's primary-screen ED 2 scrolls the viewport INTO
            // history rather than blanking in place, so erasing history
            // first would leave the "cleared" rows alive in scrollback.
            processor.advance(&mut *term, b"\x1b[2J\x1b[3J\x1b[H");
            term.scroll_display(Scroll::Bottom);
            // The captured lines' rows go too: a CAPTURES entry must say its
            // line is gone, not jump to a copy printed after the Clear
            // (#1450). Cleared under the same `term` lock as the grid (lock
            // order term -> capture_rows): the reader records a new capture
            // while it holds `term`, so dropping the lock first would let a
            // line printed in between keep its row on screen but lose it here.
            self.capture_rows.lock().unwrap().rows.clear();
        }
        // The content the images and OSC 133 marks anchored to is gone;
        // keeping either leaves overlays (a floating picture, phantom
        // decoration dots whose menus copy or re-run the wrong rows) over
        // the fresh prompt.
        self.images.lock().unwrap().clear();
        self.marks.lock().unwrap().clear();
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// The PTY grid's current column count.
    ///
    /// Test-only, and the point of it is #313: a collapsed pane must not be
    /// reflowed into its one-column strip. `resize` clamps to two columns, so
    /// a collapsed pane that got rendered would not crash - it would quietly
    /// rewrap the running shell to a two-column grid and lose everything on
    /// its screen. Reading the real grid width is what tells that apart from
    /// a pane that was simply left alone.
    #[cfg(test)]
    pub fn grid_cols(&self) -> u16 {
        self.cols
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        // A degenerate pane (a host window shrunk to a sliver leaves the
        // layout no room) must not reach alacritty: Term::resize panics on
        // a zero/one-cell grid mid-reflow. Keep the last sane size; the
        // grid simply clips until the window grows back.
        let (cols, rows) = (cols.max(2), rows.max(2));
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        *self.size_shared.lock().unwrap() = (cols, rows);
        // Grid first, pty second, under one hold of the grid lock (#473).
        // The pty ioctl delivers SIGWINCH, and a full-screen child answers
        // it with a complete repaint sized to the NEW dimensions. If that
        // repaint reaches the reader before the grid has been resized, it
        // is parsed into the old grid: rows land on the wrong lines, wide
        // lines wrap, and once the grid does grow those cells sit where a
        // diff-based renderer (Claude Code, and anything else that only
        // redraws what it believes changed) considers them already blank.
        // Nothing ever repaints them, and the stale text stays on screen.
        // Resizing the grid while holding the lock across the ioctl means
        // every byte the child emits in reply meets a grid of the right
        // size.
        let mut term = self.term.lock();
        let size = TermSize::new(cols as usize, rows as usize);
        let mut clock = self.clock.lock().unwrap();
        self.capture_rows
            .lock()
            .unwrap()
            .reflow(&mut term, &mut clock, |term| resize_term(term, size));
        drop(clock);
        self.backend.resize(cols, rows);
        drop(term);
        self.pty_dirty.store(true, Ordering::Release);
    }
}

/// A `Flags` bit alacritty leaves unused (its own stop at bit 14), set on
/// every cell of a row [`resize_term`] clipped. Writing a cell replaces its
/// flags, so a row whose shown cells all still carry it is one no program
/// has written to since: the cells kept past its edge still belong to it.
pub(super) const CLIPPED: Flags = Flags::from_bits_retain(0x8000);

/// Resize a pane's model, clipping rows rather than re-wrapping them (#1857).
///
/// alacritty reflows every row wider than a narrowed pane onto the row
/// below. Anything a program laid out to the old width falls apart: each
/// row of a Rich panel or a table splits in two, its right border lands
/// halfway along the next row, and a program that redraws by counting rows
/// up from the cursor misses the extra ones. So a row the program ended
/// itself (no soft wrap into it or out of it) is cut at the new width
/// instead. The cells cut off are kept past the row's last shown column,
/// where nothing draws or writes, and widening shows them again. A row a
/// program wrote to while cut drops them, since they no longer belong to
/// what it shows. Soft-wrapped lines still reflow, and so does the cursor's
/// row, as the shell redrawing a prompt expects. The primary screen is out
/// of reach while a full-screen program holds the alternate one, so a
/// resize then reflows it as before.
pub(crate) fn resize_term(term: &mut Term<VoidListener>, size: TermSize) {
    let (old, new) = (term.columns(), size.columns());
    if old == new || term.mode().contains(TermMode::ALT_SCREEN) {
        term.resize(size);
        return;
    }
    let grid = term.grid_mut();
    let cursor = grid.cursor.point.line.0;
    // A shorter pane drops the screen rows from here down (alacritty keeps
    // the cursor's row on it); a tail queued for one would go on another.
    let kept = (size.screen_lines() as i32).max(cursor + 1);
    let mut tails = Vec::new();
    let mut wrapped_into = false;
    for l in grid.topmost_line().0..=grid.bottommost_line().0 {
        let row = &mut grid[Line(l)];
        let restored = unclip_row(row, old);
        let wraps = !restored && row[Column(old - 1)].flags.contains(Flags::WRAPLINE);
        let overflows = row[..].iter().skip(new).any(|c| !c.is_empty());
        if overflows && l < kept && !wrapped_into && !wraps && (new > old || l != cursor) {
            let mut tail = clip_row(row, new);
            if new > old {
                // alacritty only grows a row narrower than the pane.
                row.append(&mut tail);
            } else {
                // Narrowing would reflow the tail; it goes back on after.
                tails.push(tail);
            }
        }
        wrapped_into = wraps;
    }
    term.resize(size);
    // The rows just clipped are the only marked ones, still in order, and
    // the oldest are the ones a full scrollback drops: match from the bottom.
    let grid = term.grid_mut();
    let mut tails = tails.into_iter().rev();
    for l in (grid.topmost_line().0..=grid.bottommost_line().0).rev() {
        let row = &mut grid[Line(l)];
        if row[Column(0)].flags.contains(CLIPPED)
            && let Some(mut tail) = tails.next()
        {
            row.append(&mut tail);
        }
    }
}

/// Cut `row` to show `cols` cells, marking them and returning the rest
/// (trailing blanks dropped), marked too. A wide char the edge would split
/// goes whole into the rest, leaving alacritty's placeholder for a wide
/// char moved on to the next row.
pub(super) fn clip_row(row: &mut Row<Cell>, cols: usize) -> Vec<Cell> {
    let mut cells = row[..].to_vec();
    let end = cells
        .iter()
        .rposition(|c| !c.is_empty())
        .map_or(0, |i| i + 1);
    cells.truncate(end);
    let cut = if cells[cols - 1].flags.contains(Flags::WIDE_CHAR) {
        cols - 1
    } else {
        cols
    };
    let mut tail = cells.split_off(cut);
    if cut < cols {
        cells.push(Cell {
            flags: Flags::LEADING_WIDE_CHAR_SPACER,
            ..Cell::default()
        });
    }
    for cell in cells.iter_mut().chain(&mut tail) {
        cell.flags.insert(CLIPPED);
    }
    *row = Row::from_vec(cells, cols);
    tail
}

/// Undo [`clip_row`] on a row showing `cols` cells: put back what it kept
/// past its edge, or drop that when a program has written to the row since.
/// Whether the row came back whole.
pub(super) fn unclip_row(row: &mut Row<Cell>, cols: usize) -> bool {
    if row.len() <= cols {
        // A clipped row alacritty reflowed under a full-screen program.
        if row[Column(0)].flags.contains(CLIPPED) {
            for cell in &mut row[..] {
                cell.flags.remove(CLIPPED);
            }
        }
        return false;
    }
    let intact = row[..Column(cols)]
        .iter()
        .all(|c| c.flags.contains(CLIPPED));
    let mut cells = row[..].to_vec();
    if !intact {
        cells.truncate(cols);
    } else if cells[cols - 1]
        .flags
        .contains(Flags::LEADING_WIDE_CHAR_SPACER)
    {
        cells.remove(cols - 1);
    }
    for cell in &mut cells {
        cell.flags.remove(CLIPPED);
    }
    let len = cells.len();
    *row = Row::from_vec(cells, len);
    intact
}

/// The display offset that brings absolute grid line `abs_line` to the
/// vertical middle of a `rows`-tall viewport, clamped to `[0, max_off]` (0 =
/// live bottom, `max_off` = oldest scrollback). The render loop shows the
/// grid line `y - display_offset` at viewport row `y`, so centering the
/// target means `display_offset = rows/2 - abs_line`. Pure for testing.
pub fn scroll_offset_for_line(rows: i32, max_off: i32, abs_line: i32) -> i32 {
    (rows / 2 - abs_line).clamp(0, max_off.max(0))
}

/// Scroll the locked `term` so absolute grid line `abs_line` sits at the top
/// of the pane (VS Code parks the jumped-to command at the top). No-op in
/// alternate screen, like the other scrollback moves. The caller marks the
/// pane dirty once it lets go of the lock.
pub(super) fn scroll_line_to_top(term: &mut Term<VoidListener>, abs_line: i32) {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return;
    }
    let max_off = (-term.grid().topmost_line().0).max(0);
    let desired = (-abs_line).clamp(0, max_off);
    let delta = desired - term.grid().display_offset() as i32;
    if delta != 0 {
        term.scroll_display(Scroll::Delta(delta));
    }
}
