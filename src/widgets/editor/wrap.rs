//! Soft wrap and comment boxes: the rows each line takes on screen, and drawing and
//! clicking the comment boxes shown between lines.

use super::*;

/// The navigator's identity color: its comment boxes and its caret (the
/// same orange its note ◆ wore). Fixed like the git decoration colors —
/// legible on every dark background.
pub(crate) const NAVIGATOR_ACCENT: Color = Color::Rgb(0xff, 0x9d, 0x2f);

/// Columns of the comment-box footer tail ` ✕ Ignore ╯`.
pub(super) const IGNORE_TAIL_COLS: usize = 11;

/// One comment box: the AI pair programmer's voice, anchored below a buffer
/// line. Rendered as an unnumbered block (title, body, reply field + Ignore
/// button); it never touches the buffer text and is never saved. The App
/// feeds these per tick from the pair host's note snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentBox {
    /// The note's stable id in the pilot's state (Ignore removes it, a
    /// reply appends to it).
    pub id: u64,
    /// 0-based buffer line the box hangs under.
    pub line: usize,
    /// The navigator's caret name, shown in the title row.
    pub author: String,
    /// Body text; replies append as further lines.
    pub body: String,
}

/// What a click inside a comment box landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentHit {
    /// The title or body area: focus the box.
    Body,
    /// The footer's reply field: focus and place the caret.
    Reply,
    /// The footer's ✕ Ignore button: dismiss the box.
    Ignore,
}

/// The reply draft in the focused box's footer field. While set, typing goes
/// here instead of the buffer (the App routes keys).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentFocus {
    /// Which box is focused (its [`CommentBox::id`]).
    pub id: u64,
    /// The draft text.
    pub reply: String,
    /// Caret position in chars within `reply`.
    pub cursor: usize,
}

/// One visual row of the painted layout: a buffer line's wrap segment
/// (`Text`), or one row of a comment box, which belongs to no buffer line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisRow {
    /// `line`'s chars `[start, end)` (the whole visible span when not
    /// wrapping).
    Text {
        line: usize,
        start: usize,
        end: usize,
    },
    /// Row `box_row` (0-based within the block) of `comment_boxes[box_idx]`.
    Box { box_idx: usize, box_row: usize },
    /// Line `index` (1-based; line 0 is the ghost text at the caret) of the
    /// live inline suggestion (#1625), shown under the caret's line so a
    /// multi-line suggestion is read before Tab takes it.
    Ghost { index: usize },
}

impl Editor {
    /// Soft-wrap mode: long lines fold onto multiple visual rows instead of
    /// scrolling horizontally. Matches VS Code, which ships word-wrap on by
    /// default for Markdown only (`"[markdown]": { "editor.wordWrap": "on" }`).
    /// `toggle_wrap` (Alt+Z) sets a per-buffer override that wins over the
    /// language default.
    pub fn wrap_enabled(&self) -> bool {
        self.wrap_override
            .unwrap_or(matches!(self.lang, Some(LangKind::Markdown)))
    }

