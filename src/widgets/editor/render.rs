//! Drawing the editor: each row's text, colours and overlays, the caret, selections and
//! search matches, diagnostics, and how wide each character is drawn.

use super::*;

pub(super) fn write_cell(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, style: Style) {
    use unicode_segmentation::UnicodeSegmentation;
    // Fill the slot in screen columns, not chars (#1138): a CJK or emoji
    // glyph is two columns, so `w` chars drew up to twice the slot and ran
    // into the next column. Blank it first; then a wide glyph that would
    // straddle the edge is left out and its column stays blank. Overflow is
    // cut plainly with no trailing ellipsis marker (the user wants no
    // ellipsis anywhere in croft).
    let width = w as usize;
    buf.set_stringn(x, y, " ".repeat(width), width, style);
    let mut col = 0;
    for g in text.graphemes(true) {
        let gw = crate::sheet::grapheme_width(g);
        if gw == 0 {
            continue;
        }
        if col + gw > width {
            break;
        }
        if g.contains(char::is_control) {
            let shown: String = g.chars().map(visible_control).collect();
            buf.set_stringn(x + col as u16, y, &shown, gw, style);
        } else {
            buf.set_stringn(x + col as u16, y, g, gw, style);
        }
        col += gw;
    }
}

/// A control character as a glyph a cell can show (#1002): the buffer drops
/// control characters, so `line1\nline2\ttabbed` read `line1line2tabbed`.
/// One glyph per character, so a cell's width and an edit's caret still count
/// characters: `⏎` for a newline, `→` for a tab, a control picture (`␍`,
/// `␀`, `␡`) for the rest.
pub(super) fn visible_control(c: char) -> char {
    match c {
        '\n' => '\u{23ce}',
        '\t' => '\u{2192}',
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'),
        '\u{7f}' => '\u{2421}',
        c if c.is_control() => '\u{fffd}',
        c => c,
    }
}

impl Editor {
    /// Fill the whole-buffer caches a first paint would otherwise compute
    /// (fold tables, conflict markers, the widest line), so a view built on
    /// another thread (#371) costs the UI thread only its visible rows.
    pub(super) fn warm_render_caches(&mut self) {
        self.refresh_fold_tables();
        self.conflicts();
    }

    /// The display cell a row showing `line` from char `start` begins at, for
    /// tab stops: a wrapped row counts from its own left edge, a flat one
    /// from the line's (the cells scrolled past).
    pub(super) fn row_origin(&self, line: &str, start: usize) -> usize {
        if self.wrap_enabled() {
            0
        } else {
            cells_before(line, start)
        }
    }

    /// The caret's column as the status bar shows it, counted in display
    /// cells from 1: a tab moves it to the next tab stop, as in VS Code and
    /// vim, so it agrees with where the caret is drawn.
    pub fn cursor_visual_col(&self) -> usize {
        self.sync_tab_cells();
        // Only a tab is more than one column; any other character, a wide
        // one or each part of `👍🏽` included, counts one, exactly as VS
        // Code's status bar counts (`CursorColumns.toStatusbarColumn`).
        self.lines
            .get(self.cursor_row)
            .map_or(self.cursor_col, |l| {
                l.chars().take(self.cursor_col).fold(0, |col, c| {
                    col + if c == '\t' { tab_cells_at(col) } else { 1 }
                })
            })
            + 1
    }

    /// Make `\t` measure and paint as this buffer's Tab Size from here on.
    pub(crate) fn sync_tab_cells(&self) {
        set_tab_cells(self.indent_style().width as usize);
    }
}

pub(super) fn set_tab_cells(n: usize) {
    TAB_CELLS.with(|t| t.set(n.max(1)));
}

/// Screen cells a char takes in the editor: its display width, with a tab
/// drawn as the buffer's Tab Size blank cells.
pub(super) fn char_cells(c: char) -> usize {
    if c == '\t' {
        TAB_CELLS.with(std::cell::Cell::get)
    } else {
        unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
    }
}

/// Cells a tab takes when it starts at display cell `col` of its row: up to
/// the next multiple of Tab Size, as in vim and VS Code, so text after a tab
/// lines up whatever came before it.
pub(super) fn tab_cells_at(col: usize) -> usize {
    let t = TAB_CELLS.with(std::cell::Cell::get).max(1);
    t - col % t
}

/// Cells `c` takes when it starts at display cell `col`.
pub(super) fn char_cells_at(c: char, col: usize) -> usize {
    if c == '\t' {
        tab_cells_at(col)
    } else {
        char_cells(c)
    }
}

/// Cells the grapheme cluster `cluster` takes when it starts at display cell
/// `col`. ratatui paints a cluster as one glyph sized by the whole cluster
/// (`crate::cell_map`): `👍🏽` is two cells, not four, and `❤️` two, not one.
/// A lone character keeps [`char_cells_at`], so a tab runs to the next stop
/// (a tab is always a cluster of its own).
pub(super) fn cluster_cells_at(cluster: &str, col: usize) -> usize {
    let mut chars = cluster.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => char_cells_at(c, col),
        (Some(_), Some(_)) => unicode_width::UnicodeWidthStr::width(cluster),
        (None, _) => 0,
    }
}

/// Each character of `text` with the cells it takes when `text` starts at
/// display cell `origin` of its row: a cluster's first character takes the
/// whole cluster's cells ([`cluster_cells_at`]) and the rest none. Everything
/// that measures text for tab stops goes through here, so the drawing, the
/// caret and clicks agree on where a tab after an emoji ends.
pub(super) fn char_cells_from(
    text: &str,
    origin: usize,
) -> impl Iterator<Item = (char, usize)> + '_ {
    use unicode_segmentation::UnicodeSegmentation;
    let mut col = origin;
    text.graphemes(true).flat_map(move |cluster| {
        let w = cluster_cells_at(cluster, col);
        col += w;
        cluster
            .chars()
            .enumerate()
            .map(move |(i, c)| (c, if i == 0 { w } else { 0 }))
    })
}

/// Cells `text` takes from display cell `origin` of its row on.
pub(super) fn cells_from(text: &str, origin: usize) -> usize {
    char_cells_from(text, origin).map(|(_, w)| w).sum()
}

/// Display cells of the first `chars` characters of `line`, from the line's
/// left edge: where its character number `chars` starts.
pub(super) fn cells_before(line: &str, chars: usize) -> usize {
    cells_from(&line[..byte_index_of_char(line, chars)], 0)
}

/// `text` as the editor paints it: each tab as the blank cells it takes
/// ([`char_cells`]), which the caret, selection, clicks and every overlay
/// count for it. ratatui drops control characters, so a raw tab took no
/// cell and the text after it slid left under all of them (#891).
pub(super) fn blank_tabs(text: &str) -> std::borrow::Cow<'_, str> {
    blank_tabs_from(text, 0)
}

/// [`blank_tabs`] for text that starts at display cell `origin` of its row,
/// so each tab fills up to the next tab stop.
pub(super) fn blank_tabs_from(text: &str, origin: usize) -> std::borrow::Cow<'_, str> {
    if !text.contains('\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for (c, w) in char_cells_from(text, origin) {
        if c == '\t' {
            out.extend(std::iter::repeat_n(' ', w));
        } else {
            out.push(c);
        }
    }
    out.into()
}

pub(super) fn shift_spans_for_view(spans: &[HiSpan], byte_start: usize) -> Vec<HiSpan> {
    let mut out = Vec::with_capacity(spans.len());
    for sp in spans {
        let range = sp.range();
        if range.end <= byte_start {
            continue;
        }
        out.push(HiSpan::new(
            range.start.saturating_sub(byte_start),
            range.end - byte_start,
            sp.style,
        ));
    }
    out
}

/// Overlay `over` onto `base` (both sorted, non-overlapping, byte offsets
/// within one line) and return a sorted, non-overlapping span list. Where
/// the two cover the same bytes, `over` wins; `base` shows through in the
/// gaps. This is the per-line realization of the VS Code / Zed "combined"
/// rule: semantic tokens repaint the bytes they resolve, tree-sitter
/// syntax fills the rest.
/// Resolve a span's foreground to RGB for the minimap; non-RGB colors
/// (named/indexed/reset) fall back to the editor's default text color.
pub(super) fn span_rgb(style: ratatui::style::Style, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match style.fg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => (r, g, b),
        _ => default,
    }
}

pub(super) fn merge_overlay(base: &[HiSpan], over: &[HiSpan]) -> Vec<HiSpan> {
    if over.is_empty() {
        return base.to_vec();
    }
    let mut out: Vec<HiSpan> = Vec::with_capacity(base.len() + over.len());
    for b in base {
        // Emit the parts of this base span not covered by any overlay span.
        let mut cur = b.start;
        for o in over.iter().filter(|o| o.end > b.start && o.start < b.end) {
            if o.start > cur {
                out.push(HiSpan {
                    start: cur,
                    end: o.start,
                    style: b.style,
                });
            }
            cur = cur.max(o.end);
            if cur >= b.end {
                break;
            }
        }
        if cur < b.end {
            out.push(HiSpan {
                start: cur,
                end: b.end,
                style: b.style,
            });
        }
    }
    // The overlay normally wins outright, except over a comment tag (`TODO`,
    // `FIXME`, ...), which the tag pass deliberately marked so it stands out.
    // A server that reports the whole comment as one `comment` token would
    // otherwise repaint the tag in ordinary comment grey, so tag highlighting
    // would silently stop the moment an LSP attached. Carve those ranges out
    // of each overlay span instead. Keywords are BOLD as well, so the tag
    // test is the tag hue, not BOLD alone.
    let keep: Vec<&HiSpan> = base
        .iter()
        .filter(|b| crate::highlight::is_comment_tag_style(b.style))
        .collect();
    for o in over {
        let mut cur = o.start;
        for k in keep.iter().filter(|k| k.end > o.start && k.start < o.end) {
            if k.start > cur {
                out.push(HiSpan {
                    start: cur,
                    end: k.start,
                    style: o.style,
                });
            }
            out.push(HiSpan {
                start: k.start.max(o.start),
                end: k.end.min(o.end),
                style: k.style,
            });
            cur = cur.max(k.end);
            if cur >= o.end {
                break;
            }
        }
        if cur < o.end {
            out.push(HiSpan {
                start: cur,
                end: o.end,
                style: o.style,
            });
        }
    }
    out.sort_by_key(|s| s.start);
    out
}

/// `origin` is the display cell `line` starts at in its row (tab stops).
pub(super) fn build_line_spans<'a>(
    line: &'a str,
    spans: &[HiSpan],
    origin: usize,
) -> Vec<Span<'a>> {
    if spans.is_empty() {
        return vec![Span::raw(blank_tabs_from(line, origin))];
    }
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() * 2);
    let mut cursor = 0usize;
    // Display cells painted so far, from the row's left edge.
    let mut col = origin;
    let mut push = |out: &mut Vec<Span<'a>>, text: &'a str, style: Option<Style>| {
        let shown = blank_tabs_from(text, col);
        col += cells_from(text, col);
        out.push(match style {
            Some(st) => Span::styled(shown, st),
            None => Span::raw(shown),
        });
    };
    for sp in spans {
        let range = sp.range();
        if range.start > cursor && range.start <= line.len() {
            let slice = &line[cursor..range.start];
            if !slice.is_empty() {
                push(&mut out, slice, None);
            }
        }
        let s = range.start.min(line.len());
        let e = range.end.min(line.len());
        if e > s {
            push(&mut out, &line[s..e], Some(sp.style));
            cursor = e;
        }
    }
    if cursor < line.len() {
        push(&mut out, &line[cursor..], None);
    }
    out
}

