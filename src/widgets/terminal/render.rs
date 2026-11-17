//! Drawing the pane: each cell's text and colours, selections, overlays, decorations and
//! notes.

use super::*;

/// Map an alacritty cell color to ratatui through the theme's 16-color ANSI
/// palette: Named colors and Indexed 0-15 resolve to the palette's RGB (so a
/// pane renders the same on every host terminal, the way VS Code owns its
/// terminal palette); true-color cells pass through; higher indexes keep the
/// standard 256-color cube the host renders.
pub(super) fn ansi_to_ratatui(c: AnsiColor, palette: &[(u8, u8, u8); 16]) -> Option<Color> {
    match c {
        AnsiColor::Spec(rgb) => Some(Color::Rgb(rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) if i < 16 => Some(pal(palette, i as usize)),
        AnsiColor::Indexed(i) => Some(Color::Indexed(i)),
        AnsiColor::Named(named) => named_to_ratatui(named, palette),
    }
}

pub(super) fn pal(palette: &[(u8, u8, u8); 16], i: usize) -> Color {
    let (r, g, b) = palette[i];
    Color::Rgb(r, g, b)
}

pub(super) fn named_to_ratatui(n: NamedColor, palette: &[(u8, u8, u8); 16]) -> Option<Color> {
    use NamedColor::*;
    match n {
        Foreground | Background | Cursor | DimForeground => None,
        Black | DimBlack => Some(pal(palette, 0)),
        Red | DimRed => Some(pal(palette, 1)),
        Green | DimGreen => Some(pal(palette, 2)),
        Yellow | DimYellow => Some(pal(palette, 3)),
        Blue | DimBlue => Some(pal(palette, 4)),
        Magenta | DimMagenta => Some(pal(palette, 5)),
        Cyan | DimCyan => Some(pal(palette, 6)),
        White | DimWhite => Some(pal(palette, 7)),
        BrightBlack => Some(pal(palette, 8)),
        BrightRed => Some(pal(palette, 9)),
        BrightGreen => Some(pal(palette, 10)),
        BrightYellow => Some(pal(palette, 11)),
        BrightBlue => Some(pal(palette, 12)),
        BrightMagenta => Some(pal(palette, 13)),
        BrightCyan => Some(pal(palette, 14)),
        BrightWhite => Some(pal(palette, 15)),
        BrightForeground => None,
    }
}

impl PtyTerminal {
    /// The ink of a command that went well (a finished command's dot, a
    /// normal OSC 9;4 gauge): VS Code's blue on a boxed theme; on Croft Loch
    /// the accent eased a quarter of the way into the canvas, so success
    /// reads as on-palette and quieter than a failure's red.
    pub(super) fn quiet_ok_ink(&self) -> Color {
        match crate::widgets::surface::tones(self.theme) {
            Some(t) => {
                crate::gradient::rgb_color(crate::gradient::lerp_rgb(t.accent, t.canvas, 0.25))
            }
            None => self.theme.ui(Color::Rgb(0x1b, 0x81, 0xa8)),
        }
    }
}

impl Widget for &mut PtyTerminal {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // A host-accent rule outranks the focus color: a production pane
        // must read as dangerous whether or not it holds focus.
        let block_style = if let Some((r, g, b)) = self.accent {
            Style::default().fg(Color::Rgb(r, g, b))
        } else if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // The panel group's tab strip already labels this region "TERMINAL", so
        // each pane stays titleless — a bare bordered box, matching VS Code's
        // unlabelled split terminal panes. The border alone frames the pane.
        //
        // The focused pane draws its box in heavy line-drawing glyphs
        // (┏━┓) against the unfocused pane's light ones (┌─┐), so which
        // pane has the keyboard reads at a glance and not only from the
        // border's colour (#470).
        //
        // Weight rather than a second ring: the issue asks for "twice as
        // thick", and a terminal cell is the smallest unit there is, so a
        // literally doubled border means a second box one cell inside the
        // first -- costing the shell a row and a column on every side of
        // every pane. Heavy glyphs give the emphasis for free, which is
        // what iTerm2 and WezTerm do for the same reason.
        //
        // Croft Loch draws no box: the frame is padding on the canvas, every
        // pane's top edge is a hairline under the tab strip, and the focused
        // pane's carries the accent lip. A host accent keeps a full box
        // there too, because a production pane must read as dangerous at a
        // glance.
        let surfaces = self.theme.surfaces().is_some() && self.accent.is_none();
        let block = if surfaces {
            crate::widgets::surface::pane_block(self.theme.editor_bg())
        } else {
            Block::default()
                .borders(Borders::ALL)
                .border_type(if self.focused {
                    ratatui::widgets::BorderType::Thick
                } else {
                    ratatui::widgets::BorderType::Plain
                })
                .border_style(block_style)
        };
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box).
        if surfaces {
            crate::widgets::surface::paint_pane_top(buf, area, self.theme, self.focused);
        } else if self.focused && self.focus_gradient && self.accent.is_none() {
            crate::gradient::paint_gradient_box_heavy(buf, area);
        }
        self.last_area = area;
        self.last_inner = inner;
        // Re-anchor the selection / copy-cursor to the content they were
        // made on before painting, so the highlight scrolls WITH streaming
        // output instead of sitting at fixed screen rows while text slides
        // underneath (the Claude Code drag-select bug).
        self.rebase_selection();
        // (is_block, bounds): block selections rectangle-test each cell,
        // linear ones use the row-major span. A dormant alt-screen
        // selection paints nothing — its content is scrolled out of the
        // app's view and the frozen coordinates sit over unrelated text; a
        // partially covered one paints only its surviving rows.
        let dormant = self.alt_sel.as_ref().is_some_and(|a| a.dormant);
        let sel_clip = self.alt_sel.as_ref().and_then(|a| a.visible.clone());
        let sel_paint = self.selection.filter(|_| !dormant).map(|s| {
            if s.block {
                (true, s.block_bounds())
            } else {
                (false, s.normalised())
            }
        });

        let cols = inner.width;
        let rows = inner.height;
        self.resize(cols, rows);

        // Snapshot before taking the term lock: command_decorations locks
        // the term itself and FairMutex is not reentrant.
        let decorations = self.command_decorations();
        // Trigger set snapshot for the highlight pass (cheap Arc clone).
        let trigger_set = self.triggers.lock().unwrap().clone();

        let term = self.term.lock();
        // Annotation spans at their current grid lines, translated by the
        // scroll clock (same anchor as selections — history growth
        // saturates and froze these in long-lived panes). None while an
        // alt-screen app owns the viewport: the spans name primary-screen
        // rows, and painting them over vim/htop content is chart junk.
        let ann_spans: Vec<(i32, u16, u16)> = if term.mode().contains(TermMode::ALT_SCREEN) {
            Vec::new()
        } else {
            let ann_now = self.clock_now(&term);
            self.annotations
                .iter()
                .map(|a| (a.line_rec - (ann_now - a.clock_rec) as i32, a.start, a.len))
                .collect()
        };
        let display_offset = term.grid().display_offset();
        let cursor_visible = term.mode().contains(TermMode::SHOW_CURSOR) && self.focused;
        let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
        let cursor_point = term.grid().cursor.point;
        // The cursor is in absolute grid coords (Line is 0..rows in alt
        // screen, can wander into negative scrollback in normal screen).
        // Convert to viewport row by adding display_offset (>0 when the
        // user has scrolled up into history).
        let cursor_row_in_viewport = if alt_screen {
            cursor_point.line.0
        } else {
            cursor_point.line.0 + display_offset as i32
        };
        let cursor_col_in_viewport = cursor_point.column.0 as i32;

        // Quick-select hint drift: lines were captured at the set's clock
        // reading; re-base them to now so labels follow their matches
        // through streaming output (the annotation translation, one read
        // per frame).
        let hint_drift: i32 = self
            .hints
            .as_ref()
            .map_or(0, |(c0, _)| (self.clock_now(&term) - c0) as i32);

        // The active find match, re-based to the current clock the same way
        // (its stored line drifts by exactly the rows scrolled since it was
        // anchored, so the bright cell stays glued to its text).
        let active_match: Option<(i32, usize, usize)> = self
            .current_match
            .map(|(c0, l, c, n)| (l - (self.clock_now(&term) - c0) as i32, c, n));

        let reveal_redactions = self.reveal_redactions;
        let mut redacted_spans = 0usize;
        // Croft Loch's inks and fills, looked up once per frame for the find
        // highlight's amber tint rather than once per cell.
        let tones = crate::widgets::surface::tones(self.theme);
        for y in 0..rows {
            // Find matches on this row once, then paint them per cell below.
            // Match positions are char indices in the spacer-skipped row text
            // (the same text the find bar searched); the colmap translates
            // them back to grid columns. 0 = no match, 1 = match, 2 = active.
            let row_line_idx = (y as i32) - (display_offset as i32);
            //
            // Matches are found on the logical line, as the find bar counts
            // them, and each paints the part that falls on this row: a match
            // the soft wrap cut in two lights up on both rows (#1275).
            let row_paint: Option<Vec<u8>> = self.search_needle.as_deref().map(|needle| {
                let (text, colmap) = row_text_and_cols(&term, row_line_idx);
                let (logical, offset, start) = logical_row_span(&term, row_line_idx);
                let row_len = text.chars().count();
                let mut paint = vec![0u8; cols as usize];
                for (mc, ml) in
                    crate::widgets::editor_find::line_matches(&logical, self.search_opts, needle)
                {
                    let (lo, hi) = (mc.max(offset), (mc + ml).min(offset + row_len));
                    if lo >= hi {
                        continue;
                    }
                    let active = active_match == Some((start, mc, ml));
                    for k in lo - offset..hi - offset {
                        if let Some(&col) = colmap.get(k) {
                            paint[col] = if active { 2 } else { 1 };
                        }
                    }
                }
                paint
            });
            // Quick-select hints on this row: 1 = match span, 2 = label cell
            // (the char to draw rides in the parallel vec). Char indices go
            // through the same colmap as the find highlight.
            let hint_paint: Option<(Vec<u8>, Vec<char>)> =
                self.hints.as_ref().and_then(|(_, hints)| {
                    let row_hints: Vec<&HintSpan> = hints
                        .iter()
                        .filter(|h| h.line - hint_drift == row_line_idx)
                        .collect();
                    if row_hints.is_empty() {
                        return None;
                    }
                    let (_text, colmap) = row_text_and_cols(&term, row_line_idx);
                    let mut span = vec![0u8; cols as usize];
                    let mut label = vec!['\0'; cols as usize];
                    for h in row_hints {
                        for k in h.start..h.start + h.len {
                            if let Some(&col) = colmap.get(k) {
                                span[col] = 1;
                            }
                        }
                        for (j, c) in h.label.chars().skip(h.typed).enumerate() {
                            if let Some(&col) = colmap.get(h.start + j) {
                                span[col] = 2;
                                label[col] = c;
                            }
                        }
                    }
                    Some((span, label))
                });
            // Trigger highlights on this row: per-cell fg/bg from the first
            // matching highlight trigger. Painted under the find highlight
            // and quick-select labels, which both win.
            let trig_paint: Option<TrigRowPaint> = trigger_set.has_highlights().then(|| {
                let (text, colmap) = row_text_and_cols(&term, row_line_idx);
                let mut paint: TrigRowPaint = vec![None; cols as usize];
                for s in crate::triggers::highlight_spans(&text, &trigger_set) {
                    let fg = s.fg.map(|(r, g, b)| Color::Rgb(r, g, b));
                    let bg = s.bg.map(|(r, g, b)| Color::Rgb(r, g, b));
                    for k in s.start..s.start + s.len {
                        if let Some(&col) = colmap.get(k) {
                            paint[col] = Some(TrigCell {
                                fg,
                                bg,
                                mask: false,
                            });
                        }
                    }
                }
                // Redaction (#360) is paint-only too: the grid keeps the
                // real text, the cell shows a mask glyph. Counted per span
                // for the status chip; nothing is masked while revealing.
                if !reveal_redactions {
                    // Over the logical line: a secret the soft wrap cut in
                    // two matches on neither row alone.
                    let (logical, offset) = logical_row_text(&term, row_line_idx);
                    let row_len = text.chars().count();
                    for s in crate::triggers::redact_spans(&logical, &trigger_set) {
                        let (lo, hi) =
                            (s.start.max(offset), (s.start + s.len).min(offset + row_len));
                        if lo >= hi {
                            continue;
                        }
                        // Counted on the row it starts on, once.
                        if s.start >= offset {
                            redacted_spans += 1;
                        }
                        for k in lo - offset..hi - offset {
                            if let Some(&col) = colmap.get(k) {
                                let cell = paint[col].get_or_insert(TrigCell {
                                    fg: None,
                                    bg: None,
                                    mask: false,
                                });
                                cell.mask = true;
                            }
                        }
                    }
                }
                paint
            });
            for x in 0..cols {
                let line_idx = (y as i32) - (display_offset as i32);
                let p = Point::new(Line(line_idx), Column(x as usize));
                let cell = &term.grid()[p];
                let mut display_char = cell_glyph(cell.c);
                let mut style = Style::default();
                if let Some(c) = ansi_to_ratatui(cell.fg, &self.palette) {
                    style = style.fg(c);
                }
                if let Some(c) = ansi_to_ratatui(cell.bg, &self.palette) {
                    style = style.bg(c);
                }
                let flags = cell.flags;
                if flags.contains(Flags::BOLD) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if flags.contains(Flags::ITALIC) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                // SGR 2 (faint): Claude Code's ghost suggestions and other
                // TUIs de-emphasise text with this; dropping it makes the
                // suggestion read as typed input.
                if flags.contains(Flags::DIM) {
                    style = style.add_modifier(Modifier::DIM);
                }
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if flags.contains(Flags::INVERSE) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // SGR 9: coding agents strike through finished todo items.
                if flags.contains(Flags::STRIKEOUT) {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
                // SGR 8 (conceal) paints a blank, not just HIDDEN, so the
                // text stays hidden on a host terminal that ignores SGR 8.
                // The grid keeps the real text for copy, as in xterm.
                if flags.contains(Flags::HIDDEN) {
                    style = style.add_modifier(Modifier::HIDDEN);
                    display_char = ' ';
                }
                // Trigger highlight: the user's per-trigger colours over the
                // matched span. Cursor / selection / find / quick-select all
                // paint after this, so they stay visible on top.
                if let Some(paint) = trig_paint.as_ref()
                    && let Some(Some(cell)) = paint.get(x as usize)
                {
                    if let Some(c) = cell.fg {
                        style = style.fg(c);
                    }
                    if let Some(c) = cell.bg {
                        style = style.bg(c);
                    }
                    if cell.mask {
                        display_char = crate::triggers::MASK;
                    }
                }
                // Annotated spans: amber + underline, under the cursor /
                // selection / find layers so those still win.
                if ann_spans
                    .iter()
                    .any(|&(l, s0, ln)| l == line_idx && x >= s0 && x < s0 + ln)
                {
                    style = style
                        .fg(self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)))
                        .add_modifier(Modifier::UNDERLINED);
                }
                if cursor_visible
                    && (y as i32) == cursor_row_in_viewport
                    && (x as i32) == cursor_col_in_viewport
                {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some((block, (sr, sc, er, ec))) = sel_paint
                    && sel_clip.as_ref().is_none_or(|clips| {
                        clips
                            .iter()
                            .any(|&(l, lo, hi)| l == line_idx && x >= lo && x <= hi)
                    })
                    && if block {
                        cell_in_block_selection(line_idx, x, sr, sc, er, ec)
                    } else {
                        cell_in_selection(line_idx, x, sr, sc, er, ec)
                    }
                {
                    style = style.bg(self.theme.ui(Color::Rgb(0x26, 0x4f, 0x78)));
                }
                // Find highlight: muted amber on every occurrence, bright
                // orange on the active match (VS Code's find colours). Croft
                // Loch tints the cell's own background amber instead, as the
                // editor does, keeping the program's ink where it reads.
                if let Some(paint) = row_paint.as_ref() {
                    match (paint.get(x as usize), tones) {
                        (Some(&mark @ (1 | 2)), Some(t)) => {
                            style = crate::widgets::surface::match_style(
                                style,
                                t,
                                self.theme,
                                mark == 2,
                            );
                        }
                        (Some(1), None) => {
                            style = style
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        (Some(2), None) => {
                            style = style
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0x8c, 0x2a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        _ => {}
                    }
                }
                // Quick-select: matched spans turn green, label cells overlay
                // black-on-gold and replace the glyph underneath (WezTerm's
                // quick-select colours).
                if let Some((span, labels)) = hint_paint.as_ref() {
                    match span.get(x as usize) {
                        Some(1) => {
                            style = style
                                .fg(self.theme.ui(Color::Rgb(0x66, 0xcc, 0x66)))
                                .add_modifier(Modifier::BOLD);
                        }
                        Some(2) => {
                            if let Some(&lc) = labels.get(x as usize)
                                && lc != '\0'
                            {
                                display_char = lc;
                            }
                            style = Style::default()
                                .fg(Color::Black)
                                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                                .add_modifier(Modifier::BOLD);
                        }
                        _ => {}
                    }
                }
                // Copy-mode cursor: a green modal block, painted last so the
                // caret stays visible over selection / find / trigger paint.
                if let Some((cl, cc)) = self.copy_cursor
                    && line_idx == cl
                    && x == cc
                {
                    style = Style::default()
                        .fg(Color::Black)
                        .bg(self.theme.ui(Color::Rgb(0x66, 0xcc, 0x66)))
                        .add_modifier(Modifier::BOLD);
                }
                let target_x = inner.x + x;
                let target_y = inner.y + y;
                let target = &mut buf[(target_x, target_y)];
                let mut tmp = [0u8; 4];
                target.set_symbol(display_char.encode_utf8(&mut tmp));
                target.set_style(style);
            }
        }
        self.redacted_on_screen = redacted_spans;
        // OSC 9;4 progress gauge along the bottom border (Ghostty/WezTerm
        // parity): a fill in the state's colour over the border glyphs —
        // blue normal, red error, yellow warning — and a sweeping segment
        // while indeterminate. Border cells are croft chrome, so no program
        // content is ever covered (same contract as the decoration dots).
        if let Some((state, pct)) = *self.progress.lock().unwrap()
            && area.height >= 2
            && inner.width > 0
        {
            let w = inner.width as u32;
            let (fill_from, fill_len) = if state == 3 {
                // Indeterminate: a ~fifth-width segment sweeping left to
                // right, phased off the wall clock so each frame advances.
                let seg = (w / 5).max(1);
                let span = (w - seg).max(1);
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u32)
                    .unwrap_or(0);
                ((ms / 80) % span, seg)
            } else {
                (0, w * u32::from(pct.min(100)) / 100)
            };
            let color = match state {
                2 => self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
                4 => self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)),
                _ => self.quiet_ok_ink(),
            };
            let by = area.y + area.height - 1;
            // The fill glyph has to be HEAVIER than the border it sits on,
            // or the gauge reads as full (#470): a focused pane's border is
            // already `━`, so painting `━` over it leaves only colour to
            // distinguish filled from unfilled -- and the focused pane is
            // the one most likely to be running the build being measured.
            // `═` against a heavy border is the same trick `━` plays
            // against a light one.
            // Croft Loch has no border to outweigh: a quarter block sits on
            // the frame row's floor, heavier than the hairline seam there.
            let fill_glyph = if surfaces {
                "\u{2582}"
            } else if self.focused {
                "═"
            } else {
                "━"
            };
            for i in 0..fill_len.min(w) {
                let x = inner.x + (fill_from + i) as u16;
                let cell = &mut buf[(x, by)];
                cell.set_symbol(fill_glyph);
                cell.set_style(Style::default().fg(color));
            }
        }
        // Timestamps gutter: the arrival clock of each stamped row hugs the
        // right edge (iTerm2's Show Timestamps), amber with a warning mark
        // when the row landed a long stall after its predecessor. Chrome
        // over content, painted only while the palette toggle is on.
        if self.show_timestamps && !alt_screen && inner.width > 14 {
            // Row ids are `clock + grid line` — content-stable through
            // scrollback saturation, matching the reader thread's stamps.
            // Clock before line_times: the shared lock order is
            // term → clock → line_times (see [`ScrollClock`]).
            let clock_now = self.clock_now(&term);
            let lt = self.line_times.lock().unwrap();
            for y in 0..rows {
                let abs = clock_now + (y as i32 - display_offset as i32) as i64;
                let Some(&ms) = lt.get(&abs) else { continue };
                let prev = lt.range(..abs).next_back().map(|(_, &v)| v);
                let stalled = prev.is_some_and(|p| ms.saturating_sub(p) >= STALL_GAP_MS);
                let mut text = hhmmss(ms);
                if stalled {
                    text = format!("\u{26a0} {text}");
                }
                let tw = text.chars().count() as u16;
                if tw >= inner.width {
                    continue;
                }
                let x0 = inner.x + inner.width - tw;
                let fg = if stalled {
                    self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b))
                } else {
                    self.theme.ui(Color::Rgb(0x5b, 0x64, 0x72))
                };
                for (j, c) in text.chars().enumerate() {
                    let cell = &mut buf[(x0 + j as u16, inner.y + y)];
                    let mut tmpc = [0u8; 4];
                    cell.set_symbol(c.encode_utf8(&mut tmpc));
                    cell.set_style(Style::default().fg(fg));
                }
            }
        }
        // Host-accent badge watermark: the rule's badge text, dim in the
        // accent color, parked at the pane's top-right (under the sticky
        // header, over content — it is a warning, that is the point).
        if let (Some((r, g, b)), Some(badge)) = (self.accent, self.accent_badge.as_deref())
            && !badge.is_empty()
        {
            let bw = badge.chars().count() as u16;
            if bw + 2 < inner.width {
                let x0 = inner.x + inner.width - bw - 1;
                for (j, c) in badge.chars().enumerate() {
                    let cell = &mut buf[(x0 + j as u16, inner.y)];
                    let mut tmpc = [0u8; 4];
                    cell.set_symbol(c.encode_utf8(&mut tmpc));
                    cell.set_style(
                        Style::default()
                            .fg(Color::Rgb(r, g, b))
                            .add_modifier(Modifier::DIM | Modifier::BOLD),
                    );
                }
            }
        }
        // Sticky command header (Warp): when the viewport is scrolled so its
        // top row falls inside one command's output while that command's
        // prompt sits above the view, the typed command pins to the pane's
        // top row with the scroll depth. Finished commands resolve through
        // the decoration spans; the still-running one through the newest
        // CommandStart mark. Unpins the instant the top row leaves the span.
        if display_offset > 0 && !alt_screen && rows > 0 && inner.width > 4 {
            let top_row = -(display_offset as i32);
            let cols_last = term.columns().saturating_sub(1);
            let header = decorations
                .iter()
                .find(|d| d.line < top_row && d.output_start <= top_row && top_row < d.output_end)
                .and_then(|d| {
                    let (l, c) = d.input?;
                    (d.output_start > l)
                        .then(|| extract_selection_text(&term, l, c, d.output_start - 1, cols_last))
                })
                .or_else(|| {
                    let ms = self.marks.lock().unwrap();
                    let running = ms.last().is_some_and(|m| {
                        matches!(m.kind, crate::shell_integration::OscEvent::CommandStart)
                    });
                    if !running {
                        return None;
                    }
                    let now = self.clock_now(&term);
                    let last = ms.last().unwrap();
                    let c_line = last.line_rec - (now - last.clock_rec) as i32;
                    (c_line < top_row).then(|| last_command_input_text(&term, &ms, now))
                })
                .filter(|t| !t.trim().is_empty());
            if let Some(text) = header {
                // Croft Loch pins the command on the raised surface in body
                // text, the scroll depth muted: a quiet card, not a banner.
                let tones = crate::widgets::surface::tones(self.theme);
                let bg = tones.map_or(self.theme.ui(Color::Rgb(0x25, 0x2b, 0x36)), |t| t.raised());
                let command_style = match tones {
                    Some(t) => Style::default().fg(t.text()).bg(bg),
                    None => Style::default()
                        .fg(self.theme.ui(Color::Rgb(0xec, 0xf0, 0xf4)))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                };
                let depth_fg =
                    tones.map_or(self.theme.ui(Color::Rgb(0x8b, 0x93, 0xa1)), |t| t.muted());
                for x in 0..inner.width {
                    let cell = &mut buf[(inner.x + x, inner.y)];
                    cell.set_symbol(" ");
                    cell.set_style(Style::default().bg(bg));
                }
                // A multi-row command (soft-wrapped or a quoted/heredoc
                // newline) arrives with its rows joined by '\n'; a cell must
                // never hold ANY control char, so the whole class maps to
                // spaces, not just the row join.
                let label: String = format!("\u{25b6} {}", text.trim())
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                let mut xw = inner.x + 1;
                for ch in label.chars() {
                    if xw + 1 >= inner.x + inner.width {
                        break;
                    }
                    let cell = &mut buf[(xw, inner.y)];
                    let mut tmpc = [0u8; 4];
                    cell.set_symbol(ch.encode_utf8(&mut tmpc));
                    cell.set_style(command_style);
                    xw += 1;
                }
                let right = format!(" \u{2191} {display_offset} ");
                let rw = right.chars().count() as u16;
                if xw + rw < inner.x + inner.width {
                    let x0 = inner.x + inner.width - rw;
                    for (j, ch) in right.chars().enumerate() {
                        let cell = &mut buf[(x0 + j as u16, inner.y)];
                        let mut tmpc = [0u8; 4];
                        cell.set_symbol(ch.encode_utf8(&mut tmpc));
                        cell.set_style(Style::default().fg(depth_fg).bg(bg));
                    }
                }
            }
        }
        // Command decorations: a VS Code-style dot on the left border at
        // each finished command's prompt row — blue for success, red for a
        // non-zero exit. Normal screen only; an alt-screen app owns the
        // viewport and has no prompts.
        if !alt_screen {
            for d in &decorations {
                let vp = d.line + display_offset as i32;
                if (0..rows as i32).contains(&vp) {
                    let ok = d.exit.unwrap_or(0) == 0;
                    let cell = &mut buf[(area.x, inner.y + vp as u16)];
                    cell.set_symbol("●");
                    cell.set_style(Style::default().fg(if ok {
                        self.quiet_ok_ink()
                    } else {
                        self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c))
                    }));
                }
            }
        }
    }
}

pub(super) type TrigRowPaint = Vec<Option<TrigCell>>;