    /// Widest line as a horizontal extent in char columns for a text column
    /// `width` cells wide (tabs are Tab Size cells, see below), cached until the
    /// buffer, the width or the Tab Size changes. Drives the horizontal
    /// scrollbar's content extent and the `scroll_col` clamp. `&mut self` so the lazy recompute can populate the cache.
    pub(super) fn content_cols(&mut self, width: usize) -> usize {
        use unicode_segmentation::UnicodeSegmentation;
        self.sync_tab_cells();
        let tab = char_cells('\t');
        if let Some((w, t, cols)) = self.hscroll_content_cols
            && w == width
            && t == tab
        {
            return cols;
        }
        let cols = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                // Spliced hint cells widen the line's display, so the
                // horizontal extent must include them or the tail of a long
                // hinted line could never scroll into view.
                let extra: usize = self
                    .inlay_spans
                    .get(i)
                    .map(|hs| hs.iter().map(|(_, label, _)| label.chars().count()).sum())
                    .unwrap_or(0);
                if !l.contains('\t') {
                    return l.chars().count() + extra;
                }
                // A tab is Tab Size cells, but `scroll_col` counts chars. The
                // extent is in the same char columns: how far the line must
                // scroll for its tail to fit `width` cells, plus `width`, so
                // that `extent - width` is exactly that scroll.
                // Tab stops count from the line's left edge, so the tail from
                // char `first` takes `col - start(first)` cells, which shrinks
                // as `first` grows: the scroll is the first grapheme cluster
                // starting at or past the `col + extra - width` cells that must
                // scroll off. A whole cluster, never a char inside one: the row
                // scrolled to `👍🏽`'s skin tone draws the tone on its own, two
                // cells wider than the tail measured here. Two passes and no
                // table per char, which on a huge one-line file was hundreds of
                // MB before the first frame.
                let (col, len) =
                    char_cells_from(l, 0).fold((0, 0), |(col, n), (_, w)| (col + w, n + 1));
                if col + extra <= width {
                    return len + extra;
                }
                let off = col + extra - width;
                let (mut at, mut first) = (0, 0);
                for cluster in l.graphemes(true) {
                    if at >= off {
                        return first + width;
                    }
                    at += cluster_cells_at(cluster, at);
                    first += cluster.chars().count();
                }
                len + width
            })
            .max()
            .unwrap_or(0);
        self.hscroll_content_cols = Some((width, tab, cols));
        cols
    }

    // -- Comment-box geometry ----------------------------------------------
    // Comment boxes are unnumbered blocks rendered between a buffer line and
    // the next. They add visual rows without owning buffer positions; every
    // helper here is O(#boxes) and short-circuits to zero when no box is
    // present, so the hot path pays nothing.

    /// The layout entry at visual index `i` when it is a buffer-text row
    /// (None = out of range or a comment-box row). The one funnel every
    /// visual-row inversion goes through.
    pub(super) fn text_row(&self, i: usize) -> Option<(usize, usize, usize)> {
        match self.last_wrap_rows.get(i)? {
            VisRow::Text { line, start, end } => Some((*line, *start, *end)),
            VisRow::Box { .. } | VisRow::Ghost { .. } => None,
        }
    }

    /// The wrapped display lines of a box body at `bw` columns (its inner
    /// text width). Bodies are short; this allocates only while boxes exist.
    pub(super) fn comment_body_lines(body: &str, bw: usize) -> Vec<String> {
        let bw = bw.max(8);
        let mut out = Vec::new();
        for line in body.split('\n') {
            let chars: Vec<char> = line.chars().collect();
            for &(s, e) in &wrap_segments(line, bw) {
                out.push(chars[s..e].iter().collect());
            }
        }
        if out.is_empty() {
            out.push(String::new());
        }
        out
    }

    /// Total rows `comment_boxes[idx]` occupies at text width `tw`:
    /// title + wrapped body + footer.
    pub(super) fn comment_box_height(&self, idx: usize, tw: usize) -> usize {
        let body = &self.comment_boxes[idx].body;
        2 + Self::comment_body_lines(body, tw.saturating_sub(4)).len()
    }

    /// Rows contributed by the boxes anchored under `line` at width `tw`.
    pub(super) fn box_rows_at_line(&self, line: usize, tw: usize) -> usize {
        if self.comment_boxes.is_empty() {
            return 0;
        }
        (0..self.comment_boxes.len())
            .filter(|&i| self.comment_boxes[i].line == line)
            .map(|i| self.comment_box_height(i, tw))
            .sum()
    }

    /// Rows contributed by boxes anchored under lines `[from, to)`.
    pub(super) fn box_rows_between(&self, from: usize, to: usize, tw: usize) -> usize {
        if self.comment_boxes.is_empty() {
            return 0;
        }
        (0..self.comment_boxes.len())
            .filter(|&i| {
                let l = self.comment_boxes[i].line;
                // A box on a line hidden inside a collapsed fold is never
                // painted (the layout skips hidden lines wholesale), so it
                // must not count toward any scroll geometry either - it
                // kept a scrollbar alive for invisible content and let the
                // wheel walk the viewport into blank space.
                l >= from && l < to && !self.is_line_hidden(l)
            })
            .map(|i| self.comment_box_height(i, tw))
            .sum()
    }

    /// Paint one row of a comment box: blank unnumbered gutter, then the
    /// block chrome. Rows: 0 = title (author), middle = body, last = footer
    /// (reply field left, ✕ Ignore right).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_comment_box_row(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        inner: Rect,
        y: u16,
        gutter_width: u16,
        text_x: u16,
        text_width: u16,
        box_idx: usize,
        box_row: usize,
    ) {
        // The whole gutter goes blank: no number, no glyphs, no git bar.
        buf.set_line(
            inner.x,
            y,
            &Line::from(" ".repeat(gutter_width as usize + 1)),
            gutter_width + 1,
        );
        let w = text_width as usize;
        if w < 8 {
            return;
        }
        let b = &self.comment_boxes[box_idx];
        let accent = Style::default()
            .fg(NAVIGATOR_ACCENT)
            .bg(self.theme.sticky_scroll_bg());
        let body_st = Style::default().bg(self.theme.sticky_scroll_bg());
        let dim = Style::default()
            .fg(Color::DarkGray)
            .bg(self.theme.sticky_scroll_bg());
        let height = self.comment_box_height(box_idx, w);
        let pad = |s: &str, n: usize| -> String {
            let mut out: String = s.chars().take(n).collect();
            while out.chars().count() < n {
                out.push(' ');
            }
            out
        };
        let line = if box_row == 0 {
            let head = format!("\u{256d}\u{2500} \u{25c6} {} ", b.author);
            let fill = w.saturating_sub(head.chars().count() + 1);
            Line::from(Span::styled(
                format!("{head}{}\u{256e}", "\u{2500}".repeat(fill)),
                accent,
            ))
        } else if box_row + 1 < height {
            let body_lines = Self::comment_body_lines(&b.body, w.saturating_sub(4));
            let text = body_lines.get(box_row - 1).cloned().unwrap_or_default();
            Line::from(vec![
                Span::styled("\u{2502} ", accent),
                Span::styled(pad(&text, w - 4), body_st),
                Span::styled(" \u{2502}", accent),
            ])
        } else {
            // Footer: `╰ ❯ <reply>            ✕ Ignore ╯`
            let focus = self.comment_focus.as_ref().filter(|f| f.id == b.id);
            let field_w = w.saturating_sub(4 + IGNORE_TAIL_COLS);
            // Window the draft around the caret: a reply longer than the
            // field otherwise froze at its first field_w chars and the
            // user typed blind, with no caret drawn anywhere.
            let win = focus.map_or(0, |f| f.cursor.saturating_sub(field_w.saturating_sub(1)));
            let (draft, style) = match focus {
                Some(f) => {
                    let visible: String = f.reply.chars().skip(win).take(field_w).collect();
                    (pad(&visible, field_w), body_st)
                }
                None => (pad("Reply", field_w), dim),
            };
            let mut spans = vec![
                Span::styled("\u{2570} ", accent),
                Span::styled("\u{276f} ", accent),
            ];
            match focus {
                Some(f) if field_w > 0 && f.cursor - win < field_w => {
                    // Split the draft around the caret cell so it shows.
                    let cursor = f.cursor - win;
                    let chars: Vec<char> = draft.chars().collect();
                    let before: String = chars[..cursor].iter().collect();
                    let at: String = chars[cursor..=cursor].iter().collect();
                    let after: String = chars[cursor + 1..].iter().collect();
                    spans.push(Span::styled(before, style));
                    spans.push(Span::styled(
                        at,
                        style.add_modifier(ratatui::style::Modifier::REVERSED),
                    ));
                    spans.push(Span::styled(after, style));
                }
                _ => spans.push(Span::styled(draft, style)),
            }
            spans.push(Span::styled(" \u{2715} Ignore ", accent));
            spans.push(Span::styled("\u{256f}", accent));
            Line::from(spans)
        };
        buf.set_line(text_x, y, &line, text_width);
        // Ensure the block's background reaches the right edge even when a
        // span fell short (padding already covers the normal case).
        for x in text_x..text_x + text_width {
            if buf[(x, y)].symbol() == " " && buf[(x, y)].style().bg.is_none() {
                buf[(x, y)].set_style(body_st);
            }
        }
    }

    /// What a click at screen `(col, row)` hits inside a comment box:
    /// Ignore on the footer's ✕ Ignore cells, Reply on the rest of the
    /// footer, Body anywhere else in the block. None = not a box row.
    pub fn comment_box_hit(&self, col: u16, row: u16) -> Option<(u64, CommentHit)> {
        if self.comment_boxes.is_empty() {
            return None;
        }
        let inner = self.last_inner;
        if row < inner.y || col < inner.x {
            return None;
        }
        let VisRow::Box { box_idx, box_row } =
            *self.last_wrap_rows.get((row - inner.y) as usize)?
        else {
            return None;
        };
        let b = &self.comment_boxes[box_idx];
        let text_x = inner.x + self.last_gutter_width + 1;
        let w = self.visible_text_width();
        if box_row + 1 == self.comment_box_height(box_idx, w) {
            // Footer: the ` ✕ Ignore ╯` tail owns its cells.
            let rel = (col.saturating_sub(text_x)) as usize;
            if rel + IGNORE_TAIL_COLS >= w && rel < w {
                return Some((b.id, CommentHit::Ignore));
            }
            return Some((b.id, CommentHit::Reply));
        }
        Some((b.id, CommentHit::Body))
    }

    // -- Soft-wrap visual-row geometry ------------------------------------
    // In wrap mode a logical line maps to one or more visual rows. These
    // helpers convert between logical positions and the flattened visual-row
    // space the viewport scrolls over. They are only called on the wrap path;
    // the non-wrap path keeps its cheap one-row-per-line arithmetic.

    /// Wrap segments of a single logical line at the given text width.
    pub(super) fn line_segments(&self, line: usize, width: usize) -> Vec<(usize, usize)> {
        wrap_segments(self.lines.get(line).map_or("", String::as_str), width)
    }

    /// Number of visual rows a logical line occupies. Avoids allocating the
    /// segment vector for the common case of a line that already fits.
    pub(super) fn line_visual_rows(&self, line: usize, width: usize) -> usize {
        // A line inside a collapsed fold is not drawn: no rows. Counted, it
        // put Down/Up onto the hidden line (which then unfolded) and threw
        // the scrollbar and wheel off by the hidden rows.
        if self.is_line_hidden(line) {
            return 0;
        }
        if width == 0 {
            return 1;
        }
        let len = self.line_char_len(line);
        if len <= width && self.lines.get(line).is_none_or(|l| l.is_ascii()) {
            1
        } else {
            self.line_segments(line, width).len()
        }
    }

    /// Visual rows of `line`'s whole group: its wrap segments plus the
    /// comment-box rows hanging under it. All flattened visual-row math
    /// (totals, viewport top, cursor row, decomposition) speaks group rows
    /// so boxes shift everything below them consistently.
    pub(super) fn group_visual_rows(&self, line: usize, width: usize) -> usize {
        if self.is_line_hidden(line) {
            return 0;
        }
        self.line_visual_rows(line, width) + self.box_rows_at_line(line, width)
    }

    /// Total visual rows across the whole buffer. The text-only total is
    /// cached per width; box rows are added fresh (boxes are few and can
    /// change between frames without touching the buffer).
    pub(super) fn total_visual_rows(&mut self, width: usize) -> usize {
        let text =
            if let Some(&(_, total)) = self.wrap_total_cache.iter().find(|&&(w, _)| w == width) {
                total
            } else {
                let total: usize = (0..self.lines.len())
                    .map(|l| self.line_visual_rows(l, width))
                    .sum();
                self.wrap_total_cache.push((width, total));
                total
            };
        text + self.box_rows_between(0, self.lines.len(), width)
    }

    /// Visual-row index of the current viewport top `(scroll, scroll_sub)`.
    pub(super) fn top_visual_row(&self, width: usize) -> usize {
        (0..self.scroll)
            .map(|l| self.group_visual_rows(l, width))
            .sum::<usize>()
            + self.scroll_sub
    }

    /// Visual-row index of the segment containing the cursor. A box under
    /// the cursor's own line renders below its segments, so only boxes on
    /// earlier lines shift the cursor.
    pub(super) fn cursor_visual_row(&self, width: usize) -> usize {
        let before: usize = (0..self.cursor_row)
            .map(|l| self.group_visual_rows(l, width))
            .sum();
        before + self.segment_index_of_col(self.cursor_row, self.cursor_col, width)
    }

    /// Which segment of `line` holds character column `col`. A column sitting
    /// exactly on a wrap boundary belongs to the *next* segment (its start),
    /// matching where `cursor_screen_pos` paints the caret; only the true line
    /// end maps to the final segment. Keeping this predicate identical to the
    /// one in `cursor_screen_pos` is what keeps vertical motion consistent with
    /// where the caret actually renders.
    pub(super) fn segment_index_of_col(&self, line: usize, col: usize, width: usize) -> usize {
        let segs = self.line_segments(line, width);
        let line_len = self.line_char_len(line);
        segs.iter()
            .position(|&(s, e)| col >= s && (col < e || (col == e && e == line_len)))
            .unwrap_or(segs.len().saturating_sub(1))
    }

    /// Point the viewport top at a global visual-row index, decomposing it back
    /// into `(scroll, scroll_sub)`. Parks at the last segment when `target`
    /// runs past the end of the buffer.
    pub(super) fn set_top_to_visual_row(&mut self, target: usize, width: usize) {
        let mut acc = 0;
        for line in 0..self.lines.len() {
            let n = self.group_visual_rows(line, width);
            if acc + n > target {
                self.scroll = line;
                // scroll_sub may point into the line's box region (past its
                // segments); the layout builder starts mid-box then.
                self.scroll_sub = target - acc;
                return;
            }
            acc += n;
        }
        self.scroll = self.lines.len().saturating_sub(1);
        self.scroll_sub = self.group_visual_rows(self.scroll, width).saturating_sub(1);
    }

    /// Logical `(line, char_col)` at the start of a global visual row -
    /// the inverse of `cursor_visual_row` for the row's first column. A
    /// target inside a line's comment-box region snaps to that line's last
    /// segment (a box row has no buffer position of its own).
    pub(super) fn logical_pos_at_visual_row(&self, target: usize, width: usize) -> (usize, usize) {
        let mut acc = 0;
        for line in 0..self.lines.len() {
            if self.is_line_hidden(line) {
                continue;
            }
            let segs = self.line_segments(line, width);
            let group = segs.len() + self.box_rows_at_line(line, width);
            if acc + group > target {
                let idx = (target - acc).min(segs.len().saturating_sub(1));
                return (line, segs[idx].0);
            }
            acc += group;
        }
        (self.lines.len().saturating_sub(1), 0)
    }

    /// True when global visual row `target` falls inside a comment-box
    /// region (no buffer position). Cursor motion steps over these.
    pub(super) fn visual_row_is_box(&self, target: usize, width: usize) -> bool {
        if self.comment_boxes.is_empty() {
            return false;
        }
        let mut acc = 0;
        for line in 0..self.lines.len() {
            let segs = self.line_visual_rows(line, width);
            let group = self.group_visual_rows(line, width);
            if acc + group > target {
                return target - acc >= segs;
            }
            acc += group;
        }
        false
    }

    /// Move the wrap viewport so the given visual row is at the top, clamped to
    /// the document, then pull the cursor into the new viewport so the render's
    /// cursor-visibility clamp doesn't immediately undo the scroll. Used by the
    /// wheel and the vertical scrollbar in wrap mode.
    pub(super) fn wrap_set_top(&mut self, top_target: usize) {
        let width = self.visible_text_width();
        let viewport = self.text_rows();
        if width == 0 || viewport == 0 {
            return;
        }
        let total = self.total_visual_rows(width);
        let new_top = top_target.min(total.saturating_sub(viewport));
        self.set_top_to_visual_row(new_top, width);
        let cursor_vrow = self.cursor_visual_row(width);
        let target_row = if cursor_vrow < new_top {
            Some(new_top)
        } else if cursor_vrow >= new_top + viewport {
            Some(new_top + viewport - 1)
        } else {
            None
        };
        if let Some(vrow) = target_row {
            let (line, col) = self.logical_pos_at_visual_row(vrow, width);
            self.cursor_row = line;
            self.cursor_col = col.min(self.line_char_len(line));
        }
        self.last_edit_kind = None;
    }

    /// Move the cursor one visual row up (`dir = -1`) or down (`dir = 1`) in
    /// wrap mode, preserving the visual column within the row. Unlike the
    /// logical `move_up`/`move_down`, this steps through wrapped segments of a
    /// paragraph rather than skipping the whole logical line.
    pub(super) fn wrap_move_vertical(&mut self, dir: isize) {
        let width = self.visible_text_width().max(1);
        let segs = self.line_segments(self.cursor_row, width);
        let seg_idx = self.segment_index_of_col(self.cursor_row, self.cursor_col, width);
        let goal_col = self.vertical_goal_from(self.cursor_col - segs[seg_idx].0);
        let current = self.cursor_visual_row(width) as isize;
        let total = self.total_visual_rows(width) as isize;
        let mut target = current + dir;
        // Step over comment-box rows: the caret never enters a box.
        while target >= 0 && target < total && self.visual_row_is_box(target as usize, width) {
            target += dir;
        }
        if target < 0 || target >= total {
            return;
        }
        let (line, seg_start) = self.logical_pos_at_visual_row(target as usize, width);
        let (s, e) = self
            .line_segments(line, width)
            .into_iter()
            .find(|&(s, _)| s == seg_start)
            .unwrap_or((seg_start, seg_start));
        self.cursor_row = line;
        self.cursor_col = floor_grapheme_col(&self.lines[line], (s + goal_col).min(e));
        self.vertical_goal = Some((self.cursor_row, self.cursor_col, goal_col));
    }

    /// VS Code "View: Toggle Word Wrap" (Alt+Z): flip this buffer's soft-wrap
    /// state, overriding the language default until the file is reopened.
    pub fn toggle_wrap(&mut self) {
        let now = self.wrap_enabled();
        self.wrap_override = Some(!now);
    }

    /// Start of the wrapped row above the one that starts at `row_start`
    /// (a char column of the caret's line), on a grapheme boundary. `None`
    /// on the line's top row.
    pub(super) fn wrapped_row_above(&self, row_start: usize) -> Option<usize> {
        let width = self.visible_text_width().max(1);
        let segs = self.line_segments(self.cursor_row, width);
        let idx = segs.iter().position(|&(s, _)| s == row_start)?;
        let above = segs.get(idx.checked_sub(1)?)?.0;
        Some(floor_grapheme_col(&self.lines[self.cursor_row], above))
    }

    /// The `(start, end)` columns of the wrapped row holding the caret, or
    /// `None` with wrap off.
    pub(super) fn wrapped_row_of_cursor(&self) -> Option<(usize, usize)> {
        if !self.wrap_enabled() {
            return None;
        }
        let width = self.visible_text_width().max(1);
        let segs = self.line_segments(self.cursor_row, width);
        let idx = self.segment_index_of_col(self.cursor_row, self.cursor_col, width);
        segs.get(idx).copied()
    }
}

