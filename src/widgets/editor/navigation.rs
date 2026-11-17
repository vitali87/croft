//! Moving the caret and the view: arrows, words, Home and End, pages, Go to Line,
//! scrolling, the minimap and the scrollbar.

use super::*;

impl Editor {
    /// Clamp the cursor back inside the buffer after an edit that may have
    /// removed rows or shortened the cursor's line (e.g. a whole-document
    /// reformat). Without this a stale `cursor_row`/`cursor_col` can index past
    /// the end of `lines` on the next keystroke.
    pub fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.cursor_row = 0;
            self.cursor_col = 0;
            return;
        }
        self.cursor_row = self.cursor_row.min(self.lines.len() - 1);
        let max_col = self.lines[self.cursor_row].chars().count();
        self.cursor_col = self.cursor_col.min(max_col);
    }

    pub fn move_up(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(-1);
        } else if self.cursor_row > 0 {
            // Step over a collapsed region, as `move_down` does.
            self.cursor_row = self.prev_visible_line(self.cursor_row - 1);
            self.cursor_col = floor_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
        }
        self.last_edit_kind = None;
    }

    pub fn move_down(&mut self) {
        if self.wrap_enabled() {
            self.wrap_move_vertical(1);
        } else if self.cursor_row + 1 < self.lines.len() {
            // Step OVER a collapsed region rather than into it, the way VS Code
            // does: the fold stays shut and the caret lands on the next line
            // the user can actually see.
            if let Some(next) = self.next_visible_line(self.cursor_row + 1) {
                self.cursor_row = next;
                self.cursor_col = floor_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
            }
        }
        self.last_edit_kind = None;
    }

    /// The Up arrow: [`Self::move_up`], and on the first line (or wrapped
    /// row) the caret goes to the start of the line, as in VS Code, so
    /// Shift+Up selects to the start of the file (#1563). Vim's `k` keeps
    /// `move_up`, which stops on the first line as Vim does.
    pub fn arrow_up(&mut self) {
        let before = (self.cursor_row, self.cursor_col);
        self.move_up();
        if (self.cursor_row, self.cursor_col) == before {
            self.cursor_col = 0;
            self.ensure_cursor_col_visible();
        }
    }

    /// The Down arrow: [`Self::move_down`], and on the last line (or
    /// wrapped row) the caret goes to the end of the line, so Shift+Down
    /// selects through it instead of stopping at its start (#1563).
    pub fn arrow_down(&mut self) {
        let before = (self.cursor_row, self.cursor_col);
        self.move_down();
        if (self.cursor_row, self.cursor_col) == before {
            self.cursor_col = self.line_char_len(self.cursor_row);
            self.ensure_cursor_col_visible();
        }
    }

    pub fn goto_top(&mut self) {
        self.clear_selection();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn goto_bottom(&mut self) {
        self.clear_selection();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// `Ctrl+Home` (`Cmd+Home` on macOS), VS Code's `cursorTop`. Unlike
    /// `goto_top` it keeps the selection, so `Shift` extends it.
    pub fn cursor_top(&mut self) {
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// `Ctrl+End`, VS Code's `cursorBottom`: after the last line's last
    /// character, keeping the selection like `cursor_top`.
    pub fn cursor_bottom(&mut self) {
        self.cursor_row = self.lines.len().saturating_sub(1);
        self.cursor_col = self.line_char_len(self.cursor_row);
        self.last_edit_kind = None;
    }

    pub fn goto_line(&mut self, one_based: usize) {
        self.clear_selection();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let max = self.lines.len() - 1;
        let target = one_based.saturating_sub(1).min(max);
        self.cursor_row = target;
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col = prev_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.line_char_len(self.cursor_row) {
            self.cursor_col = next_grapheme_col(&self.lines[self.cursor_row], self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    /// Word-step right (Option+Right on macOS, Ctrl+Right elsewhere). Skips
    /// any non-word run starting at the cursor — across line boundaries —
    /// then skips the following word run on the line where it landed and
    /// stops. Word chars are ASCII alphanumerics + underscore; everything
    /// else (whitespace, punctuation, EOL) is non-word. Mirrors readline
    /// `forward-word` and VS Code's "Cursor Word Right" command.
    pub fn move_word_right(&mut self) {
        loop {
            let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            while self.cursor_col < chars.len() && !word_char_at(&chars, self.cursor_col) {
                self.cursor_col += 1;
            }
            if self.cursor_col < chars.len() {
                break;
            }
            if self.cursor_row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
            } else {
                self.ensure_cursor_col_visible();
                self.last_edit_kind = None;
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while self.cursor_col < chars.len() && word_char_at(&chars, self.cursor_col) {
            self.cursor_col += 1;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    /// Word-step left (Option+Left on macOS). Symmetric to `move_word_right`:
    /// skips non-word chars backwards across line boundaries, then walks
    /// back over the preceding word run, landing on its first char.
    pub fn move_word_left(&mut self) {
        loop {
            let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            self.cursor_col = self.cursor_col.min(chars.len());
            while self.cursor_col > 0 && !word_char_at(&chars, self.cursor_col - 1) {
                self.cursor_col -= 1;
            }
            if self.cursor_col > 0 {
                break;
            }
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                self.cursor_col = self.line_char_len(self.cursor_row);
            } else {
                self.ensure_cursor_col_visible();
                self.last_edit_kind = None;
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while self.cursor_col > 0 && word_char_at(&chars, self.cursor_col - 1) {
            self.cursor_col -= 1;
        }
        self.ensure_cursor_col_visible();
        self.last_edit_kind = None;
    }

    pub fn scroll_left_by(&mut self, n: usize) {
        self.scroll_col = self.scroll_col.saturating_sub(n);
    }

    pub fn scroll_right_by(&mut self, n: usize) {
        self.scroll_col = self.scroll_col.saturating_add(n);
    }

    /// Recompute the visible text width from `last_inner` / `last_gutter_width`
    /// (set during the most recent render) and use it to decide whether the
    /// current cursor column is on-screen. This is the same arithmetic the
    /// renderer uses, kept in one place so move-and-scroll stays in lock-step.
    pub(super) fn visible_text_width(&self) -> usize {
        let scrollbar_w = u16::from(self.last_scrollbar.width > 0);
        self.last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + scrollbar_w) as usize
    }

    /// Pull `scroll_col` so the cursor sits inside the current viewport.
    /// Mirrors `ensure_cursor_visible` for vertical scroll - keyboard nav
    /// follows; wheel scrolling does not call this so the user's wheel
    /// position is sacrosanct. `pub(crate)` so callers like
    /// `App::open_search_hit` can sync horizontal scroll after a jump.
    pub(crate) fn ensure_cursor_col_visible(&mut self) {
        if self.wrap_enabled() {
            return;
        }
        let width = self.visible_text_width();
        if width == 0 {
            return;
        }
        if self.cursor_col < self.scroll_col {
            self.scroll_col = self.cursor_col;
        } else {
            // Hint cells between the scroll origin and the caret consume
            // viewport width too. Scrolling right drops leading hints out of
            // the row, so re-measure until the caret fits (the loop is
            // bounded: `scroll_col` only grows, capped at `cursor_col`).
            self.sync_tab_cells();
            loop {
                let extra = self.inlay_cells_before_cursor(self.cursor_row, self.scroll_col);
                let row = RowCells::of_line(
                    self.lines.get(self.cursor_row).map_or("", String::as_str),
                    self.scroll_col,
                );
                let cells = row.text_before(self.cursor_col) + extra;
                if cells < width {
                    break;
                }
                // Scroll right by the cells the caret overflows by (a tab or
                // wide character is several), measured in columns.
                let overflow = cells + 1 - width;
                let mut target = self.scroll_col;
                while target < self.cursor_col && row.text_before(target) < overflow {
                    target += 1;
                }
                if target <= self.scroll_col {
                    break;
                }
                self.scroll_col = target.min(self.cursor_col);
            }
        }
    }

    /// One screen worth of rows, derived from the editor's last rendered
    /// inner height.  Falls back to a sensible default before the first
    /// render (when `last_inner.height` is still 0).
    pub fn page_size(&self) -> usize {
        let from_rows = self.text_rows();
        if from_rows > 0 { from_rows } else { 20 }
    }

    /// Move the viewport down by exactly one screen so the first
    /// previously-unseen row becomes the new top of the viewport, and place
    /// the cursor on that new top row.  Clamps at end of file.
    pub fn page_down_one_screen(&mut self) {
        let page = self.page_size();
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top + page);
            return;
        }
        let max_row = self.lines.len().saturating_sub(1);
        let new_top = (self.scroll + page).min(max_row);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Move the viewport up by exactly one screen so the new top is `page`
    /// rows above the previous top.  Cursor lands on the new top row.
    /// Clamps at the start of file.
    pub fn page_up_one_screen(&mut self) {
        let page = self.page_size();
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_sub(page));
            return;
        }
        let new_top = self.scroll.saturating_sub(page);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Start of the logical line (`Ctrl+A`), before any indentation.
    pub fn home_line(&mut self) {
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    /// End of the logical line (`Ctrl+E`, and End's second step).
    pub fn end_line(&mut self) {
        self.cursor_col = self.line_char_len(self.cursor_row);
        self.last_edit_kind = None;
    }

    /// The Home key, VS Code's `cursorHome`: to the line's first
    /// non-whitespace character, or to its start when the caret is already
    /// there, so a second press toggles (#987). It used to always go to the
    /// start, before the indentation. With wrap on (#1569) it first goes to
    /// the start of the wrapped row the caret is on. With
    /// `home_walks_rows`, a caret already on a continuation row's start goes
    /// to the start of the row above, one row per press, down to the line's
    /// own stops.
    pub fn home_key(&mut self) {
        let line = &self.lines[self.cursor_row];
        let first = line
            .chars()
            .position(|c| c != ' ' && c != '\t')
            .unwrap_or_else(|| line.chars().count());
        let smart = if self.cursor_col == first { 0 } else { first };
        self.cursor_col = match self.wrapped_row_of_cursor() {
            // A row can start on a combining mark (the wrap breaks after the
            // space it sits on), so step back to its cluster's start.
            Some((start, _)) => match floor_grapheme_col(line, start) {
                row_start if self.cursor_col > row_start && row_start > first => row_start,
                _ if self.home_walks_rows && start > 0 && self.cursor_col == start => {
                    self.wrapped_row_above(start).unwrap_or(smart)
                }
                _ => smart,
            },
            None => smart,
        };
        self.last_edit_kind = None;
    }

    /// The End key. With wrap on (#1569) it first goes to the end of the
    /// wrapped row the caret is on, before the character the row breaks at
    /// (the space, usually), since the column after it is drawn on the next
    /// row; from there, to the line's end.
    pub fn end_key(&mut self) {
        let len = self.line_char_len(self.cursor_row);
        self.cursor_col = match self.wrapped_row_of_cursor() {
            Some((_, end)) if end < len => {
                let last = floor_grapheme_col(&self.lines[self.cursor_row], end - 1);
                if self.cursor_col < last { last } else { len }
            }
            _ => len,
        };
        self.last_edit_kind = None;
    }

    pub fn scroll_up(&mut self, n: usize) {
        // A rendered log scrolls by file line, ahead of the wrap/comment-box
        // paths that assume an editable buffer (#257).
        if self.log.is_some() {
            self.scroll_view_to(self.scroll.saturating_sub(n));
            return;
        }
        if self.scroll_markdown_preview(-(n as i32)) {
            return;
        }
        if self.scroll_sheet_rows(-(n as isize)) {
            return;
        }
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_sub(n));
        } else if self.comment_boxes.is_empty() {
            self.scroll_view_to(self.scroll.saturating_sub(n));
        } else {
            self.nonwrap_set_top(self.nonwrap_top_content_row().saturating_sub(n));
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        if self.log.is_some() {
            self.scroll_view_to(self.scroll.saturating_add(n));
            return;
        }
        if self.scroll_markdown_preview(n as i32) {
            return;
        }
        if self.scroll_sheet_rows(n as isize) {
            return;
        }
        if self.wrap_enabled() {
            let top = self.top_visual_row(self.visible_text_width());
            self.wrap_set_top(top.saturating_add(n));
        } else if self.comment_boxes.is_empty() {
            self.scroll_view_to(self.scroll.saturating_add(n));
        } else {
            self.nonwrap_set_top(self.nonwrap_top_content_row().saturating_add(n));
        }
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let viewport = self.text_rows();
        if self.wrap_enabled() {
            let width = self.visible_text_width();
            let (base, total) = match self.clip_content_rows(width, true) {
                Some(clip) => clip,
                None => (0, self.total_visual_rows(width)),
            };
            let Some(metrics) = scrollbar::vertical_metrics(
                self.last_scrollbar,
                total,
                viewport,
                self.top_visual_row(width).saturating_sub(base),
            ) else {
                return false;
            };
            self.wrap_set_top(base + scrollbar::scroll_for_y(metrics, y));
            return true;
        }
        // Same content length the render sized the bar with: lines plus
        // comment-box rows. Mapping through bare `lines.len()` made the bar
        // of a short file with a tall navigator comment dead (metrics said
        // "no overflow") and a long file's thumb run away from the pointer.
        let bw = self.visible_text_width();
        let (base, content) = self.clip_content_rows(bw, false).unwrap_or((
            0,
            self.lines.len() + self.box_rows_between(0, self.lines.len(), bw),
        ));
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            content,
            viewport,
            self.nonwrap_top_content_row().saturating_sub(base),
        ) else {
            return false;
        };
        self.nonwrap_set_top(base + scrollbar::scroll_for_y(metrics, y));
        true
    }

    /// Map a click/drag X coordinate on the horizontal scrollbar onto
    /// `scroll_col`. Returns false when there is no horizontal bar (no
    /// overflow), so the caller can fall through to other hit-tests.
    pub fn scroll_to_bar_x(&mut self, x: u16) -> bool {
        let width = self.visible_text_width();
        let Some(metrics) = scrollbar::horizontal_metrics(
            self.last_hscrollbar,
            self.content_cols(width),
            width,
            self.scroll_col,
        ) else {
            return false;
        };
        self.scroll_col = scrollbar::scroll_for_x(metrics, x);
        true
    }

    /// Rows used for text in the last render (inner height minus the
    /// horizontal scrollbar row when present). Falls back to the full inner
    /// height before the first render populates `last_text_rows`.
    pub(crate) fn text_rows(&self) -> usize {
        let rows = self.last_text_rows as usize;
        if rows > 0 {
            rows
        } else {
            self.last_inner.height as usize
        }
    }

    /// Number of text rows currently visible (the minimap viewport box height).
    pub fn visible_rows(&self) -> usize {
        self.text_rows()
    }

    /// Inclusive `(start_row, end_row)` of the active selection, or `None` when
    /// there's no selection with area. Drives the minimap selection band.
    pub fn selection_rows(&self) -> Option<(usize, usize)> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let (start, end) = sel.normalised();
        Some((start.0, end.0))
    }

    /// The lines the minimap draws, as `(first, end)` with `end` exclusive:
    /// a symbol tab's symbol (#369), else the whole buffer.
    pub fn minimap_span(&self) -> (usize, usize) {
        self.symbol_clip().unwrap_or((0, self.lines.len()))
    }

    /// Move the cursor to `line` and center the viewport on it (minimap click /
    /// drag navigation). The cursor moves too because the render's scroll-follow
    /// snaps the viewport back to keep the caret visible; centering the caret
    /// here means that follow logic leaves the chosen scroll position alone.
    pub fn goto_line_centered(&mut self, line: usize) {
        if self.lines.is_empty() {
            return;
        }
        let line = line.min(self.lines.len() - 1);
        self.cursor_row = line;
        self.cursor_col = self.cursor_col.min(self.line_char_len(line));
        let viewport = self.text_rows();
        if viewport == 0 {
            // Not rendered yet (a file opened for the first time has no
            // geometry), so there is no middle to find: leave a few lines of
            // context above the target and let the first render follow.
            self.scroll = line.saturating_sub(5);
        } else {
            self.scroll = line
                .saturating_sub(viewport / 2)
                .min(self.lines.len().saturating_sub(viewport));
        }
        self.last_edit_kind = None;
    }

    /// Rasterize the whole document into a `w`x`h` RGBA buffer for the minimap:
    /// one column per character (clipped to `w`), each non-whitespace char
    /// painted in its syntax color. Whitespace stays `bg`, so indentation reads
    /// as the file's shape, like VS Code's minimap. The lines are mapped into
    /// `content_h` rows (top-aligned) rather than the full `h`: a short file
    /// uses a fixed small per-line height and leaves the rest of the strip
    /// blank, instead of stretching six lines down the whole column. For a file
    /// long enough to fill the strip, `content_h == h` and the mapping is the
    /// plain whole-file fit.
    /// ponytail: 1px per char, clipped at strip width; sub-pixel char scaling
    /// only if a wider strip ever needs to show more columns.
    pub fn minimap_rgba(
        &self,
        w: u32,
        h: u32,
        content_h: u32,
        bg: (u8, u8, u8),
        fg: (u8, u8, u8),
    ) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px[0] = bg.0;
            px[1] = bg.1;
            px[2] = bg.2;
            px[3] = 0xff;
        }
        let (first, end) = self.minimap_span();
        let total = (end - first).max(1) as u64;
        for (i, line) in self.lines[first..end].iter().enumerate() {
            let y0 = (i as u64 * content_h as u64 / total) as u32;
            if y0 >= h {
                break;
            }
            let y1 = (((i as u64 + 1) * content_h as u64 / total) as u32).clamp(y0 + 1, h);
            let merged = merge_overlay(
                self.highlights
                    .get(first + i)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                self.semantic_overlay
                    .get(first + i)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            let mut si = 0usize;
            for (col, (byte, ch)) in (0_u32..).zip(line.char_indices()) {
                if col >= w {
                    break;
                }
                while si < merged.len() && merged[si].range().end <= byte {
                    si += 1;
                }
                if !ch.is_whitespace() {
                    let (r, g, b) = if si < merged.len() && merged[si].range().start <= byte {
                        span_rgb(merged[si].style, fg)
                    } else {
                        fg
                    };
                    for y in y0..y1 {
                        let idx = ((y * w + col) * 4) as usize;
                        buf[idx] = r;
                        buf[idx + 1] = g;
                        buf[idx + 2] = b;
                    }
                }
            }
        }
        buf
    }

    /// The non-wrap viewport top in CONTENT rows: buffer lines plus the
    /// comment-box rows above (and inside, via `scroll_sub`) the top line.
    pub(super) fn nonwrap_top_content_row(&self) -> usize {
        let bw = self.visible_text_width();
        self.scroll + self.box_rows_between(0, self.scroll, bw) + self.scroll_sub
    }

    /// Set the non-wrap viewport top to content row `row`, landing mid-box
    /// when it falls inside a comment box (`scroll_sub` counts rows past
    /// the top line's own text row). Mirrors `wrap_set_top` for the
    /// box-extended non-wrap layout; a tall box is unreadable and its
    /// Reply/Ignore footer unreachable with a line-granular top. Pulls the
    /// cursor along like `scroll_view_to`.
    pub(super) fn nonwrap_set_top(&mut self, row: usize) {
        let viewport = self.text_rows();
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.scroll_sub = 0;
            return;
        }
        if self.comment_boxes.is_empty() {
            self.scroll_sub = 0;
            self.scroll_view_to(row);
            return;
        }
        let bw = self.visible_text_width();
        let total = self.lines.len() + self.box_rows_between(0, self.lines.len(), bw);
        let row = row.min(total.saturating_sub(viewport));
        let mut acc = 0usize;
        let mut line = 0usize;
        while line < self.lines.len() {
            let group = 1 + self.box_rows_between(line, line + 1, bw);
            if acc + group > row {
                break;
            }
            acc += group;
            line += 1;
        }
        self.scroll = line.min(self.lines.len().saturating_sub(1));
        self.scroll_sub = row.saturating_sub(acc);
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        let floor = self.caret_floor_row().min(last_visible);
        if self.cursor_row < floor {
            self.cursor_row = floor;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    pub(super) fn scroll_view_to(&mut self, top: usize) {
        let viewport = self.last_inner.height as usize;
        // A rendered log's text side is a one-line stub, so clamping against
        // `lines` would pin scroll at 0 and the view could never move past its
        // first screen (#257). Clamp against the log's own line count instead;
        // its body area is one row shorter than the viewport (the header).
        if let Some(log) = self.log.as_ref() {
            let rows = viewport.saturating_sub(1).max(1);
            self.scroll = top.min(log.len().saturating_sub(rows));
            // Any scroll is the reader's own place now, which a reload still
            // settling must not take back.
            self.log_reload_top = None;
            return;
        }
        if viewport == 0 || self.lines.is_empty() {
            self.scroll = 0;
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.last_edit_kind = None;
            return;
        }
        self.scroll = top.min(self.lines.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.lines.len().saturating_sub(1));
        let floor = self.caret_floor_row().min(last_visible);
        if self.cursor_row < floor {
            self.cursor_row = floor;
        } else if self.cursor_row > last_visible {
            self.cursor_row = last_visible;
        }
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Topmost line the caret may be dragged to when the viewport scrolls out
    /// from under it: the first row the sticky band does not cover. Parking it
    /// on `scroll` itself puts it under the pinned headers, where it cannot be
    /// seen and where the band would have to yield to stay honest — which
    /// deleted the band for the whole of a wheel scroll.
    ///
    /// The band is measured from the previous frame's `sticky_lines` (the app
    /// layer rebuilds them from `scroll` on the next render). A burst of wheel
    /// events can therefore be one header out mid-gesture, which the render's
    /// own caret guard absorbs by trimming the band for that frame.
    pub(super) fn caret_floor_row(&self) -> usize {
        if self.wrap_enabled() || self.sticky_lines.is_empty() {
            return self.scroll;
        }
        let rows_avail = (self.last_inner.height as usize).saturating_sub(1);
        self.scroll + self.sticky_lines.len().min(rows_avail)
    }
}

/// `(char col, byte, char)` for `line`'s chars before char column `end`
/// (all of them when `None`), the last first: one pass for a backward
/// bracket scan, however long the line (#959).
pub(super) fn chars_back(
    line: &str,
    end: Option<usize>,
) -> impl Iterator<Item = (usize, usize, char)> + '_ {
    let head = &line[..end.map_or(line.len(), |end| char_byte(line, end))];
    let count = head.chars().count();
    head.char_indices()
        .rev()
        .zip((0..count).rev())
        .map(|((byte, ch), col)| (col, byte, ch))
}

/// Convert a char index within `s` to a byte index, saturating at `s.len()`.
pub(super) fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Char column of the extended grapheme cluster boundary before `col` on
/// `line`, so one step never stops inside a ZWJ emoji, a flag or a letter with
/// its combining accent (#1196). Columns stay char-indexed: every cluster
/// boundary is a char boundary.
pub(super) fn prev_grapheme_col(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        return col.saturating_sub(1);
    }
    let mut start = 0;
    let mut prev = 0;
    for g in line.graphemes(true) {
        if start >= col {
            break;
        }
        prev = start;
        start += g.chars().count();
    }
    prev
}

/// Char column of the extended grapheme cluster boundary after `col` on
/// `line`, saturating at the line's end (#1196).
pub(super) fn next_grapheme_col(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        return (col + 1).min(line.len());
    }
    let mut end = 0;
    for g in line.graphemes(true) {
        end += g.chars().count();
        if end > col {
            return end;
        }
    }
    end
}

/// `col` moved back to the start of the grapheme cluster it falls inside, so
/// a vertical move or a click never leaves the caret mid-cluster (#1196).
pub(super) fn floor_grapheme_col(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        return col.min(line.len());
    }
    let mut start = 0;
    for g in line.graphemes(true) {
        let len = g.chars().count();
        if start + len > col {
            return start;
        }
        start += len;
    }
    start
}

pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `chars[i]` belongs to a word for the word motions and deletes
/// (#909): a word char, or a zero-width mark riding on one (a combining
/// accent in decomposed text, a variation selector), so `Ctrl+Right` never
/// stops between `e` and its accent for `Ctrl+Backspace` to split them.
pub(crate) fn word_char_at(chars: &[char], i: usize) -> bool {
    let joins = |c: char| !c.is_control() && unicode_width::UnicodeWidthChar::width(c) == Some(0);
    let base = (0..=i)
        .rev()
        .find(|&j| j == 0 || !joins(chars[j]))
        .unwrap_or(0);
    is_word_char(chars[base])
}

/// Where Delete Word Left stops on `line`, going back from char `col`: over
/// any non-word chars, then over the word before them (`move_word_left`'s
/// stop within a line), moved back to a grapheme boundary.
pub(super) fn word_start_before(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    while i > 0 && !word_char_at(&chars, i - 1) {
        i -= 1;
    }
    while i > 0 && word_char_at(&chars, i - 1) {
        i -= 1;
    }
    floor_grapheme_col(line, i)
}

/// Where Delete Word Right stops on `line`, going on from char `col`: over
/// any non-word chars, then to the end of the word after them
/// (`move_word_right`'s stop within a line), on to a grapheme boundary.
pub(super) fn word_end_after(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    while i < chars.len() && !word_char_at(&chars, i) {
        i += 1;
    }
    while i < chars.len() && word_char_at(&chars, i) {
        i += 1;
    }
    match floor_grapheme_col(line, i) {
        start if start < i => next_grapheme_col(line, start),
        _ => i,
    }
}