/// What every visual row shares in one frame of [`Editor::render`]: the
/// text area and where the gutter and the text sit in it, and the
/// frame-wide facts the row painters read.
struct RowFrame {
    /// The editor's text area: the gutter starts at its left edge.
    inner: Rect,
    /// The line-number column with its sign margin, before the change lane.
    gutter_width: u16,
    /// The screen column the text starts at, right of the gutter.
    text_x: u16,
    /// The text's width right of the gutter.
    text_width: u16,
    /// Lines wrap: a long line takes several visual rows.
    wrap: bool,
    /// The primary selection, start before end, when it covers any text.
    sel_norm: Option<((usize, usize), (usize, usize))>,
    /// Columns between indentation guides.
    guide_step: usize,
    /// The caret block's guide as (column, first line, last line), when
    /// the focused editor shows guides.
    active_guide: Option<(usize, usize, usize)>,
    /// The guides' colour.
    guide_fg: Color,
    /// The active guide's colour.
    guide_active_fg: Color,
    /// The theme draws `surfaces`: a quieter gutter.
    surfaces: bool,
    /// The pointer is over the gutter, which shows every fold chevron.
    gutter_hovered: bool,
}

impl Widget for &mut Editor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.clamp_to_symbol_view();
        self.sync_tab_cells();
        let block_style = if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let surfaces = self.theme.surfaces().is_some();
        let block = if surfaces {
            crate::widgets::surface::pane_block(self.theme.editor_bg())
        } else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(block_style)
        };
        let mut inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient that matches the welcome activity box. The editor has no
        // title, so nothing needs re-stamping over the gradient top edge.
        // Croft Loch always draws the seam under the header along the top
        // edge, and lights it with the accent lip only while the editor has
        // the keyboard: a modal quick input above it takes the lip away.
        if surfaces {
            crate::widgets::surface::paint_pane_top(
                buf,
                area,
                self.theme,
                self.focused && !self.overlay_has_keyboard,
            );
        } else if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        self.last_inner = inner;
        self.last_scrollbar = Rect::default();
        self.last_hscrollbar = Rect::default();
        self.merge_action_spans.clear();
        self.code_lens_spans.clear();
        self.blame_span = None;
        // Every rect this frame publishes is cleared up front, so a frame
        // that paints nothing leaves nothing behind for the mouse path to
        // hit-test against. `render_log` sets its own when it paints; the
        // early return below means it may never run at all, which is the
        // case a reset inside `render_log` cannot cover.
        if let Some(log) = self.log.as_mut() {
            log.last_body = Rect::default();
        }

        if inner.height == 0 {
            return;
        }
        let cbg = canvas_bg(
            crate::iterm2_inline::detect_iterm2_inline_support(),
            self.theme,
        );
        // Merge editor (#253): reconcile the tracked regions with any
        // manual edit since last frame, carve the source panes off the
        // top, then FALL THROUGH to the text path with the remaining
        // rect — the Result below is the ordinary text buffer.
        let (len, seq, row) = (self.lines.len(), self.edit_seq, self.merge_edit_row);
        let theme = self.theme;
        if let Some(mv) = self.merge.as_mut() {
            mv.sync_with_buffer(len, seq, row);
            inner = render_merge_panes(mv, inner, buf, theme);
            self.last_inner = inner;
        }
        // The Result height comes from the POST-carve rect: the merge
        // panes above just shrank `inner`, and every row / scrollbar /
        // cursor-visibility computation below must see what is left.
        let height = inner.height as usize;
        if height == 0 {
            return;
        }
        if let Some(image) = self.image.as_ref() {
            let ibg = image_canvas_bg(
                crate::iterm2_inline::detect_inline_image_protocol(),
                self.theme,
            );
            render_image_placeholder(image, self.path.as_deref(), inner, buf, ibg, self.theme);
            if self.cell_images {
                let area = Rect {
                    y: inner.y + 1,
                    height: inner.height.saturating_sub(1),
                    ..inner
                };
                let key = (image.generation, area.width, area.height, ibg);
                if self.cell_image.as_ref().map(|c| c.key) != Some(key) {
                    // A body that does not decode (a truncated file whose
                    // header still gave dimensions) is cached as an empty
                    // picture too, so each frame does not decode it again.
                    let (cols, rows, cells) =
                        halfblock_cells(&image.bytes, area.width, area.height, ibg)
                            .unwrap_or_default();
                    self.cell_image = Some(CellImage {
                        key,
                        cols,
                        rows,
                        cells,
                    });
                }
                if let Some(picture) = self.cell_image.as_ref() {
                    paint_cell_image(picture, area, buf);
                }
            }
            return;
        }
        if let Some(view) = self.sheet.as_mut() {
            render_sheet(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.archive.as_mut() {
            render_archive(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.pr_review.as_mut() {
            crate::widgets::pr_review::render(view, inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.fleet.as_mut() {
            crate::widgets::fleet::render(view, inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.sarif.as_mut() {
            crate::sarif::render::render(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if let Some(view) = self.log.as_mut() {
            let search = self
                .search_highlight
                .as_deref()
                .map(|t| (t, self.search_highlight_opts));
            let active = self.active_search_match;
            render_log(
                view,
                self.path.as_deref(),
                inner,
                buf,
                cbg,
                self.theme,
                self.scroll,
                search,
                active,
            );
            return;
        }
        if let Some(view) = self.hex.as_mut() {
            render_hex(view, self.path.as_deref(), inner, buf, cbg, self.theme);
            return;
        }
        if self.markdown_preview.is_some() {
            self.render_markdown_preview(inner, buf);
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            let (prev_arrow, next_arrow) =
                render_diff(diff, inner, buf, cbg, self.theme, self.pointer);
            self.diff_prev_arrow = prev_arrow;
            self.diff_next_arrow = next_arrow;
            return;
        }
        // Non-diff tabs: clear the hit rects so a stale arrow click on a
        // tab the user just switched away from can't fire.
        self.diff_prev_arrow = Rect::default();
        self.diff_next_arrow = Rect::default();

        // Gutter = a left glyph margin (the breakpoint / stop column, VS Code
        // style) + the right-aligned line number + its trailing space. Folding
        // the margin into `gutter_width` shifts the numbers and code right
        // together, so the number-to-code gap is unchanged and click/cursor
        // mapping (which derives from `gutter_width`) stays correct for free.
        const SIGN_MARGIN: u16 = 2;
        // `off` drops the number column only; the sign margin with its
        // breakpoints and fold chevrons, and the change bar, stay (#1576).
        let digits = match self.line_numbers {
            LineNumbers::Off => 0,
            _ => (self.lines.len() + 1).to_string().len() as u16,
        };
        let gutter_width = digits + 1 + SIGN_MARGIN;
        self.last_gutter_width = gutter_width;
        // Rebuild the git-gutter marks if the buffer moved since last frame
        // (cheap no-op otherwise), so the bars below diff against HEAD.
        self.refresh_git_marks();
        // Same cadence for the fallback fold table (#254): region markers
        // and comment runs the gutter chevrons below consult.
        self.refresh_fold_tables();
        // Rescan merge-conflict blocks on the same cadence so the region
        // tints below always match the buffer.
        self.conflicts();
        // Fold headers are line indexes; an insert/delete anywhere shifts them.
        // If the line count changed since folds were set, drop them wholesale
        // rather than hide the wrong lines (see `fold_epoch_lines`).
        if !self.folded.is_empty() && self.lines.len() != self.fold_epoch_lines {
            self.folded.clear();
            self.hidden_ranges.clear();
        }
        // A caret on a hidden line has no painted row, so nothing is drawn and
        // the user cannot see where they are. Every direct `cursor_row` setter
        // can land there — go-to-definition, search, Ctrl+G, a restored session
        // — and enumerating them is how one gets missed, so normalise here,
        // where all of them converge before anything is painted. One binary
        // search against `hidden_ranges` when a fold exists, nothing when none
        // does.
        if !self.folded.is_empty() && self.is_line_hidden(self.cursor_row) {
            self.reveal_cursor_fold();
        }
        let wrap = self.wrap_enabled();
        if wrap {
            // Wrapped text folds onto extra rows instead of scrolling sideways.
            self.scroll_col = 0;
        }
        let text_x = inner.x + gutter_width + 1;

        // Layout has a small cycle: the horizontal bar takes the bottom row
        // only when a line overflows the text column, but the text column's
        // width depends on whether the vertical bar takes a column. Break it
        // in one pass: provisionally reserve the vertical bar's column from
        // the FULL height to decide the horizontal bar, then recompute the
        // vertical thumb against the real (possibly shortened) viewport.
        // Reserving the horizontal row can only ever narrow the text column,
        // never widen it, so a line that overflows under the provisional
        // width still overflows under the final one - the decision is stable.
        // The horizontal bar never appears in wrap mode (lines fold instead).
        let provisional_text_width = inner
            .width
            .saturating_sub(gutter_width + 2 + u16::from(self.lines.len() > height))
            as usize;
        let content_cols = self.content_cols(provisional_text_width);
        let hbar_present =
            !wrap && provisional_text_width > 0 && content_cols > provisional_text_width;
        let text_height = height - usize::from(hbar_present);
        self.last_text_rows = text_height as u16;

        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: text_height as u16,
        };

        // Decide the vertical scrollbar and final text width, then clamp the
        // scroll so the cursor stays visible. In wrap mode the content extent
        // and scroll position are measured in VISUAL rows (a logical line may
        // span several); otherwise in logical lines, one per row.
        let (text_width, scrollbar_metrics) = if wrap {
            let wide = inner.width.saturating_sub(gutter_width + 2) as usize;
            // Overflow at the wide width forces the vbar, which narrows the
            // column and only ever adds rows, so the decision is stable.
            let vbar = match self.clip_content_rows(wide, true) {
                Some((_, rows)) => rows > text_height,
                None => self.total_visual_rows(wide) > text_height,
            };
            let tw = if vbar { wide.saturating_sub(1) } else { wide };
            // A symbol tab measures only its clip: `base` rows sit above it.
            let (base, content_rows) = match self.clip_content_rows(tw, true) {
                Some(clip) => clip,
                None => (0, self.total_visual_rows(tw)),
            };

            // Keep the cursor's visual row inside the viewport.
            if text_height > 0 {
                let cursor_vrow = self.cursor_visual_row(tw);
                let mut top = self.top_visual_row(tw);
                if cursor_vrow < top {
                    top = cursor_vrow;
                } else if cursor_vrow >= top + text_height {
                    top = cursor_vrow + 1 - text_height;
                }
                top = top
                    .min(base + content_rows.saturating_sub(text_height))
                    .max(base);
                self.set_top_to_visual_row(top, tw);
            }
            let metrics = scrollbar::vertical_metrics(
                scrollbar_area,
                content_rows,
                text_height,
                self.top_visual_row(tw).saturating_sub(base),
            );
            (tw as u16, metrics)
        } else {
            // `scroll_sub` counts rows INTO the top line's comment box (set
            // by the content-row scroller); clamp it to the group and drop
            // it entirely when a cursor clamp below moves the top line.
            if self.comment_boxes.is_empty() {
                self.scroll_sub = 0;
            } else {
                let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                let group = 1 + self.box_rows_between(self.scroll, self.scroll + 1, bw);
                self.scroll_sub = self.scroll_sub.min(group - 1);
            }
            let scroll_before = self.scroll;
            if text_height > 0 {
                if self.cursor_row < self.scroll {
                    self.scroll = self.cursor_row;
                } else if self.cursor_row >= self.scroll + text_height {
                    self.scroll = self.cursor_row + 1 - text_height;
                }
                // Comment boxes between the top and the cursor consume rows;
                // scroll further until the cursor's painted row fits. Boxes
                // are measured at the pre-scrollbar width, one column
                // narrower than the final text column, which can only
                // overestimate their height (the cursor stays visible).
                if !self.comment_boxes.is_empty() {
                    let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                    while self.cursor_row + self.box_rows_between(self.scroll, self.cursor_row, bw)
                        >= self.scroll + text_height
                    {
                        self.scroll += 1;
                    }
                }
            }
            if let Some((first, end)) = self.symbol_clip() {
                // The lowest top that still fills the pane with the clip's
                // tail, so scrolling never strands one clip line at the top.
                let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
                let (mut top, mut rows) = (end, 0);
                while top > first {
                    let line = top - 1;
                    let need = if self.is_line_hidden(line) {
                        0
                    } else {
                        1 + self.box_rows_between(line, top, bw)
                    };
                    if rows + need > text_height {
                        break;
                    }
                    rows += need;
                    top = line;
                }
                self.scroll = self.scroll.clamp(first, top.min(end - 1));
            }
            if self.scroll != scroll_before {
                self.scroll_sub = 0;
            }
            let bw = inner.width.saturating_sub(gutter_width + 3) as usize;
            let (base, content) = self.clip_content_rows(bw, false).unwrap_or((
                0,
                self.lines.len() + self.box_rows_between(0, self.lines.len(), bw),
            ));
            let metrics = scrollbar::vertical_metrics(
                scrollbar_area,
                content,
                text_height,
                // Content-row top: lines above plus box rows above plus the
                // rows scrolled into the top line's own box.
                (self.scroll + self.box_rows_between(0, self.scroll, bw) + self.scroll_sub)
                    .saturating_sub(base),
            );
            let sw = u16::from(metrics.is_some());
            let tw = inner.width.saturating_sub(gutter_width + 2 + sw);
            (tw, metrics)
        };
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }

        // Clamp horizontal scroll (non-wrap only) so a wheel swipe can't
        // strand the buffer off-screen, then lay out the horizontal bar.
        // The extent depends on the width it is measured against, and the
        // final text column may be narrower than the provisional one.
        let content_cols = self.content_cols(text_width as usize);
        let max_scroll_col = content_cols.saturating_sub(text_width as usize);
        self.scroll_col = self.scroll_col.min(max_scroll_col);
        let hbar_metrics = scrollbar::horizontal_metrics(
            Rect {
                x: text_x,
                y: inner.y + text_height as u16,
                width: text_width,
                height: u16::from(hbar_present),
            },
            content_cols,
            text_width as usize,
            self.scroll_col,
        );
        self.last_hscrollbar = hbar_metrics.map(|m| m.area).unwrap_or_default();

        // Build the visual-row layout the loop paints and the inverse maps
        // (cursor/click) read back. Each entry is (logical_line, char_start,
        // char_end). Non-wrap: one row per visible line, range [scroll_col,
        // scroll_col + text_width). Wrap: the wrapped segments from
        // (scroll, scroll_sub) down, capped at the viewport height.
        let mut visual_rows: Vec<VisRow> = Vec::with_capacity(text_height);
        // The boxes hanging under a line, emitted right after its last
        // segment. `group_pos` is the row's index within the line's whole
        // group (segments first, then box rows) so a wrap viewport top that
        // decomposed into a box region resumes mid-box.
        let push_box_rows = |rows: &mut Vec<VisRow>,
                             ed: &Editor,
                             line: usize,
                             skip: usize,
                             seg_count: usize,
                             tw: usize| {
            if ed.comment_boxes.is_empty() {
                return;
            }
            let mut group_pos = seg_count;
            for idx in 0..ed.comment_boxes.len() {
                if ed.comment_boxes[idx].line != line {
                    continue;
                }
                for box_row in 0..ed.comment_box_height(idx, tw) {
                    if group_pos >= skip && rows.len() < text_height {
                        rows.push(VisRow::Box {
                            box_idx: idx,
                            box_row,
                        });
                    }
                    group_pos += 1;
                }
            }
        };
        // The lines of a live inline suggestion after its first (#1625),
        // hung under the caret's line as their own rows: only the first fits
        // at the caret, and a "(+2 lines)" count let Tab insert a function
        // body nobody had read. The caret row says how many did not fit.
        let ghost_tail = if self.focused {
            self.live_ghost()
                .map_or(0, |t| t.split('\n').count().saturating_sub(1))
        } else {
            0
        };
        let push_ghost_rows = |rows: &mut Vec<VisRow>| {
            for index in 1..=ghost_tail {
                if rows.len() >= text_height {
                    break;
                }
                rows.push(VisRow::Ghost { index });
            }
        };
        // A symbol tab stops painting at its symbol's last line.
        let clip_end = self.symbol_clip().map_or(self.lines.len(), |(_, end)| end);
        if wrap {
            let tw = text_width as usize;
            let mut line = self.scroll;
            let mut skip = self.scroll_sub;
            while line < clip_end && visual_rows.len() < text_height {
                if self.is_line_hidden(line) {
                    line += 1;
                    continue;
                }
                let segs = self.line_segments(line, tw);
                for (i, &(s, e)) in segs.iter().enumerate() {
                    if i < skip {
                        continue;
                    }
                    if visual_rows.len() >= text_height {
                        break;
                    }
                    visual_rows.push(VisRow::Text {
                        line,
                        start: s,
                        end: e,
                    });
                }
                if line == self.cursor_row && skip < segs.len() {
                    push_ghost_rows(&mut visual_rows);
                }
                push_box_rows(&mut visual_rows, self, line, skip, segs.len(), tw);
                skip = 0;
                line += 1;
            }
        } else {
            // Collect up to `text_height` VISIBLE logical lines from `scroll`,
            // skipping any hidden inside a collapsed fold.
            let tw = text_width as usize;
            let mut line = self.scroll;
            // Group-position skip on the top line: 0 = its text row, 1..
            // = rows into its comment box, so the viewport can start
            // mid-box exactly like the wrap path.
            let mut skip = self.scroll_sub;
            while line < clip_end && visual_rows.len() < text_height {
                if !self.is_line_hidden(line) {
                    if skip == 0 && visual_rows.len() < text_height {
                        visual_rows.push(VisRow::Text {
                            line,
                            start: self.scroll_col,
                            end: self.scroll_col + text_width as usize,
                        });
                        if line == self.cursor_row {
                            push_ghost_rows(&mut visual_rows);
                        }
                    }
                    push_box_rows(&mut visual_rows, self, line, skip, 1, tw);
                    skip = 0;
                }
                line += 1;
            }
        }
        self.last_wrap_rows = visual_rows.clone();

        let sel_norm = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised());
        let occ_needle = self.selection_occurrence_needle();
        // Bracket-match highlight (VS Code `editorBracketMatch`): only in the
        // focused editor, and only when the caret is beside a bracket.
        let bracket_pair = if self.focused {
            self.cached_bracket_match_pair()
        } else {
            None
        };
        // Indentation guides: the cursor block's active guide (focused editor
        // only) and the paint colours, resolved once per frame — the active
        // walk is O(block), not per-row work.
        let guide_step = self.guide_step();
        let active_guide = (self.show_indent_guides && self.focused)
            .then(|| self.active_indent_guide())
            .flatten();
        let guide_fg = self.theme.indent_guide();
        let guide_active_fg = self.theme.indent_guide_active();
        // A `surfaces` theme keeps the gutter quiet: an expanded fold's
        // chevron shows on the caret's line, or on every foldable line while
        // the pointer is over the gutter (VS Code's `showFoldingControls:
        // mouseover`); the change bar hugs the code edge as a quarter block.
        let surfaces = self.theme.surfaces().is_some();
        let gutter_hovered = self.pointer.is_some_and(|(px, py)| {
            px >= inner.x
                && px <= inner.x + gutter_width
                && py >= inner.y
                && py < inner.y + inner.height
        });
        // What every row shares this frame.
        let frame = RowFrame {
            inner,
            gutter_width,
            text_x,
            text_width,
            wrap,
            sel_norm,
            guide_step,
            active_guide,
            guide_fg,
            guide_active_fg,
            surfaces,
            gutter_hovered,
        };
        // Each text row's geometry, refilled per row (#873).
        let mut row_cells = RowCells::default();

        for (row_idx, vis_row) in visual_rows.iter().enumerate() {
            let y = inner.y + row_idx as u16;
            let (line_idx, row_start, row_end) = match *vis_row {
                VisRow::Box { box_idx, box_row } => {
                    self.paint_comment_box_row(
                        buf,
                        inner,
                        y,
                        gutter_width,
                        text_x,
                        text_width,
                        box_idx,
                        box_row,
                    );
                    continue;
                }
                VisRow::Ghost { index } => {
                    self.paint_ghost_row(
                        buf,
                        inner,
                        y,
                        gutter_width,
                        text_x,
                        text_width,
                        index,
                        wrap,
                    );
                    continue;
                }
                VisRow::Text { line, start, end } => (line, start, end),
            };
            self.paint_row_gutter(buf, &frame, y, line_idx, !wrap || row_start == 0);

            let (line_len, row_width) =
                self.paint_row_text(buf, &frame, &mut row_cells, y, line_idx, row_start, row_end);
            let raw = &self.lines[line_idx];

            // Merge-conflict region tints (VS Code's current/incoming
            // backgrounds), painted right after the text so diagnostics,
            // search highlights, and the selection all win over them.
            if let Some(tint) = conflict_row_tint(&self.conflicts, line_idx, self.theme) {
                paint_full_row_bg(buf, text_x, y, row_width, tint);
            }
            // Clickable accept buttons on the conflict header row — VS Code's
            // merge-conflict CodeLens, drawn after the marker text where the
            // `<<<<<<<` line is otherwise dead space (#1169). A narrow row
            // gets the short labels rather than losing buttons. Hit spans are
            // recorded per frame (cleared at render start) so the rects always
            // describe the painted frame (#103's invariant).
            if let Some(block_idx) = self.conflicts.iter().position(|b| b.ours_start == line_idx) {
                let block = self.conflicts[block_idx];
                let marker_len = row_cells.text_before(line_len) as u16;
                // A header that wraps gets them after its last segment only.
                if marker_len <= row_width {
                    use crate::merge::Resolution;
                    let chips = [
                        AcceptChip::new("Accept Current", "Current", ACCEPT_CURRENT_FILL),
                        AcceptChip::new("Accept Incoming", "Incoming", ACCEPT_INCOMING_FILL),
                        AcceptChip::new("Accept Both", "Both", ACCEPT_BOTH_FILL),
                    ];
                    let res = [Resolution::Current, Resolution::Incoming, Resolution::Both];
                    let x0 = text_x + marker_len + 2;
                    for (i, xs) in
                        paint_accept_chips(buf, &chips, x0, text_x + row_width, y, self.theme)
                    {
                        self.merge_action_spans
                            .push((y, xs, block.ours_start, res[i]));
                    }
                }
            }

            // LSP diagnostics: underline each problem span in its severity
            // colour, clipped to the visible row window. VS Code draws a wavy
            // underline; the crossterm backend exposes only a straight one, so
            // a coloured straight underline carries the severity instead. The
            // underline colour is independent of the glyph's foreground, so the
            // syntax/semantic colour underneath is preserved.
            let empty_diag: Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)> =
                Vec::new();
            for &(sc, ec, severity) in self.diagnostic_spans.get(line_idx).unwrap_or(&empty_diag) {
                let vs = row_cells.cell(sc);
                // The exclusive end shifts by the hints before its LAST char,
                // so an underline never swallows a hint anchored right at it.
                let ve = row_cells.end(ec);
                if ve > vs {
                    paint_diagnostic_underline(
                        buf, text_x, y, row_width, vs, ve, severity, self.theme,
                    );
                }
            }
            let empty_tags: Vec<(usize, usize, crate::lsp::manager::DiagnosticTags)> = Vec::new();
            for &(sc, ec, tags) in self
                .diagnostic_tag_spans
                .get(line_idx)
                .unwrap_or(&empty_tags)
            {
                let vs = row_cells.cell(sc);
                let ve = row_cells.end(ec);
                if ve > vs {
                    paint_diagnostic_tags(buf, text_x, y, row_width, vs, ve, tags);
                }
            }

            // LSP occurrences of the symbol under the caret (word highlight):
            // painted before the find layer, the selection band, and the
            // bracket match so all three stay visible on top. The find layer
            // in particular paints black-on-gold; an occurrence bg over it
            // left black text on a dark grey (the caret parked on a find
            // match makes the two layers cover the same cells).
            for &(occ_row, occ_start, occ_end, occ_write) in &self.occurrences {
                if occ_row != line_idx {
                    continue;
                }
                let bg = if occ_write {
                    self.theme.occurrence_write_bg()
                } else {
                    self.theme.occurrence_bg()
                };
                for c in occ_start.max(row_start)..occ_end {
                    if !paint_row_cells(buf, text_x, y, row_width, &row_cells, c, |s| s.bg(bg)) {
                        break;
                    }
                }
            }

            if let Some(term) = self
                .search_highlight
                .as_deref()
                .filter(|_| find_window(self.find_range, line_idx) != Some((0, 0)))
            {
                let active_on_line = self
                    .active_search_match
                    .filter(|(r, _, _)| *r == line_idx)
                    .map(|(_, c, l)| (c, l));
                paint_search_highlight(
                    buf,
                    text_x,
                    y,
                    row_width,
                    raw,
                    term,
                    self.search_highlight_opts,
                    &row_cells,
                    active_on_line,
                    None,
                    find_window(self.find_range, line_idx),
                    self.theme,
                );
            }

            if let Some(needle) = occ_needle.as_deref() {
                paint_selection_occurrences(
                    buf, text_x, y, row_width, raw, needle, &row_cells, self.theme,
                );
            }

            if let Some(((sr, sc), (er, ec))) = sel_norm
                && line_idx >= sr
                && line_idx <= er
            {
                let sel_start = if line_idx == sr { sc } else { 0 };
                // For non-final selected rows, paint past the content by one
                // cell to make the trailing newline visible.
                let sel_end = if line_idx == er { ec } else { line_len + 1 };
                let visible_start = row_cells.cell(sel_start);
                let visible_end = row_cells.end(sel_end);
                if visible_end > visible_start {
                    paint_selection_band(
                        buf,
                        text_x,
                        y,
                        row_width,
                        visible_start,
                        visible_end,
                        self.theme,
                    );
                }
            }

            // Bracket-match highlight: tint the two matched bracket cells that
            // fall on this visual row, honouring horizontal scroll.
            if let Some((open, close)) = bracket_pair {
                for pos in [open, close] {
                    if pos.0 == line_idx && pos.1 >= row_start {
                        paint_bracket_match(
                            buf,
                            text_x,
                            y,
                            row_width,
                            row_cells.cell(pos.1),
                            self.theme,
                        );
                    }
                }
            }

            // Secondary multi-cursor carets ("Change All Occurrences"): paint
            // each one's selection band and a software block cursor at its
            // head. The primary cursor is the hardware caret; these blocks
            // mark the extra cursors so the user sees every edit point.
            for caret in &self.carets {
                let ((cr0, cc0), (cr1, cc1)) = caret.normalised();
                if line_idx >= cr0 && line_idx <= cr1 {
                    let cs = if line_idx == cr0 { cc0 } else { 0 };
                    let ce = if line_idx == cr1 { cc1 } else { line_len + 1 };
                    let vs = row_cells.cell(cs);
                    let ve = row_cells.end(ce);
                    if ve > vs {
                        paint_selection_band(buf, text_x, y, row_width, vs, ve, self.theme);
                    }
                }
                // Only paint the block on the visual row that holds the caret.
                let c = caret.head.1;
                let on_row = caret.head.0 == line_idx
                    && c >= row_start
                    && (c < row_end || (c == row_end && row_end == line_len));
                if on_row {
                    // `caret`, not `cell`: a caret AT a hint's anchor sits
                    // left of the hint, inside its own band.
                    paint_block_cursor(buf, text_x, y, row_width, row_cells.caret(c), self.theme);
                }
            }

            // Collaborators' ghost carets (multiplayer): the same block shape
            // as secondary carets, in each participant's color. The local
            // hardware caret is painted by the terminal after drawing, so it
            // always sits above these.
            for &(gr, gc, color) in &self.ghost_carets {
                let on_row = gr == line_idx
                    && gc >= row_start
                    && (gc < row_end || (gc == row_end && row_end == line_len));
                if on_row {
                    paint_ghost_caret(buf, text_x, y, row_width, row_cells.caret(gc), color);
                }
            }

            // GitLens-style inline blame: a dim italic annotation trailing the
            // cursor's line, on its last visual segment, in the focused editor
            // only. Painted last so no overlay covers it, and clipped to the
            // pane's right edge.
            if self.focused
                && line_idx == self.cursor_row
                && row_end >= line_len
                && !self.inline_values.contains_key(&line_idx)
                && self.live_note(line_idx).is_none()
                && self.lenses_on_line(line_idx).is_empty()
                && self.live_ghost().is_none()
                && let Some(note) = self.current_line_blame_annotation()
            {
                let text_cols = row_cells.cell(line_len);
                let start_x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                if start_x < right {
                    let avail = (right - start_x) as usize;
                    let shown: String = note.chars().take(avail).collect();
                    buf.set_string(
                        start_x,
                        y,
                        &shown,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                    // Hover and click reach the commit it names (#1556);
                    // `Uncommitted changes` has none to show.
                    if let Some(LineBlame::Commit(c)) = self.cursor_line_blame() {
                        let hash = c.hash.clone();
                        let w = unicode_width::UnicodeWidthStr::width(shown.as_str()) as u16;
                        self.blame_span = Some((y, start_x..(start_x + w).min(right), hash));
                    }
                }
            }

            // Debugger inline values (#135): the "name = value" trailer for a
            // stopped session, on the line's last visual segment like the
            // blame above (which yields to it on the cursor line — while
            // stepping, state beats history). Clipped to the pane edge.
            if row_end >= line_len
                && let Some(note) = self.inline_values.get(&line_idx)
            {
                let text_cols = row_cells.cell(line_len);
                let start_x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                if start_x < right {
                    let avail = (right - start_x) as usize;
                    let shown: String = note.chars().take(avail).collect();
                    buf.set_string(
                        start_x,
                        y,
                        &shown,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }

            // A language server's replacement strikes through the columns it
            // takes the place of, and its text stands one cell clear of them.
            let replaces = self
                .live_ghost_full()
                .and_then(|g| g.replaces.clone())
                .filter(|_| self.focused && line_idx == self.cursor_row);
            if let Some(r) = &replaces {
                let (vs, ve) = (row_cells.cell(r.start), row_cells.end(r.end));
                if ve > vs {
                    let struck = crate::lsp::manager::DiagnosticTags {
                        unnecessary: true,
                        deprecated: true,
                    };
                    paint_diagnostic_tags(buf, text_x, y, row_width, vs, ve, struck);
                }
            }

            // Inline suggestion (#607): the first line as ghost text right at
            // the caret (suggestions are only asked for at a line's end); the
            // rest hang below as `VisRow::Ghost` rows (#1625), and only lines
            // that found no row are counted here. It owns the line's tail, so
            // the trailers below stand aside.
            let ghost_here = self.focused
                && line_idx == self.cursor_row
                && row_end >= line_len
                && self.live_ghost().is_some();
            if ghost_here && let Some(text) = self.live_ghost() {
                let text_cols = row_cells.cell(line_len) + usize::from(replaces.is_some());
                let x = text_x + text_cols as u16;
                let right = inner.x + inner.width;
                if x < right {
                    let first = text.split('\n').next().unwrap_or("");
                    let shown_rows = visual_rows
                        .iter()
                        .filter(|r| matches!(r, VisRow::Ghost { .. }))
                        .count();
                    let more = text.split('\n').count() - 1 - shown_rows;
                    let shown = if more > 0 {
                        format!(
                            "{first}  (+{more} line{})",
                            if more == 1 { "" } else { "s" }
                        )
                    } else {
                        first.to_string()
                    };
                    let clipped: String = shown.chars().take((right - x) as usize).collect();
                    buf.set_string(
                        x,
                        y,
                        &clipped,
                        Style::default()
                            .fg(self.theme.ignored_fg())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }

            // Code lenses (#608): the server's "3 references | Run" at the end
            // of the symbol's line, clickable, yielding to the debugger's and
            // Live Run's trailers, which describe this very run.
            if row_end >= line_len
                && !ghost_here
                && !self.inline_values.contains_key(&line_idx)
                && self.live_note(line_idx).is_none()
            {
                let lenses = self.lenses_on_line(line_idx);
                if !lenses.is_empty() {
                    let text_cols = row_cells.cell(line_len);
                    let mut x = text_x + text_cols as u16 + 2;
                    let right = inner.x + inner.width;
                    let style = Style::default()
                        .fg(self.theme.ignored_fg())
                        .add_modifier(Modifier::UNDERLINED);
                    for (n, idx) in lenses.into_iter().enumerate() {
                        if n > 0 && x + 3 < right {
                            buf.set_string(
                                x,
                                y,
                                " | ",
                                Style::default().fg(self.theme.ignored_fg()),
                            );
                            x += 3;
                        }
                        if x >= right {
                            break;
                        }
                        let title = &self.code_lenses[idx].title;
                        let shown: String = title.chars().take((right - x) as usize).collect();
                        let w = unicode_width::UnicodeWidthStr::width(shown.as_str()) as u16;
                        buf.set_string(x, y, &shown, style);
                        self.code_lens_spans.push((y, x..x + w, idx));
                        x += w;
                    }
                }
            }

            // Live Run trailer: what this line did on the last run, painted
            // where the debugger's values go and yielding to them, since a
            // paused session is the more specific answer.
            if row_end >= line_len
                && !ghost_here
                && !self.inline_values.contains_key(&line_idx)
                && let Some(note) = self.live_note(line_idx)
            {
                let text_cols = row_cells.cell(line_len);
                let mut x = text_x + text_cols as u16 + 2;
                let right = inner.x + inner.width;
                for (text, kind) in note {
                    if x >= right {
                        break;
                    }
                    let color = match kind {
                        crate::live_run::Kind::Value => self.theme.ignored_fg(),
                        crate::live_run::Kind::Output => {
                            self.theme.ui(Color::Rgb(0x3b, 0x9e, 0xff))
                        }
                        crate::live_run::Kind::Error => self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
                        crate::live_run::Kind::Timeout => {
                            self.theme.ui(Color::Rgb(0xcc, 0xa7, 0x00))
                        }
                    };
                    let avail = (right - x) as usize;
                    let shown: String = text.chars().take(avail).collect();
                    let width = unicode_width::UnicodeWidthStr::width(shown.as_str()) as u16;
                    buf.set_string(
                        x,
                        y,
                        &shown,
                        Style::default().fg(color).add_modifier(Modifier::ITALIC),
                    );
                    x = x.saturating_add(width + 2);
                }
            }

            // The cursor itself is drawn by the host terminal as a hardware
            // caret (DECSCUSR `BlinkingBar`); App calls
            // `frame.set_cursor_position(...)` so the blink/overlay never
            // hides the underlying character.
        }

        // Rulers (#1524, VS Code `editor.rulers`): a dim │ at each ruler
        // column down the whole text area, past the end of the file too.
        // Blank cells only, so text always wins, as with the indent guides;
        // a long line runs over its ruler, which still shows the limit. The
        // column is fixed on screen: the flat path shifts it by the
        // horizontal scroll, and in wrap mode every visual row carries it.
        if !self.rulers.is_empty() {
            let ruler_fg = self.theme.ruler();
            let shift = if wrap { 0 } else { self.scroll_col };
            for row in 0..text_height {
                if matches!(visual_rows.get(row), Some(VisRow::Box { .. })) {
                    continue;
                }
                let y = inner.y + row as u16;
                for &ruler in &self.rulers {
                    let Some(col) = usize::from(ruler).checked_sub(shift) else {
                        continue;
                    };
                    if col >= text_width as usize {
                        continue;
                    }
                    let cell = &mut buf[(text_x + col as u16, y)];
                    if cell.symbol() != " " {
                        continue;
                    }
                    cell.set_symbol("\u{2502}");
                    let style = cell.style().fg(ruler_fg).remove_modifier(Modifier::ITALIC);
                    cell.set_style(style);
                }
            }
        }

        // Ghost caret name tags (VS Code Live Share): while a peer's caret is
        // inside its fade window the App feeds a label here, painted on the
        // visual row above the caret — below when the caret sits on the
        // viewport's top row — at the caret's on-screen column. Painted after
        // the row loop (a tag overlays a neighbouring row's content) but
        // before sticky scroll, which floats above everything.
        for (cl, cc, name, color) in &self.ghost_caret_labels {
            let Some(caret_vidx) = visual_rows.iter().position(|r| {
                matches!(r, VisRow::Text { line: li, start: s, end: e }
                    if *li == *cl && *cc >= *s
                        && (*cc < *e || (*cc == *e && *e == self.line_char_len(*li))))
            }) else {
                continue;
            };
            let label_vidx = if caret_vidx > 0 {
                caret_vidx - 1
            } else {
                caret_vidx + 1
            };
            if label_vidx >= visual_rows.len() {
                continue;
            }
            // The caret's on-screen column: translate through the caret row's
            // own inlay map, exactly as its ghost cell was painted.
            let VisRow::Text {
                start: caret_start,
                end: caret_end,
                ..
            } = visual_rows[caret_vidx]
            else {
                continue;
            };
            if *cc < caret_start {
                continue;
            }
            let caret_raw = &self.lines[*cl];
            let hint_cap = caret_end.min(self.line_char_len(*cl));
            row_cells.fill(
                &caret_raw[byte_index_of_char(caret_raw, caret_start)
                    ..byte_index_of_char(caret_raw, caret_end)],
                caret_start,
                self.row_origin(caret_raw, caret_start),
                self.row_inlay_spans(*cl)
                    .iter()
                    .filter(|(hc, _, _)| *hc >= caret_start && *hc <= hint_cap)
                    .map(|(hc, l, _)| (*hc, l.chars().count())),
            );
            let x0 = row_cells.caret(*cc) as u16;
            let y = inner.y + label_vidx as u16;
            let style = Style::default()
                .fg(Color::Black)
                .bg(*color)
                .add_modifier(Modifier::BOLD);
            for (i, ch) in name.chars().enumerate() {
                let x = x0 + i as u16;
                if x >= text_width {
                    break;
                }
                buf[(text_x + x, y)].set_char(ch).set_style(style);
            }
        }

        // Sticky scroll: pin the enclosing scope headers to the top of the
        // viewport, overpainting the topmost content rows (VS Code's sticky
        // widget floats over the content the same way). Non-wrap only; the app
        // supplies the header lines outermost-first, already filtered to those
        // scrolled above the viewport.
        self.sticky_click_rows.clear();
        if !wrap && !self.sticky_lines.is_empty() && inner.height > 1 {
            let bg = self.theme.sticky_scroll_bg();
            let text_w = (inner.x + inner.width).saturating_sub(text_x);
            // Never paint over the caret's own row: a caret under the band
            // cannot be seen at all. `caret_floor_row` keeps a scroll from
            // parking it there, so this normally trims nothing; it still has to
            // hold for the frames where the caret arrived some other way.
            //
            // The caret's PAINTED row, from the layout built above — counting
            // buffer lines instead would overstate it whenever a collapsed fold
            // or a comment box sits between the top of the viewport and the
            // caret, and the band would paint straight over it. `None` means
            // the caret's line is not painted at all (scrolled off, or hidden
            // inside a fold), so nothing needs to be kept clear.
            let rows_avail = (inner.height - 1) as usize;
            let caret_row = self
                .last_wrap_rows
                .iter()
                .position(|r| matches!(r, VisRow::Text { line, .. } if *line == self.cursor_row));
            let max_rows = match caret_row {
                Some(row) => rows_avail.min(row),
                None => rows_avail,
            };
            let sticky = self.sticky_lines.clone();
            for (i, &line) in sticky.iter().take(max_rows).enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + inner.width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let line_no = format!(
                    "{:>width$} ",
                    self.gutter_number(line as usize)
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    width = gutter_width.saturating_sub(1) as usize
                );
                buf.set_string(
                    inner.x,
                    y,
                    &line_no,
                    Style::default().fg(self.theme.line_number_fg()).bg(bg),
                );
                self.paint_highlighted_line(buf, text_x, y, text_w, line as usize);
                self.sticky_click_rows.push((y, line));
            }
            // The band just overpainted those rows, so any [Accept …] span
            // recorded there no longer matches what is on screen — a click
            // on a sticky header must never resolve an unseen conflict.
            let band_end = inner.y + sticky.len().min(max_rows) as u16;
            self.merge_action_spans
                .retain(|(y, _, _, _)| *y >= band_end);
        }

        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
            // Overview ruler: ticks for the whole file painted over the track
            // the thumb rides on, so problems and search hits off screen are
            // still visible. Computed first so `self` is free while painting.
            if self.overview_ruler {
                let ticks = self.overview_ruler_rows_wrapped(
                    metrics.area.height,
                    wrap.then_some(text_width as usize),
                );
                paint_overview_ruler(buf, metrics.area, &ticks, self.theme);
            }
        }
        if let Some(metrics) = hbar_metrics {
            scrollbar::render_horizontal(buf, metrics, self.focused, self.theme);
        }
    }
}

impl Editor {
    /// Paint the gutter of one visual row at screen row `y` showing
    /// `line_idx`: the current-line wash, the line number, the sign cell, the
    /// fold chevron and the change lane. `first_row` is whether this is the
    /// line's first visual row, the only one that carries its number and
    /// glyphs when the line wraps.
    fn paint_row_gutter(
        &self,
        buf: &mut Buffer,
        frame: &RowFrame,
        y: u16,
        line_idx: usize,
        first_row: bool,
    ) {
        // Current-line highlight (VS Code `editor.lineHighlightBackground`,
        // #248): a whisper of the accent behind every visual row of the
        // cursor's line, gutter through code. Focused editor only, and
        // hidden while a selection is active so the two washes never stack
        // — VS Code's `renderLineHighlight` behaves the same. Painted
        // first: spans that carry their own background (selection, search,
        // occurrences) patch over it, spans that don't inherit it.
        if self.focused && line_idx == self.cursor_row && frame.sel_norm.is_none() {
            buf.set_style(
                Rect::new(
                    frame.inner.x,
                    y,
                    frame.gutter_width + 1 + frame.text_width,
                    1,
                ),
                Style::default().bg(self.theme.current_line_bg()),
            );
        }
        // The line number shows once per logical line - on its first visual
        // row; wrapped continuation rows get a blank gutter, like VS Code.
        if first_row {
            let line_no = format!(
                "{:>width$} ",
                self.gutter_number(line_idx)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                width = frame.gutter_width as usize - 1
            );
            // Live Run coverage tints the number: green for a statement
            // that ran, a muted red for one that never did. Otherwise a
            // caret's line is brighter than the rest (VS Code's
            // `editorLineNumber.activeForeground`, #1576).
            let number_fg = match self
                .live_run
                .as_ref()
                .and_then(|v| v.coverage(line_idx, &self.lines[line_idx]))
            {
                Some(crate::live_run::Coverage::Ran) => self.theme.git_added(),
                Some(crate::live_run::Coverage::NeverRan) => {
                    self.theme.ui(Color::Rgb(0xa0, 0x55, 0x55))
                }
                None if self.is_caret_line(line_idx) => self.theme.line_number_active(),
                None => self.theme.line_number_fg(),
            };
            let gutter = Line::from(Span::styled(line_no, Style::default().fg(number_fg)));
            buf.set_line(frame.inner.x, y, &gutter, frame.gutter_width);
        } else {
            buf.set_line(
                frame.inner.x,
                y,
                &Line::from(" ".repeat(frame.gutter_width as usize)),
                frame.gutter_width,
            );
        }

        // The sign cell: the dedicated glyph margin at the far left
        // (`inner.x`), VS Code-style, so a glyph never crowds the line
        // number or the code. One glyph, on a logical line's first visual
        // row, chosen by `sign_glyph`, the rule a click answers by too.
        let sign_x = frame.inner.x;
        if let Some(glyph) = first_row.then(|| self.sign_glyph(line_idx)).flatten() {
            let (symbol, color) = match glyph {
                // The debugger's yellow stop arrow.
                SignGlyph::Stop => ("▶", Color::Rgb(0xff, 0xcc, 0x00)),
                // Hollow dimmed ring when the adapter could not bind it;
                // a red diamond for a conditional breakpoint; an amber
                // one for a logpoint; a solid red dot for a plain one.
                SignGlyph::Breakpoint(BreakpointMark::Unverified) => {
                    ("○", Color::Rgb(0x99, 0x99, 0x99))
                }
                SignGlyph::Breakpoint(BreakpointMark::Conditional) => {
                    ("◆", Color::Rgb(0xe5, 0x1c, 0x23))
                }
                SignGlyph::Breakpoint(BreakpointMark::Logpoint) => {
                    ("◆", Color::Rgb(0xe5, 0xc0, 0x7b))
                }
                SignGlyph::Breakpoint(BreakpointMark::Plain) => ("●", Color::Rgb(0xe5, 0x1c, 0x23)),
                // AI-stream stop button (croft pair): while a pilot
                // streams into this file, the row under its caret wears
                // a stop square in the badge's orange; clicking it (or
                // Cmd+K X) cancels and reverts the stream.
                SignGlyph::StreamStop => ("■", Color::Rgb(0xff, 0x9d, 0x2f)),
                // ⚑, a plain-Unicode flag: no Nerd Font needed.
                SignGlyph::Bookmark => ("\u{2691}", Color::Rgb(0x4f, 0xc1, 0xff)),
                // The play button beside a test fn's definition, VS
                // Code's run bead in its testing green: cod-play, the
                // Testing panel's run glyph.
                SignGlyph::TestPlay(_) => ("\u{eb2c}", Color::Rgb(0x73, 0xc9, 0x91)),
            };
            buf.set_string(sign_x, y, symbol, Style::default().fg(self.theme.ui(color)));
        }

        // Fold chevron: on a foldable header's first visual row, in the
        // second sign-margin column (`sign_x + 1`) so it never collides with
        // a breakpoint dot at `sign_x`. ▾ = expanded, ▸ = collapsed; the
        // collapsed one is brighter so hidden content is noticeable. Matches
        // the gutter's own line numbers. On a `surfaces` theme the
        // expanded one waits for the caret or the pointer, and the
        // collapsed one, always shown, wears the muted ink.
        if first_row && self.is_foldable(line_idx) {
            let collapsed = self.folded.contains(&line_idx);
            let glyph = if collapsed {
                let ink = crate::widgets::surface::tones(self.theme)
                    .map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());
                Some(("▸", ink))
            } else if !frame.surfaces || frame.gutter_hovered || self.is_caret_line(line_idx) {
                Some(("▾", self.theme.line_number_fg()))
            } else {
                None
            };
            if let Some((glyph, color)) = glyph {
                buf.set_string(sign_x + 1, y, glyph, Style::default().fg(color));
            }
        }

        // Git gutter: a thin coloured bar in the spacer cell between the
        // line number and the code (VS Code's dirty-diff lane), on a
        // logical line's first visual row only. Colour carries add/mod/del.
        // ponytail: one heavy bar in three colours; the bar's column also
        // shows deletions, which VS Code renders as a small triangle — fine
        // as a first cut, the colour already disambiguates.
        if first_row && let Some(mark) = self.git_marks.get(&line_idx) {
            let color = match mark {
                GitMark::Added => self.theme.git_added(),
                GitMark::Modified => self.theme.git_modified(),
                GitMark::Deleted => self.theme.git_deleted(),
            };
            // ┃ heavy vertical; ▎ left quarter block on a `surfaces`
            // theme, a crisp stripe on the cell edge rather than a line
            // through its middle.
            let bar = if frame.surfaces {
                "\u{258e}"
            } else {
                "\u{2503}"
            };
            buf.set_string(
                frame.inner.x + frame.gutter_width,
                y,
                bar,
                Style::default().fg(color),
            );
        }
        // Coverage lens (#263): the same lane, painted over the git bar
        // while a coverage report is shown. Green run, red never run,
        // amber run with a branch missed; dimmed once the file changed
        // after the run. `Coverage: Clear` gives the lane back to git.
        if first_row
            && let Some(lens) = self.coverage.as_ref()
            && let Some(cov) = lens.lines.get(&line_idx)
        {
            use crate::testing::coverage::LineCov;
            let (rgb, glyph) = match cov {
                LineCov::Covered => ((0x4e, 0xc9, 0x7a), "\u{258c}"),
                LineCov::Uncovered => ((0xe0, 0x55, 0x55), "\u{258c}"),
                LineCov::Partial => ((0xe0, 0xb0, 0x40), "\u{2596}"),
            };
            let (r, g, b) = if lens.stale {
                (rgb.0 / 2, rgb.1 / 2, rgb.2 / 2)
            } else {
                rgb
            };
            buf.set_string(
                frame.inner.x + frame.gutter_width,
                y,
                glyph,
                Style::default().fg(self.theme.ui(Color::Rgb(r, g, b))),
            );
        }
        // Provenance overlay (#349): the same lane, a thinner bar in the
        // seat's hue, painted over the git bar while the lens is on
        // because the user asked who typed the line, not whether it
        // differs from HEAD. A line with no record paints nothing: an
        // unknown line is never guessed at.
        if self.provenance_overlay
            && first_row
            && let Some(seat) = self.provenance.seat(line_idx)
        {
            let (r, g, b) = seat.hue();
            // ▎ left one-quarter block; ▏ one-eighth on a `surfaces`
            // theme, whose change bar already takes the quarter block.
            let bar = if frame.surfaces {
                "\u{258f}"
            } else {
                "\u{258e}"
            };
            buf.set_string(
                frame.inner.x + frame.gutter_width,
                y,
                bar,
                Style::default().fg(self.theme.ui(Color::Rgb(r, g, b))),
            );
        }
    }

    /// Paint the text of one visual row at screen row `y`: the segment
    /// `[row_start, row_end)` of `line_idx` with its syntax and semantic
    /// colours and inlay hints, then bracket colours, colour swatches,
    /// indentation guides and whitespace marks. Fills `row_cells` with the
    /// row's geometry for the overlays painted after it, and returns the
    /// line's length in characters and the row's width in cells.
    #[allow(clippy::too_many_arguments)]
    fn paint_row_text(
        &self,
        buf: &mut Buffer,
        frame: &RowFrame,
        row_cells: &mut RowCells,
        y: u16,
        line_idx: usize,
        row_start: usize,
        row_end: usize,
    ) -> (usize, u16) {
        // Per-row window: the segment [row_start, row_end). For non-wrap
        // this is exactly the horizontally-scrolled view; for wrap it is
        // one folded segment. The overlay painters treat `row_start` as
        // the scroll offset and `row_width` as the text width, so the
        // same code paints both modes.
        let raw = &self.lines[line_idx];
        let line_len = self.line_char_len(line_idx);
        let byte_start = byte_index_of_char(raw, row_start);
        let byte_end = byte_index_of_char(raw, row_end);
        // A wrapped row's width in CELLS, which is what the painters clip
        // by: its char count cut a row of wide characters in half.
        let row_origin = self.row_origin(raw, row_start);
        let row_width = {
            let cells: usize = cells_from(&raw[byte_start..byte_end], row_origin);
            cells
                .max(row_end - row_start)
                .min(frame.text_width as usize) as u16
        };
        let visible_raw = &raw[byte_start..byte_end];
        let seg_bytes = byte_end - byte_start;
        let empty: Vec<HiSpan> = Vec::new();
        let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
        // Paint the LSP semantic overlay over the tree-sitter base so
        // resolved symbols (parameters, etc.) win wherever the server
        // has an opinion, syntax fills the gaps (the "combined" model).
        let sem_spans = self.semantic_overlay.get(line_idx).unwrap_or(&empty);
        let merged = merge_overlay(line_spans, sem_spans);
        // The row's geometry: its text by display width (#873) and the
        // inlay hints anchored inside its window, as (anchor col,
        // display cells) pairs. A hint's label is spliced into the row
        // BEFORE the character at its anchor; every buffer column at or
        // past an anchor therefore paints `inlay_cells_before` cells
        // further right, and each overlay painter below translates its
        // columns through `row_cells` so highlights, underlines, and
        // carets stay glued to their glyphs.
        let hint_cap = row_end.min(line_len);
        row_cells.fill(
            visible_raw,
            row_start,
            row_origin,
            self.row_inlay_spans(line_idx)
                .iter()
                .filter(|(hc, _, _)| *hc >= row_start && *hc <= hint_cap)
                .map(|(hc, l, _)| (*hc, l.chars().count())),
        );
        if row_cells.hints.is_empty() {
            // Shift highlight spans to the row origin and clip them to the
            // segment so build_line_spans never slices past `visible_raw`.
            let shifted: Vec<HiSpan> = shift_spans_for_view(&merged, byte_start)
                .into_iter()
                .filter_map(|sp| sp.clipped(seg_bytes))
                .collect();
            let spans = build_line_spans(visible_raw, &shifted, row_origin);
            buf.set_line(frame.text_x, y, &Line::from(spans), row_width);
        } else {
            // Splice each hint label between the text segments it splits.
            // Style A (Zed's look): dim italic text, one shade quieter
            // than comments, no chip background.
            let hint_style = Style::default()
                .fg(self.theme.ignored_fg())
                .add_modifier(Modifier::ITALIC);
            let mut out: Vec<Span> = Vec::new();
            let mut from = row_start;
            for (hcol, label, swatch) in self
                .row_inlay_spans(line_idx)
                .iter()
                .filter(|(hc, _, _)| *hc >= row_start && *hc <= hint_cap)
            {
                if *hcol > from {
                    out.extend(inlay_text_segment(
                        raw,
                        &merged,
                        (row_start, row_origin),
                        from,
                        *hcol,
                    ));
                }
                // A color swatch paints in ITS color (#254); plain
                // hints keep the dim italic Zed look.
                let style = match swatch {
                    Some(c) => Style::default().fg(*c),
                    None => hint_style,
                };
                out.push(Span::styled(label.clone(), style));
                from = from.max(*hcol);
            }
            if row_end > from {
                out.extend(inlay_text_segment(
                    raw,
                    &merged,
                    (row_start, row_origin),
                    from,
                    row_end,
                ));
            }
            buf.set_line(frame.text_x, y, &Line::from(out), row_width);
        }

        // Bracket-pair colorization (#131): recolour each bracket cell by
        // its nesting depth (red for an unmatched closer). A foreground
        // override on the already-painted glyph, so selection / search /
        // occurrence layers below still lay their backgrounds over it —
        // and the find layer, painted later, wins outright like it does
        // over syntax colours. Columns translate through inlay cells and
        // the row window like every painter; wrap continuation segments
        // participate (brackets live anywhere in the line, unlike the
        // leading-whitespace guides).
        if self.show_bracket_colors
            && let Some(cols) = self.bracket_colors.get(line_idx)
        {
            for &(c, ci) in cols {
                let c = c as usize;
                if c < row_start {
                    continue;
                }
                let fg = if ci == UNEXPECTED_BRACKET {
                    self.theme.bracket_unexpected_fg()
                } else {
                    self.theme.bracket_pair_color(usize::from(ci))
                };
                if !paint_row_cells(buf, frame.text_x, y, row_width, row_cells, c, |s| s.fg(fg)) {
                    break;
                }
            }
        }

        // Inline color-literal decorations (VS Code's
        // `naumovs.color-highlight`, filling the "no colour-swatch
        // decorations" gap): tint each recognized `#hex` / `rgb()` /
        // `hsl()` literal's own cells with its parsed color, choosing a
        // black or white foreground for legibility. Painted after
        // bracket colorization so a color literal's own brackets (e.g.
        // `rgb(...)`) still read against the tint, and before the
        // overlays below (selection, search) so those still win over it.
        if self.show_color_swatches
            && let Some(hits) = self.color_swatches.get(line_idx)
        {
            for &(start_c, end_c, bg, fg) in hits {
                if end_c <= row_start {
                    continue;
                }
                let from = start_c.max(row_start);
                for c in from..end_c {
                    let tint = |s: Style| s.bg(bg).fg(fg);
                    if !paint_row_cells(buf, frame.text_x, y, row_width, row_cells, c, tint) {
                        break;
                    }
                }
            }
        }

        // Indentation guides (VS Code `editor.guides.indentation`): a dim
        // │ at each indent-unit column of the line's leading whitespace,
        // with the cursor block's guide highlighted. Painted into blank
        // cells only, right after the text, so text always wins and the
        // overlays below (selection band, conflict tints) lay their
        // backgrounds over the glyph. A wrap continuation row carries no
        // leading whitespace, so only a line's first segment draws guides;
        // in the flat path `row_start` is the horizontal scroll and
        // translates the columns like every other painter here.
        if self.show_indent_guides && (!frame.wrap || row_start == 0) {
            let (guide_width, guide_src) = self.guide_indent(line_idx);
            // A blank line's guides sit where its neighbour's indentation
            // is drawn, tabs and all, not one cell per level.
            let src_cells;
            let geometry = if guide_src == line_idx {
                &*row_cells
            } else {
                src_cells = RowCells::of_line_at(
                    &self.lines[guide_src],
                    row_start,
                    self.row_origin(&self.lines[guide_src], row_start),
                );
                &src_cells
            };
            for c in (0..guide_width).step_by(frame.guide_step) {
                if c < row_start {
                    continue;
                }
                let col = geometry.cell(c);
                if col >= usize::from(row_width) {
                    break;
                }
                let cell = &mut buf[(frame.text_x + col as u16, y)];
                if cell.symbol() != " " {
                    continue;
                }
                let fg = match frame.active_guide {
                    Some((ac, lo, hi)) if ac == c && (lo..=hi).contains(&line_idx) => {
                        frame.guide_active_fg
                    }
                    _ => frame.guide_fg,
                };
                cell.set_symbol("\u{2502}");
                // Not italic even over a comment: a slanted guide looks
                // broken, and the text's modifier is not the guide's.
                let style = cell.style().fg(fg).remove_modifier(Modifier::ITALIC);
                cell.set_style(style);
            }
        }

        // Render whitespace (#133): swap space/tab cells to `·`/`→` in a
        // dim theme colour — across the whole row in All mode, inside the
        // primary and secondary-caret selections in Selection mode (the
        // VS Code default). Painted after the indent guides so the glyph
        // wins the cell: it reports a real character where the guide only
        // decorates, and the selection band below touches backgrounds
        // only, so the glyph rides the band like text does.
        if self.whitespace_mode != WhitespaceMode::None {
            let ws_fg = self.theme.whitespace_fg();
            let mut ws_spans: Vec<(usize, usize)> = Vec::new();
            match self.whitespace_mode {
                WhitespaceMode::All => ws_spans.push((0, line_len)),
                WhitespaceMode::Selection => {
                    if let Some(((sr, sc), (er, ec))) = frame.sel_norm
                        && line_idx >= sr
                        && line_idx <= er
                    {
                        let s = if line_idx == sr { sc } else { 0 };
                        let e = if line_idx == er { ec } else { line_len };
                        if e > s {
                            ws_spans.push((s, e));
                        }
                    }
                    for caret in &self.carets {
                        let ((cr0, cc0), (cr1, cc1)) = caret.normalised();
                        if line_idx >= cr0 && line_idx <= cr1 {
                            let s = if line_idx == cr0 { cc0 } else { 0 };
                            let e = if line_idx == cr1 { cc1 } else { line_len };
                            if e > s {
                                ws_spans.push((s, e));
                            }
                        }
                    }
                }
                WhitespaceMode::None => {}
            }
            for (s, e) in ws_spans {
                let from = s.max(row_start);
                let to = e.min(row_end).min(line_len);
                if to <= from {
                    continue;
                }
                for (ci, ch) in raw.chars().enumerate().skip(from).take(to - from) {
                    let glyph = match ch {
                        ' ' => "\u{b7}",
                        '\t' => "\u{2192}",
                        _ => continue,
                    };
                    let col = row_cells.cell(ci);
                    if col >= usize::from(row_width) {
                        break;
                    }
                    let cell = &mut buf[(frame.text_x + col as u16, y)];
                    cell.set_symbol(glyph);
                    // The glyph marks whitespace, not comment text.
                    let style = cell.style().fg(ws_fg).remove_modifier(Modifier::ITALIC);
                    cell.set_style(style);
                }
            }
        }
        (line_len, row_width)
    }

    /// Paint logical line `line_idx`'s syntax/semantic-highlighted text at row
    /// `y` from column 0 (no horizontal scroll), clipped to `width`. Used by
    /// the sticky-scroll bar to render pinned scope headers. Assumes the row
    /// background is already filled by the caller.
    pub(super) fn paint_highlighted_line(
        &self,
        buf: &mut Buffer,
        text_x: u16,
        y: u16,
        width: u16,
        line_idx: usize,
    ) {
        let Some(raw) = self.lines.get(line_idx) else {
            return;
        };
        let empty: Vec<HiSpan> = Vec::new();
        let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
        let sem_spans = self.semantic_overlay.get(line_idx).unwrap_or(&empty);
        let merged = merge_overlay(line_spans, sem_spans);
        let raw_bytes = raw.len();
        let clamped: Vec<HiSpan> = merged
            .into_iter()
            .filter_map(|sp| sp.clipped(raw_bytes))
            .collect();
        let spans = build_line_spans(raw, &clamped, 0);
        buf.set_line(text_x, y, &Line::from(spans), width);
    }

    /// If `(col, row)` lands on a pinned sticky-scroll header, return the
    /// logical line to jump to. `None` when the row is not a sticky header.
    pub fn sticky_line_at(&self, col: u16, row: u16) -> Option<u32> {
        if col < self.last_inner.x || col >= self.last_inner.x + self.last_inner.width {
            return None;
        }
        self.sticky_click_rows
            .iter()
            .find(|&&(y, _)| y == row)
            .map(|&(_, line)| line)
    }

    /// Absolute (column, row) of the editor's cursor in screen coordinates,
    /// or `None` if the cursor is outside the visible viewport. Used by
    /// `App::render` to position the host terminal's hardware caret.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if self.last_inner.height == 0 {
            return None;
        }
        self.sync_tab_cells();
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if self.wrap_enabled() {
            // Find the visual row holding the cursor in the layout the last
            // render captured (a logical line spans several wrapped rows). A
            // column on a wrap boundary belongs to the next row's start.
            let idx = self.last_wrap_rows.iter().position(|r| {
                matches!(r, VisRow::Text { line, start, end }
                    if *line == self.cursor_row
                        && self.cursor_col >= *start
                        && (self.cursor_col < *end
                            || (self.cursor_col == *end
                                && *end == self.line_char_len(*line))))
            })?;
            let (_, start, _) = self.text_row(idx)?;
            // The caret is valid on every cell of the segment AND one past
            // its last cell (end of line / blank line — where the user is
            // about to type). Bail only when the cell would fall outside the
            // text column: past the pane edge or onto the vertical scrollbar.
            // In cells, as the row was painted: wide characters before the
            // caret push it right by two each.
            let visible_col = RowCells::of_line_at(&self.lines[self.cursor_row], start, 0)
                .text_before(self.cursor_col);
            let x = text_x as usize + visible_col;
            let right = (self.last_inner.x + self.last_inner.width)
                .saturating_sub(u16::from(self.last_scrollbar.width > 0))
                as usize;
            if x >= right {
                return None;
            }
            return Some((x as u16, self.last_inner.y + idx as u16));
        }
        // Non-wrap: one row per line, horizontally scrolled by `scroll_col`;
        // comment boxes shift the rows below them, so map through the
        // painted layout when one exists.
        if self.cursor_row < self.scroll {
            return None;
        }
        let row_in_view = if self.last_wrap_rows.is_empty() {
            self.cursor_row - self.scroll
        } else {
            self.last_wrap_rows
                .iter()
                .position(|r| matches!(r, VisRow::Text { line, .. } if *line == self.cursor_row))?
        };
        if (row_in_view as u16) >= self.last_inner.height {
            return None;
        }
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2 + u16::from(self.last_scrollbar.width > 0));
        if text_width == 0 || self.cursor_col < self.scroll_col {
            return None;
        }
        // Inlay-hint cells before the caret shift it right. Strictly-before
        // (`hc < cursor_col`, not `<=`): a caret AT a hint's anchor sits left
        // of the hint, like VS Code, so typing there pushes the hint along.
        let extra = self.inlay_cells_before_cursor(self.cursor_row, self.scroll_col);
        let visible_col = self
            .lines
            .get(self.cursor_row)
            .map_or(self.cursor_col - self.scroll_col, |l| {
                RowCells::of_line(l, self.scroll_col).text_before(self.cursor_col)
            })
            + extra;
        if visible_col >= text_width as usize {
            return None;
        }
        Some((
            text_x + visible_col as u16,
            self.last_inner.y + row_in_view as u16,
        ))
    }

    /// Hint cells spliced between `scroll_col` and the caret on `line`
    /// (anchors strictly before `cursor_col`, at or past `scroll_col`).
    pub(super) fn inlay_cells_before_cursor(&self, line: usize, scroll_col: usize) -> usize {
        self.row_inlay_spans(line)
            .iter()
            .filter(|(hc, _, _)| *hc >= scroll_col && *hc < self.cursor_col)
            .map(|(_, l, _)| l.chars().count())
            .sum()
    }
}

/// Overpaint every match of `needle` in `raw_line` with the search-match
/// style, honouring `opts` (case-sensitive / whole-word / regex). Delegates
/// to `split_for_highlight` so the highlight rule stays 1:1 with the
/// search-engine matcher; column conversion uses `chars().count()` over
/// the byte prefix to stay correct for Unicode.
/// Hint cells inserted at or before buffer column `c` on this row: the
/// display-column shift every overlay painter applies past inlay hints.
/// `hints` is the row's visible (anchor col, label cells) set.
pub(super) fn inlay_cells_before(hints: &[(usize, usize)], c: usize) -> usize {
    hints
        .iter()
        .filter(|(hc, _)| *hc <= c)
        .map(|(_, n)| n)
        .sum()
}

/// Hint cells strictly before buffer column `c`: the shift for a CARET at
/// `c`, which sits left of a hint anchored exactly there (a character at
/// `c` sits right of it — that is [`inlay_cells_before`]).
pub(super) fn inlay_cells_strictly_before(hints: &[(usize, usize)], c: usize) -> usize {
    hints.iter().filter(|(hc, _)| *hc < c).map(|(_, n)| n).sum()
}

/// Where buffer columns land on one painted text row (#873): the text is
/// drawn by display width, a CJK character or most emoji taking two cells
/// ([`char_cells`]), with inlay-hint labels spliced between characters.
/// Every overlay laid over the text maps its columns through this, or it
/// lands one cell left of its glyph per wide character before it.
#[derive(Debug, Default)]
pub(super) struct RowCells {
    /// Buffer column of the row's first character: the horizontal scroll, or
    /// a wrapped segment's start.
    start: usize,
    /// Cells of the row's text before each of its characters, plus one entry
    /// for where that text ends. Empty means one cell per character.
    before: Vec<usize>,
    /// The row's inlay hints as (anchor column, label cells).
    hints: Vec<(usize, usize)>,
}

impl RowCells {
    /// The geometry of a row showing `line` from buffer column `start`, with
    /// no inlay hints.
    pub(super) fn of_line(line: &str, start: usize) -> Self {
        Self::of_line_at(line, start, cells_before(line, start))
    }

    /// [`Self::of_line`] for a row whose first cell is display cell `origin`
    /// of its tab stops: 0 for a wrapped row, the cells scrolled past for a
    /// horizontally scrolled one.
    pub(super) fn of_line_at(line: &str, start: usize, origin: usize) -> Self {
        let mut row = Self::default();
        row.fill(&line[byte_index_of_char(line, start)..], start, origin, []);
        row
    }

    /// Refill for a row painting `text` from buffer column `start` with
    /// `hints` spliced in, reusing the allocations.
    pub(super) fn fill(
        &mut self,
        text: &str,
        start: usize,
        origin: usize,
        hints: impl IntoIterator<Item = (usize, usize)>,
    ) {
        self.start = start;
        self.before.clear();
        self.before.push(0);
        let mut cells = 0;
        // Tab stops count from the row's left edge, `origin` cells before its
        // first character. A grapheme cluster's first character takes the
        // whole cluster's cells and the rest none ([`char_cells_from`]).
        for (_, w) in char_cells_from(text, origin) {
            cells += w;
            self.before.push(cells);
        }
        self.hints.clear();
        self.hints.extend(hints);
    }

    /// Cells of the row's own text before buffer column `col`: none at or
    /// before the row's start, and one per column past the end of its text,
    /// where the selection's newline cell sits.
    pub(super) fn text_before(&self, col: usize) -> usize {
        let i = col.saturating_sub(self.start);
        match self.before.get(i) {
            Some(&cells) => cells,
            None => {
                let end = self.before.len().saturating_sub(1);
                self.before.last().copied().unwrap_or(0) + (i - end)
            }
        }
    }

    /// The first cell of the character at `col`: right of a hint anchored at
    /// or before it.
    pub(super) fn cell(&self, col: usize) -> usize {
        self.text_before(col) + inlay_cells_before(&self.hints, col)
    }

    /// The cell a caret at `col` sits on: left of a hint anchored exactly
    /// there, like the primary caret (`cursor_screen_pos`), marking the cell
    /// its typed character will land on.
    pub(super) fn caret(&self, col: usize) -> usize {
        self.text_before(col) + inlay_cells_strictly_before(&self.hints, col)
    }

    /// The cell just past a range ending before `col`: shifted by the hints
    /// before its LAST character, so a hint anchored at `col` stays outside.
    pub(super) fn end(&self, col: usize) -> usize {
        self.text_before(col) + inlay_cells_before(&self.hints, col.saturating_sub(1))
    }

    /// Cells the character at `col` takes: two for a wide one, none for a
    /// combining mark, one past the end of the text.
    pub(super) fn width(&self, col: usize) -> usize {
        self.text_before(col + 1) - self.text_before(col)
    }
}

/// Restyle every cell the character at buffer column `col` occupies on a
/// text row: [`paint_log_cells`] for the editor's row geometry. Returns false
/// once the row's right edge is reached, so a caller walking columns can stop.
pub(super) fn paint_row_cells(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    row: &RowCells,
    col: usize,
    restyle: impl Fn(Style) -> Style,
) -> bool {
    let first = row.cell(col);
    let width = row.width(col);
    // A character that does not fit WHOLE was never painted (`set_line`
    // stops before it), so its band would colour an empty cell.
    if first >= usize::from(text_width) || first + width > usize::from(text_width) {
        return false;
    }
    for x in first..first + width {
        let cell = &mut buf[(text_x + x as u16, y)];
        cell.set_style(restyle(cell.style()));
    }
    true
}

/// The syntax-highlighted spans of `raw`'s character range `[from, to)`,
/// clipped out of the line's merged highlight spans. Paints the text
/// segments between spliced inlay-hint labels.
pub(super) fn inlay_text_segment<'a>(
    raw: &'a str,
    merged: &[HiSpan],
    row: (usize, usize),
    from: usize,
    to: usize,
) -> Vec<Span<'a>> {
    let byte_from = byte_index_of_char(raw, from);
    let byte_to = byte_index_of_char(raw, to);
    let seg = &raw[byte_from..byte_to];
    let seg_bytes = byte_to - byte_from;
    let shifted: Vec<HiSpan> = shift_spans_for_view(merged, byte_from)
        .into_iter()
        .filter_map(|sp| sp.clipped(seg_bytes))
        .collect();
    // Tab stops count from the row's left edge: its first char is `row.0`,
    // which sits at display cell `row.1` (hint labels do not count).
    let row_start = byte_index_of_char(raw, row.0).min(byte_from);
    let origin = row.1 + cells_from(&raw[row_start..byte_from], row.1);
    build_line_spans(seg, &shifted, origin)
}

/// The (inactive, active) find-highlight styles, shared by the editor's
/// highlighter and the rendered log's so the two tabs read the same. A
/// boxed theme's solid blocks; Croft Loch tints each cell instead
/// ([`crate::widgets::surface::match_style`]).
pub(super) fn search_highlight_styles(theme: crate::theme::Theme) -> (Style, Style) {
    let inactive = Style::default()
        .fg(Color::Black)
        .bg(theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
        .add_modifier(Modifier::BOLD);
    let active = Style::default()
        .fg(Color::Black)
        .bg(theme.ui(Color::Rgb(0xff, 0x8c, 0x2a)))
        .add_modifier(Modifier::BOLD);
    (inactive, active)
}

// Render helper: each argument is an independent painting input (buffer,
// geometry, text, styling); bundling them into a struct would add indirection
// without improving clarity.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_search_highlight(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    raw_line: &str,
    needle: &str,
    opts: crate::widgets::search::SearchOpts,
    row: &RowCells,
    active_match_on_line: Option<(usize, usize)>,
    cells: Option<&crate::cell_map::CellMap>,
    window: Option<(usize, usize)>,
    theme: crate::theme::Theme,
) {
    if needle.is_empty() {
        return;
    }
    let (inactive_style, active_style) = search_highlight_styles(theme);
    // Croft Loch tints each matched cell's own background amber and keeps
    // its syntax ink where it stays legible ([`surface::match_style`]); a
    // boxed theme stamps the solid find style over it.
    let tones = crate::widgets::surface::tones(theme);
    let restyle = |active: bool| {
        move |s: Style| match tones {
            Some(t) => crate::widgets::surface::match_style(s, t, theme, active),
            None if active => active_style,
            None => inactive_style,
        }
    };
    let segments = crate::widgets::search::split_for_highlight(raw_line, needle, opts);
    // `abs_col` tracks the absolute character index in the original line.
    // Columns before the row's start are scrolled out; the rest map
    // through the row's geometry and are painted inside `text_width`.
    let mut abs_col: usize = 0;
    for (chunk, is_match) in segments {
        let chunk_cols = chunk.chars().count();
        // Find in Selection (#1577): only matches wholly inside the window.
        let in_window = window.is_none_or(|(lo, hi)| abs_col >= lo && abs_col + chunk_cols <= hi);
        if is_match && in_window {
            let is_active =
                active_match_on_line.is_some_and(|(c, l)| c == abs_col && l == chunk_cols);
            let style = restyle(is_active);
            for c in 0..chunk_cols {
                let absolute = abs_col + c;
                if absolute < row.start {
                    continue;
                }
                // A rendered log paints by ratatui's grapheme rules (#404),
                // so its band goes through the same cell map the text did;
                // the editor's text path goes through its row geometry.
                if let Some(cells) = cells {
                    if !paint_log_cells(buf, text_x, text_x + text_width, y, cells, absolute, style)
                    {
                        break;
                    }
                    continue;
                }
                if !paint_row_cells(buf, text_x, y, text_width, row, absolute, style) {
                    break;
                }
            }
        }
        abs_col = abs_col.saturating_add(chunk_cols);
        if row.cell(abs_col) >= usize::from(text_width) {
            break;
        }
    }
}

/// Paint VS Code-style "selection highlight": a muted blue box over every
/// occurrence of `needle` on this line. Matches the literal selected text
/// case-sensitively (like VS Code's `editor.selectionHighlight`). Only the
/// background is repainted, so syntax foreground colours show through. The
/// active selection itself is overpainted afterwards by
/// `paint_selection_band` with a brighter blue, giving the two-tone look.
// Render helper: same independent-inputs shape as paint_search_highlight.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_selection_occurrences(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    raw_line: &str,
    needle: &str,
    row: &RowCells,
    theme: crate::theme::Theme,
) {
    if needle.is_empty() {
        return;
    }
    let bg = theme.ui(Color::Rgb(0x37, 0x61, 0x8e));
    let needle_cols = needle.chars().count();
    let mut search_from = 0usize;
    while let Some(rel) = raw_line[search_from..].find(needle) {
        let byte_start = search_from + rel;
        let char_start = raw_line[..byte_start].chars().count();
        for absolute in char_start.max(row.start)..char_start + needle_cols {
            if !paint_row_cells(buf, text_x, y, text_width, row, absolute, |s| s.bg(bg)) {
                break;
            }
        }
        search_from = byte_start + needle.len();
    }
}

