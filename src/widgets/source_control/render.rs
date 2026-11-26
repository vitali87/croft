//! Drawing the panel: the header, the branch summary, the message box, the Commit button,
//! the change list and the commit feedback, wrapped to fit.

use super::*;

pub(super) fn badge_color(kind: ChangeKind, theme: crate::theme::Theme) -> Color {
    match kind {
        ChangeKind::StagedAdded => theme.ui(Color::Rgb(0x81, 0xb8, 0x8c)),
        ChangeKind::StagedModified | ChangeKind::Modified => theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b)),
        ChangeKind::StagedDeleted | ChangeKind::Deleted => theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
        ChangeKind::StagedRenamed => theme.ui(Color::Rgb(0xb4, 0x8e, 0xad)),
        ChangeKind::Untracked => theme.ui(Color::Rgb(0x88, 0xc0, 0xd0)),
        ChangeKind::Conflicted => theme.ui(Color::Rgb(0xe7, 0x4c, 0x3c)),
    }
}

impl Widget for &mut SourceControlPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let focus_blue = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
        // Black theme: the commit-input focus ring wears the muted brand teal
        // instead of the legacy blue (the outer border becomes the gradient).
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::INNER_ACCENT)
        } else {
            focus_blue
        };
        let outer_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = crate::widgets::surface::view_block(self.theme, true, outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient.
        // The SOURCE CONTROL header sits inside the border, so nothing on the
        // border itself needs re-stamping.
        crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        );
        self.last_area = area;
        self.last_inner = inner;
        self.last_input_area = Rect::default();
        self.last_button_area = Rect::default();
        self.last_commit_caret_area = Rect::default();
        self.last_list_area = Rect::default();
        self.last_scrollbar = Rect::default();
        self.last_row_actions = None;
        self.header_refresh_btn = Rect::default();
        self.last_branch_area = Rect::default();
        self.header_more_btn = Rect::default();
        // Frame-truth (#103): a zero-sized frame must not leave last
        // frame's repository rows clickable.
        self.repo_row_areas.clear();

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Row 0: SOURCE CONTROL header (light grey bold), inside the panel.
        let header_title = crate::i18n::tr("SOURCE CONTROL");
        // Header pills must start past the (translated) title plus a gap.
        let title_end = inner.x
            + u16::try_from(unicode_width::UnicodeWidthStr::width(header_title.as_ref()))
                .unwrap_or(u16::MAX)
                .saturating_add(1);
        // Croft Loch: the view title is the strong ink while the view has
        // the keyboard and muted otherwise, never bold, as the Explorer's.
        let tones = crate::widgets::surface::tones(self.theme);
        let title_style = match tones {
            Some(t) => Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            None => Style::default()
                .fg(self.theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8)))
                .add_modifier(Modifier::BOLD),
        };
        buf.set_stringn(
            inner.x,
            inner.y,
            &header_title,
            inner.width as usize,
            title_style,
        );
        // Refresh pill at the right of the header row, mirroring the Remote
        // Explorer affordance. Live FS-driven refresh already keeps the panel
        // current; this is the manual override for the rare case the watcher
        // misses an edit. Only painted in a repo with room past the title.
        let pill_w: u16 = 2;
        let refresh_x = (inner.x + inner.width).saturating_sub(pill_w + 1);
        if self.status.in_repo && refresh_x > title_end {
            let rect = Rect {
                x: refresh_x,
                y: inner.y,
                width: pill_w,
                height: 1,
            };
            self.header_refresh_btn = rect;
            let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
            crate::widgets::header_pill::render(
                buf,
                refresh_x,
                inner.y,
                crate::widgets::header_pill::REFRESH_GLYPH,
                self.theme,
                hovered,
            );
            // "⋯" actions pill, one pill-plus-gap to the left of refresh,
            // opening the full Source Control actions menu (VS Code's title
            // "⋯" menu). cod-ellipsis (U+EA7C).
            let more_x = refresh_x.saturating_sub(pill_w + 1);
            if more_x > title_end {
                let more_rect = Rect {
                    x: more_x,
                    y: inner.y,
                    width: pill_w,
                    height: 1,
                };
                self.header_more_btn = more_rect;
                let more_hovered = crate::widgets::hover::contains(more_rect, self.hover_pointer);
                // Croft Loch draws the Explorer's midline ellipsis, so the
                // two views' "more" buttons are one glyph.
                let more_glyph = match tones {
                    Some(_) => '\u{22ef}',
                    None => crate::widgets::scm_menu::ICON_ELLIPSIS,
                };
                crate::widgets::header_pill::render(
                    buf,
                    more_x,
                    inner.y,
                    more_glyph,
                    self.theme,
                    more_hovered,
                );
            }
        }

        // Non-repo workspace: render the "No repository detected" card -
        // bordered card with illustration, title, description, primary
        // (Initialize Repository) and secondary (Open Folder) buttons,
        // plus a "Learn more about Git" help link. Mirrors the reference
        // design the user supplied.
        if !self.status.in_repo {
            self.last_init_repo_button_area = Rect::default();
            self.nested_repo_button_areas.clear();
            self.last_hero_area = Rect::default();
            self.render_no_repo_empty_state(inner, buf);
            // With several folders open, the OTHER folders' repositories
            // stay reachable from the empty state too (#161): the
            // overview paints over the card's top rows.
            self.render_repositories_section(inner, inner.y + 2, buf);
            return;
        }
        // Repo path: the hero and the card's buttons are only for the empty
        // state, and a button area left from its last frame would take a
        // click meant for what is drawn there now (#2736).
        self.last_hero_area = Rect::default();
        self.last_init_repo_button_area = Rect::default();
        self.nested_repo_button_areas.clear();

        // Row 2: branch row — a green branch glyph plus the branch name.
        let mut y = inner.y + 2;
        y = self.render_repositories_section(inner, y, buf);
        if y >= inner.y + inner.height {
            return;
        }
        let mut spans: Vec<Span> = Vec::with_capacity(5);
        // Codicon `cod-source-control` (U+EA68, verified against the
        // upstream codicon mapping.json AND the Nerd Fonts CSS) — the
        // Y-fork branch glyph in cyan, matching VS Code's mockup.
        // U+EB14 was wrong: that's `cod-link-external`, the share-arrow
        // the user spotted; ACTIVITY_SOURCE_CONTROL in icons.rs had the
        // same mis-pin and is fixed in the same commit.
        // Croft Loch keeps the accent for focus and selection: the glyph is
        // a muted icon beside the branch name.
        spans.push(Span::styled(
            "\u{ea68} ",
            Style::default().fg(match tones {
                Some(t) => t.muted(),
                None => self.theme.ui(Color::Rgb(0x88, 0xc0, 0xd0)),
            }),
        ));
        let label = self.status.head_label().to_string();
        // The glyph (2 cells) plus the name is the clickable target that
        // opens the branch picker, clamped to the panel width.
        let branch_w = (2 + label.chars().count() as u16).min(inner.width);
        self.last_branch_area = Rect {
            x: inner.x,
            y,
            width: branch_w,
            height: 1,
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(tones.map_or_else(|| self.theme.ui(Color::White), |t| t.text()))
                .add_modifier(Modifier::BOLD),
        ));
        // What git stopped in the middle of (#1356): the ⋯ menu holds its
        // Continue / Abort.
        if let Some(op) = self.status.operation {
            spans.push(Span::styled(
                format!("  {}", op.label()),
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b)))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if self.status.ahead > 0 {
            spans.push(Span::styled(
                format!("  \u{2191}{}", self.status.ahead),
                Style::default().fg(self.theme.ui(Color::Rgb(0xa3, 0xbe, 0x8c))),
            ));
        }
        if self.status.behind > 0 {
            spans.push(Span::styled(
                format!("  \u{2193}{}", self.status.behind),
                Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
            ));
        }
        buf.set_line(inner.x, y, &Line::from(spans), inner.width);
        y += 2; // blank gap below branch row

        // Rows y..y+3: commit-message input, a rounded box one row per
        // message line (up to MESSAGE_MAX_ROWS, then it scrolls).
        if y + 3 > inner.y + inner.height {
            return;
        }
        let line_count = self.message.split('\n').count();
        // Below the box: gap, Commit button, gap, the feedback line when
        // there is one, the separator and a couple of change-list rows. A
        // long message gives up rows (and scrolls) before any of those do;
        // a short panel still gets a one-line box.
        let feedback_rows = if self.commit_feedback.is_some() { 2 } else { 0 };
        let below = 1 + 3 + 1 + feedback_rows + 1 + MESSAGE_LIST_MIN_ROWS;
        let left = inner.y + inner.height - y;
        let input_box = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: (line_count.clamp(1, MESSAGE_MAX_ROWS) as u16 + 2)
                .min(left.saturating_sub(below).max(3))
                .min(left),
        };
        self.last_input_area = input_box;
        let input_border_style = if self.focused && self.keys == KeyStop::Message {
            Style::default().fg(accent)
        } else {
            Style::default().fg(self.theme.ui(Color::Rgb(0x60, 0x68, 0x78)))
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(input_border_style);
        let input_inner = input_block.inner(input_box);
        // Croft Loch: a raised well with a hairline edge (the rim at rest,
        // the accent while it has the keyboard) in place of the rounded line
        // box. The inner rect is the box's, so the text and the caret land on
        // the same cells.
        let field_focused = self.focused && self.keys == KeyStop::Message;
        match tones {
            Some(t) => crate::widgets::surface::paint_field(
                buf,
                input_box,
                t,
                if field_focused { t.accent() } else { t.rim() },
            ),
            None => input_block.render(input_box, buf),
        }
        if input_inner.width > 0 && input_inner.height > 0 {
            let content_y = input_inner.y;
            // 1-cell left padding inside the rounded border, and clamp the
            // available text width to `input_inner.width - 2` so the
            // string can never overflow past the right border into the
            // editor pane. The previous `buf.set_string` call had no width
            // cap and bled "r = push)" past the sidebar's right edge into
            // the editor area on narrow sidebar widths, which the user
            // reported as the placeholder visibly leaking out of the box.
            let text_x = input_inner.x + 1;
            let text_max = (input_inner.width as usize).saturating_sub(2);
            if self.message.is_empty() {
                self.message_scroll = 0;
                self.message_line_scroll = 0;
                if let Some(t) = tones {
                    // Croft Loch: "Message" in the muted ink, and the keys
                    // as a faint hint at the field's right edge, as long a
                    // hint as fits whole: never cut mid-word.
                    buf.set_stringn(
                        text_x,
                        content_y,
                        COMMIT_PLACEHOLDER,
                        text_max,
                        Style::default()
                            .fg(t.muted())
                            .add_modifier(Modifier::ITALIC),
                    );
                    if let Some(hint) = commit_placeholder_hint(text_max) {
                        let w = unicode_width::UnicodeWidthStr::width(hint);
                        buf.set_string(
                            text_x + (text_max - w) as u16,
                            content_y,
                            hint,
                            Style::default().fg(t.faint()),
                        );
                    }
                } else {
                    buf.set_stringn(
                        text_x,
                        content_y,
                        "Message (Enter = commit, \u{21e7}Enter = new line, \u{2303}Enter = push)",
                        text_max,
                        Style::default()
                            .fg(Color::Rgb(
                                INPUT_PROMPT_RGB.0,
                                INPUT_PROMPT_RGB.1,
                                INPUT_PROMPT_RGB.2,
                            ))
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            } else {
                // Horizontal scroll: keep the caret inside the box so a
                // message wider than the input (e.g. a pasted path) stays
                // editable. Without this the box always shows byte 0, the
                // caret falls off the right edge, and edits at the tail are
                // invisible — the field reads as frozen. Vertical scroll
                // does the same for the caret's line in a long message.
                let (line, cursor) = self.caret_line_col();
                if cursor < self.message_scroll {
                    self.message_scroll = cursor;
                } else if text_max > 0 && cursor > self.message_scroll + text_max {
                    self.message_scroll = cursor - text_max;
                }
                let rows = input_inner.height as usize;
                if line < self.message_line_scroll {
                    self.message_line_scroll = line;
                } else if line >= self.message_line_scroll + rows {
                    self.message_line_scroll = line + 1 - rows;
                }
                for (row, text) in self
                    .message
                    .split('\n')
                    .skip(self.message_line_scroll)
                    .take(rows)
                    .enumerate()
                {
                    let visible: String = text
                        .chars()
                        .skip(self.message_scroll)
                        .take(text_max)
                        .collect();
                    buf.set_stringn(
                        text_x,
                        content_y + row as u16,
                        &visible,
                        text_max,
                        Style::default()
                            .fg(tones.map_or_else(|| self.theme.ui(Color::White), |t| t.text())),
                    );
                }
            }
        }
        y += input_box.height + 1; // input box + 1-row gap

        // Rows y..y+3: chunky split-button — wide "Commit" main + narrow
        // chevron caret. The caret reveals a "Commit & Push" dropdown so
        // users have the same affordance VS Code surfaces. Narrow panels
        // skip the split and render a plain Commit button so the caret
        // never collides with the label.
        if y + 3 > inner.y + inner.height {
            return;
        }
        // Black theme: brand-teal Commit button; Croft Dark keeps VS Code blue.
        let (blue, white) = primary_button_colors(self.theme, self.focus_gradient);
        let split_caret_w: u16 = 3;
        let split_gap_w: u16 = 1;
        if inner.width >= 16 + split_caret_w + split_gap_w {
            let main_w = inner.width.saturating_sub(split_caret_w + split_gap_w);
            let main_area = Rect {
                x: inner.x,
                y,
                width: main_w,
                height: 3,
            };
            let caret_area = Rect {
                x: inner.x + main_w + split_gap_w,
                y,
                width: split_caret_w,
                height: 3,
            };
            self.last_button_area = main_area;
            self.last_commit_caret_area = caret_area;
            // Nerd Fonts `cod-check` (U+EAB2, verified against
            // ryanoasis/nerd-fonts glyphnames.json v3.4.0) — the same crisp
            // codicon VS Code paints on its Commit button, replacing the
            // generic Unicode check that did not match the dropdown glyphs.
            if tones.is_some() {
                self.render_soft_split_button(buf, main_area, caret_area);
            } else {
                render_rounded_button(buf, main_area, "\u{eab2} Commit", blue, white);
                render_rounded_button(buf, caret_area, "▾", blue, white);
            }
        } else {
            let button_area = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 3,
            };
            self.last_button_area = button_area;
            let hovered = crate::widgets::hover::contains(button_area, self.hover_pointer);
            render_button(buf, button_area, "Commit", blue, white, self.theme, hovered);
        }
        y += 3 + 1; // button + 1-row gap

        // Optional feedback line: its first line only, cut at the panel's
        // edge. Git's output runs to several lines and past any sidebar, and
        // an uncapped `set_string` painted it across the editor (#858).
        if let Some(msg) = self.commit_feedback.as_ref()
            && y < inner.y + inner.height
        {
            let style = if self.commit_feedback_is_error {
                Style::default().fg(self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)))
            } else {
                Style::default().fg(self.theme.ui(Color::Rgb(0xa3, 0xbe, 0x8c)))
            };
            // git's headline (#858), wrapped to the panel: a refusal ran on
            // past its edge into the editor, which painted over it (#989).
            let line = crate::git::headline(msg);
            let rows = wrap_feedback(line, inner.width as usize, FEEDBACK_MAX_ROWS);
            for row in &rows {
                if y >= inner.y + inner.height {
                    break;
                }
                buf.set_stringn(inner.x, y, row, inner.width as usize, style);
                y += 1;
            }
            y += 1;
        }

        // Thin separator line.
        if y >= inner.y + inner.height {
            return;
        }
        // Croft Loch: a hairline seam hugging the list below, not a line
        // twice as bright as every other seam in the side bar.
        let (sep_glyph, sep_style) = match tones {
            Some(t) => ("\u{2581}", Style::default().fg(t.hairline())),
            None => (
                "─",
                Style::default().fg(self.theme.ui(Color::Rgb(0x40, 0x48, 0x58))),
            ),
        };
        for x in inner.x..inner.x + inner.width {
            buf.set_string(x, y, sep_glyph, sep_style);
        }
        y += 1;

        // List of changes.
        if y >= inner.y + inner.height {
            return;
        }
        let list_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: inner.y + inner.height - y,
        };
        self.last_list_area = list_area;
        let lines = self.list_layout();
        let total = lines.len();
        let viewport = list_area.height as usize;
        if viewport == 0 {
            return;
        }
        if self.scroll > total.saturating_sub(viewport) {
            self.scroll = total.saturating_sub(viewport);
        }

        let scrollbar_area = Rect {
            x: list_area.x + list_area.width.saturating_sub(1),
            y: list_area.y,
            width: u16::from(list_area.width > 0),
            height: list_area.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, total, viewport, self.scroll);
        if let Some(m) = scrollbar_metrics {
            self.last_scrollbar = m.area;
        }
        let row_width = list_area
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        if total == 0 {
            buf.set_stringn(
                list_area.x,
                list_area.y,
                "No changes",
                list_area.width as usize,
                Style::default().fg(tones.map_or(Color::DarkGray, |t| t.muted())),
            );
            return;
        }

        let selected_bg = self.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
        let end = (self.scroll + viewport).min(total);
        for (row, idx) in (self.scroll..end).enumerate() {
            let row_y = list_area.y + row as u16;
            match &lines[idx] {
                ListLine::Header(section) => {
                    let (count, sec_add, sec_del) = self
                        .entries
                        .iter()
                        .filter(|e| e.kind.section() == *section)
                        .fold((0usize, 0usize, 0usize), |(c, a, d), e| {
                            (c + 1, a + e.additions, d + e.deletions)
                        });
                    let mut header_spans = match tones {
                        // Croft Loch: the side bar's section header (a muted
                        // codicon chevron, the label in body text) and a
                        // quiet count chip, muted on a faint lift.
                        Some(t) => vec![
                            Span::styled(
                                format!("{} ", crate::widgets::surface::CHEVRON_UNFOLDED),
                                Style::default().fg(t.muted()),
                            ),
                            Span::styled(
                                section_label(*section),
                                Style::default().fg(t.text()).add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                format!(" {count} "),
                                Style::default().fg(t.muted()).bg(t.keycap_on(t.chrome())),
                            ),
                        ],
                        None => vec![
                            Span::styled(
                                "\u{25be} ",
                                Style::default().fg(self.theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8))),
                            ),
                            Span::styled(
                                section_label(*section),
                                Style::default()
                                    .fg(Color::Rgb(
                                        SECTION_HEADER_RGB.0,
                                        SECTION_HEADER_RGB.1,
                                        SECTION_HEADER_RGB.2,
                                    ))
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                format!(" {count} "),
                                Style::default()
                                    .fg(self.theme.ui(Color::White))
                                    .bg(self.theme.ui(Color::Rgb(0x2a, 0x33, 0x42)))
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ],
                    };
                    if sec_add > 0 || sec_del > 0 {
                        header_spans.push(Span::raw("  "));
                        if sec_add > 0 {
                            header_spans.push(Span::styled(
                                format!("+{sec_add}"),
                                Style::default()
                                    .fg(Color::Rgb(
                                        ADDITIONS_RGB.0,
                                        ADDITIONS_RGB.1,
                                        ADDITIONS_RGB.2,
                                    ))
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        if sec_add > 0 && sec_del > 0 {
                            header_spans.push(Span::raw(" "));
                        }
                        if sec_del > 0 {
                            header_spans.push(Span::styled(
                                format!("-{sec_del}"),
                                Style::default()
                                    .fg(Color::Rgb(
                                        DELETIONS_RGB.0,
                                        DELETIONS_RGB.1,
                                        DELETIONS_RGB.2,
                                    ))
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    buf.set_line(list_area.x, row_y, &Line::from(header_spans), row_width);
                }
                ListLine::Entry(entry_idx) => {
                    let entry = &self.entries[*entry_idx];
                    let row_rect = Rect {
                        x: list_area.x,
                        y: row_y,
                        width: row_width,
                        height: 1,
                    };
                    let is_primary = self.selected_change == Some(*entry_idx);
                    let is_in_multi = self.multi_selection.contains(entry_idx);
                    // Multi-selected rows all carry the VS Code list-active
                    // blue bg so a Cmd+A → Cmd+S sweep makes its scope
                    // visible; the primary row (the one driving the diff)
                    // keeps the brightest tone, secondary rows a slightly
                    // dimmer one so the user still sees which file the
                    // editor is bound to.
                    //
                    // Croft Loch: the one selected-row anatomy (band plus the
                    // accent bar in the pane's left padding column, greyed
                    // while the list does not have the keyboard), and a
                    // softer tint of the band for the rest of a multi-row
                    // selection instead of the legacy navy.
                    if let Some(t) = tones
                        && is_primary
                    {
                        crate::widgets::surface::paint_selected_row(
                            buf,
                            Rect {
                                x: area.x,
                                width: row_width.saturating_add(list_area.x - area.x),
                                ..row_rect
                            },
                            t,
                            t.chrome,
                            self.focused && self.keys == KeyStop::List,
                        );
                    }
                    let row_bg = if is_primary {
                        tones.is_none().then_some(selected_bg)
                    } else if is_in_multi {
                        Some(match tones {
                            Some(t) => crate::gradient::rgb_color(crate::gradient::lerp_rgb(
                                t.chrome, t.accent, 0.12,
                            )),
                            None => self.theme.ui(Color::Rgb(0x1f, 0x3a, 0x5c)),
                        })
                    } else {
                        crate::widgets::hover::row_hover_bg(
                            row_rect,
                            self.hover_pointer,
                            self.theme,
                        )
                    };
                    let is_selected = is_primary;
                    if let Some(bg) = row_bg {
                        let row_bg_style = Style::default().bg(bg);
                        for rx in 0..row_rect.width {
                            buf[(row_rect.x + rx, row_rect.y)]
                                .set_symbol(" ")
                                .set_style(row_bg_style);
                        }
                    }
                    let path_str = entry.path.as_str();
                    let is_dir = path_str.ends_with('/');
                    let icon = if is_dir {
                        icons::FOLDER_CLOSED
                    } else {
                        let basename = std::path::Path::new(path_str)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path_str.to_string());
                        let suffix = std::path::Path::new(&basename)
                            .extension()
                            .map(|e| format!(".{}", e.to_string_lossy()))
                            .unwrap_or_default();
                        icons::for_path(&basename, &suffix)
                    };
                    let badge = entry.kind.badge();
                    let badge_str = badge.to_string();
                    let badge_w: u16 = 1;
                    let row_padding: u16 = 1;
                    let badge_x = row_rect
                        .x
                        .saturating_add(row_rect.width.saturating_sub(badge_w + row_padding));
                    let mut badge_style = Style::default()
                        .fg(badge_color(entry.kind, self.theme))
                        .add_modifier(Modifier::BOLD);
                    let mut icon_style = Style::default().fg(self.theme.ui(icon.color));
                    let mut path_style = Style::default().fg(self.theme.ui(Color::White));
                    if let Some(bg) = row_bg {
                        badge_style = badge_style.bg(bg);
                        icon_style = icon_style.bg(bg);
                        path_style = path_style.bg(bg);
                    }
                    if row_rect.width > badge_w + row_padding + 4 {
                        buf.set_string(badge_x, row_y, badge_str.as_str(), badge_style);
                    }
                    let icon_x = row_rect.x + 1;
                    buf.set_string(icon_x, row_y, icon.glyph.to_string(), icon_style);
                    let text_x = icon_x + 2;
                    // Reserve a 4-cell strip before the badge for inline
                    // action icons on the selected row. An unstaged row
                    // gets [discard][gap][stage]; a staged row gets the
                    // single [unstage] minus, mirroring VS Code (staged
                    // changes only offer Unstage; the Changes section
                    // offers Discard + Stage).
                    let is_staged = entry.kind.section() == ChangeSection::Staged;
                    let text_w_unreserved = badge_x.saturating_sub(text_x).saturating_sub(1);
                    let show_actions = is_selected && text_w_unreserved > ROW_ACTIONS_WIDTH;
                    let text_w_reserved = if show_actions { ROW_ACTIONS_WIDTH } else { 0 };
                    let text_w = text_w_unreserved.saturating_sub(text_w_reserved);
                    if let Some(t) = tones
                        && text_w > 0
                    {
                        // Croft Loch: the file name in body text, then its
                        // folder in the muted ink, as VS Code lists changes,
                        // so the eye finds the file before the path.
                        let trimmed = path_str.trim_end_matches('/');
                        let (dir, name) = match trimmed.rsplit_once('/') {
                            Some((dir, name)) if !is_dir => (Some(dir), name),
                            _ => (None, path_str),
                        };
                        let mut spans = vec![Span::styled(name, path_style.fg(t.text()))];
                        if let Some(dir) = dir {
                            spans.push(Span::styled(format!(" {dir}"), path_style.fg(t.muted())));
                        }
                        buf.set_line(text_x, row_y, &Line::from(spans), text_w);
                    } else if text_w > 0 {
                        let path_para = Paragraph::new(path_str).style(path_style);
                        path_para.render(
                            Rect {
                                x: text_x,
                                y: row_y,
                                width: text_w,
                                height: 1,
                            },
                            buf,
                        );
                    }
                    if show_actions {
                        let action_fg = tones.map_or_else(
                            || self.theme.ui(Color::Rgb(0xd4, 0xd9, 0xe2)),
                            |t| t.text(),
                        );
                        let mut action_style = Style::default().fg(action_fg);
                        if let Some(bg) = row_bg {
                            action_style = action_style.bg(bg);
                        }
                        let cell = |x: u16| Rect {
                            x,
                            y: row_y,
                            width: 1,
                            height: 1,
                        };
                        if is_staged {
                            let unstage_x = badge_x - 2;
                            buf.set_string(
                                unstage_x,
                                row_y,
                                UNSTAGE_GLYPH.to_string(),
                                action_style,
                            );
                            self.last_row_actions = Some(RowActionAreas {
                                entry_idx: *entry_idx,
                                discard: Rect::default(),
                                stage: Rect::default(),
                                unstage: cell(unstage_x),
                            });
                        } else {
                            let discard_x = badge_x - 4;
                            let stage_x = badge_x - 2;
                            buf.set_string(
                                discard_x,
                                row_y,
                                DISCARD_GLYPH.to_string(),
                                action_style,
                            );
                            buf.set_string(stage_x, row_y, STAGE_GLYPH.to_string(), action_style);
                            self.last_row_actions = Some(RowActionAreas {
                                entry_idx: *entry_idx,
                                discard: cell(discard_x),
                                stage: cell(stage_x),
                                unstage: Rect::default(),
                            });
                        }
                    }
                }
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical_on_chrome(buf, metrics, self.focused, self.theme);
        }
    }
}

/// Most rows a commit feedback message takes before it is cut with `…`.
pub(super) const FEEDBACK_MAX_ROWS: usize = 4;

/// `msg` broken into rows of at most `width` columns, at spaces where it can
/// be and mid-word where a word is wider than a row; at most `max_rows`,
/// the last ending in `…` when the message runs longer.
pub(super) fn wrap_feedback(msg: &str, width: usize, max_rows: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr as _;
    if width == 0 {
        return Vec::new();
    }
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in msg.split_whitespace() {
        let gap = usize::from(!row.is_empty());
        if row.width() + gap + word.width() <= width {
            if gap == 1 {
                row.push(' ');
            }
            row.push_str(word);
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        for c in word.chars() {
            if row.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > width {
                rows.push(std::mem::take(&mut row));
            }
            row.push(c);
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.len() > max_rows {
        rows.truncate(max_rows);
        let last = &mut rows[max_rows - 1];
        while last.width() + 1 > width {
            last.pop();
        }
        last.push('…');
    }
    rows
}