/// A character VS Code breaks a wrapped line *after* (a subset of its default
/// `editor.wordWrapBreakAfterCharacters` that matters for Latin/Markdown
/// prose): whitespace plus trailing punctuation, so the next word starts a
/// fresh visual row.
pub(super) fn is_wrap_break_after(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ')' | ']' | '}' | '-' | '/' | ',' | '.' | ';' | ':' | '?' | '!'
        )
}

/// Soft-wrap a logical line into visual segments for word-wrap mode.
///
/// Each segment is a half-open `(start_char, end_char)` range over the chars
/// of `line`; the segments tile the line with no gaps or overlaps, so the
/// inverse maps (cursor -> screen, click -> cursor) stay exact. Breaks after
/// the last break-after character at or before `width` when one exists (word
/// wrap), and hard-breaks a single token longer than `width` at the column. A
/// line that already fits, an empty line, or `width == 0` yields one segment
/// so the line still occupies a row. Mirrors VS Code's monospace wrap: pick
/// the rightmost break opportunity within the column, else split the
/// over-long token.
pub(super) fn wrap_segments(line: &str, width: usize) -> Vec<(usize, usize)> {
    if width == 0 {
        return vec![(0, line.chars().count())];
    }
    let mut segs = Vec::new();
    let (mut start, mut start_byte) = (0, 0);
    loop {
        // A wrapped row counts tab stops from its own left edge, so each row
        // is measured as text of its own ([`char_cells_from`]). One pass per
        // row and nothing kept per char: a huge one-line file wraps without
        // a table the size of the file.
        let row = &line[start_byte..];
        let (mut end, mut end_byte, mut used) = (start, start_byte, 0);
        // The rightmost break-after char so far, as the `(char, byte)` just
        // past it; never right at the row's start, so a row is never one
        // char by a break (or stalls).
        let mut brk = None;
        let mut fits = true;
        // The hard limit counts CELLS, as the renderer clips: counted in
        // chars, a row of wide (CJK) characters was twice as wide as the
        // pane and its second half was never shown on any row. At least one
        // char per row, so a char wider than the pane cannot stall.
        for ((at, c), (_, w)) in row.char_indices().zip(char_cells_from(row, 0)) {
            if used + w > width && end > start {
                fits = false;
                break;
            }
            used += w;
            end += 1;
            end_byte = start_byte + at + c.len_utf8();
            if end > start + 1 && is_wrap_break_after(c) {
                brk = Some((end, end_byte));
            }
        }
        if fits {
            segs.push((start, end));
            return segs;
        }
        (end, end_byte) = brk.unwrap_or((end, end_byte));
        segs.push((start, end));
        (start, start_byte) = (end, end_byte);
    }
}