/// Paint a software block cursor for a secondary multi-cursor caret on cell
/// `col` of row `y`'s text. Uses a reversed bar so it reads as a caret
/// distinct from the host terminal's hardware caret on the primary cursor.
pub(super) fn paint_block_cursor(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col: usize,
    theme: crate::theme::Theme,
) {
    if col >= usize::from(text_width) {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    // The glyph under the block takes the canvas on a `surfaces` theme
    // rather than the host palette's named black.
    let fg = crate::widgets::surface::tones(theme).map_or(Color::Black, |t| t.canvas());
    cell.set_style(
        Style::default()
            .fg(fg)
            .bg(theme.secondary_caret_bg())
            .add_modifier(Modifier::BOLD),
    );
}

/// Paint one collaborator's caret cell in their color (multiplayer ghost
/// caret): same shape and clipping as `paint_block_cursor`, but tinted per
/// participant so everyone can see where the others are.
pub(super) fn paint_ghost_caret(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    col: usize,
    color: Color,
) {
    if col >= usize::from(text_width) {
        return;
    }
    let cell = &mut buf[(text_x + col as u16, y)];
    cell.set_style(
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
}

/// Tint every cell of one text row, keeping the glyphs and their colours.
pub(super) fn paint_full_row_bg(buf: &mut Buffer, text_x: u16, y: u16, text_width: u16, bg: Color) {
    for col in 0..text_width {
        let x = text_x + col;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

pub(super) fn paint_selection_band(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    theme: crate::theme::Theme,
) {
    let bg = theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

/// Paint a coloured underline across `[start_char, end_char)` of a rendered
/// row to mark an LSP diagnostic. The colour encodes severity, matching VS
/// Code's palette (red error, yellow warning, blue info, faint grey for a
/// hint's short marker). Uses the
/// terminal's separate underline colour so the glyph's foreground (its syntax
/// or semantic colour) is left untouched, the way VS Code's squiggle sits
/// under unchanged text.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_diagnostic_underline(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    severity: crate::lsp::manager::DiagnosticSeverity,
    theme: crate::theme::Theme,
) {
    use crate::lsp::manager::DiagnosticSeverity;
    let color = match severity {
        DiagnosticSeverity::Error => theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
        DiagnosticSeverity::Warning => theme.ui(Color::Rgb(0xcc, 0xa7, 0x00)),
        DiagnosticSeverity::Information => theme.ui(Color::Rgb(0x3b, 0x9e, 0xff)),
        DiagnosticSeverity::Hint => theme.ui(Color::Rgb(0x80, 0x80, 0x80)),
    };
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(
            cell.style()
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(color),
        );
    }
}

/// A diagnostic's message for its hover (#1540): VS Code's `source(code)`
/// after it, and the rule's documentation link below, when the server sent
/// them.
pub(super) fn message_with_code(d: &crate::lsp::manager::Diagnostic) -> String {
    let Some(code) = &d.code else {
        return d.message.clone();
    };
    let mut text = format!("{}  {}", d.message, code.label(""));
    if let Some(href) = &code.href {
        text.push('\n');
        text.push_str(href);
    }
    text
}

/// Draw a tagged diagnostic's look across `[start_char, end_char)` of a
/// rendered row (#1522): Unnecessary code (unused, unreachable) faded, as VS
/// Code draws it at reduced opacity, and Deprecated code struck through. The
/// glyphs keep their syntax colour.
pub(super) fn paint_diagnostic_tags(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
    tags: crate::lsp::manager::DiagnosticTags,
) {
    let mut look = Modifier::empty();
    if tags.unnecessary {
        look |= Modifier::DIM;
    }
    if tags.deprecated {
        look |= Modifier::CROSSED_OUT;
    }
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    for col in s..e {
        let cell = &mut buf[(text_x + col as u16, y)];
        cell.set_style(cell.style().add_modifier(look));
    }
}
