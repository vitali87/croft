//! Drawing the tree: its rows, icons, marks and guides, the sticky band, the scrollbar,
//! and the dot on a file an agent changed.

use super::*;

/// The unreviewed-file marker (#345). A middle dot rather than a filled
/// circle: it must read as a small annotation beside a filename, not compete
/// with the file-type icon at the start of the row.
pub(super) const AGENT_DOT: char = '\u{b7}';

impl Widget for &mut FileTree {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Black theme: chipless brand header (panelTitle.activeForeground);
        // Croft Dark keeps the historical white-on-navy chip.
        let label = format!(" {} ", crate::i18n::tr("EXPLORER"));
        let label_width = unicode_width::UnicodeWidthStr::width(label.as_str());
        let tones = crate::widgets::surface::tones(self.theme);
        // Croft Loch: the view title is the one label in the side bar that
        // says where focus is, so it is the strong ink while the tree has
        // the keyboard and recedes to muted otherwise. Never bold: weight
        // is kept for the workspace root below it.
        let title = if let Some(t) = tones {
            Span::styled(
                label,
                Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            )
        } else if self.focus_gradient {
            Span::styled(
                label,
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                label,
                Style::default()
                    .fg(self.theme.ui(Color::White))
                    .bg(self.theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block =
            crate::widgets::surface::view_block(self.theme, true, block_style).title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the orange→green
        // gradient (matching the welcome activity box), then re-stamp the
        // title the gradient top edge just overwrote. Croft Loch marks focus
        // with the accent lip, which starts one cell past the title's own
        // padding: the title keeps a breath of space on both sides and the
        // lip's accent end sits right beside the name of the focused view.
        if tones.is_some() {
            if self.focused {
                let lip_x = area
                    .x
                    .saturating_add(1)
                    .saturating_add(u16::try_from(label_width).unwrap_or(u16::MAX))
                    .min(area.right());
                crate::widgets::surface::paint_focus_rule(
                    buf,
                    Rect {
                        x: lip_x,
                        width: area.right() - lip_x,
                        ..area
                    },
                    self.theme,
                );
            }
        } else if crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        ) {
            buf.set_span(area.x + 1, area.y, &title, title.width() as u16);
        }
        // The New File / New Folder / Refresh / Collapse toolbar is painted
        // later, on the root folder row. The "Views and More Actions" (⋯)
        // button sits here on the EXPLORER title line, mirroring VS Code's
        // view-container header. Reset the hit-test rects each frame so a
        // hidden affordance never registers stale clicks.
        self.header_new_file_btn = Rect::default();
        self.header_new_folder_btn = Rect::default();
        self.header_refresh_btn = Rect::default();
        self.header_collapse_btn = Rect::default();
        self.header_views_btn = Rect::default();
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.sticky_rows.clear();

        // Paint the ⋯ button at the right end of the title border row. It is
        // always available (not focus-gated) so the view-toggle menu is one
        // click away even when the tree is unfocused, like VS Code. It wears
        // the shared header-action pill — a space-padded `" ⋯ "` inset one cell
        // from the rounded corner — so it matches the terminal `+` button
        // exactly instead of jamming a naked glyph against the corner.
        // Only when the pill clears the (translated) title, which starts one
        // cell in from the corner: `x + 1 + label_width <= x + width - 4`.
        if usize::from(area.width) >= label_width + 5
            && let Some(rect) = crate::widgets::header_pill::render_action(
                buf,
                area.x + area.width - 1,
                area.y,
                crate::widgets::header_pill::MORE_LABEL,
                self.theme,
                self.hover_pointer,
            )
        {
            self.header_views_btn = rect;
        }

        self.last_inner = inner;

        let visible_height = inner.height as usize;
        if visible_height == 0 {
            return;
        }
        // Every node, in tree order — `visible` holds node indices.
        let visible = self.visible_indices();
        // Position of the selected node within the filtered list, so scroll and
        // highlight track the cursor even when intermediate rows are hidden.
        let sel_pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        // Scroll is tracked in filtered-row positions. Re-derive it each frame
        // from `self.scroll` (a node index) by mapping to a position, then keep
        // the selection on screen.
        let mut scroll_pos = visible.iter().position(|&i| i >= self.scroll).unwrap_or(0);
        if sel_pos < scroll_pos {
            scroll_pos = sel_pos;
        } else if sel_pos >= scroll_pos + visible_height {
            scroll_pos = sel_pos + 1 - visible_height;
        }
        // Persist the node index at the top of the viewport so the next frame's
        // mapping is stable.
        self.scroll = visible.get(scroll_pos).copied().unwrap_or(0);
        self.last_scroll = self.scroll;
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: u16::from(inner.width > 0),
            height: inner.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, visible.len(), visible_height, scroll_pos);
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = inner
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        let pointer = self.hover_pointer;
        // No Explorer filter any more, so no characters are ever highlighted.
        let query = String::new();
        let end = (scroll_pos + visible_height).min(visible.len());
        // Croft Loch's indent guides: the guide of the selected row's parent
        // folder is drawn a step brighter down that folder's children (VS
        // Code's active indent guide), as `(level, first, end)` node indices.
        let active_guide = tones.and_then(|_| self.active_indent_guide());
        for (row, &idx) in visible[scroll_pos..end].iter().enumerate() {
            let node = &self.nodes[idx];
            let is_selected = idx == self.selected;
            let is_marked = self.marked.contains(&node.path);
            let is_drop_target = self.drag_target == Some(idx);
            let y = inner.y + row as u16;

            let mut spans: Vec<Span> = Vec::with_capacity(6);
            match tones {
                // Croft Loch: a thin guide under each ancestor folder's
                // chevron, in the cells the indent already takes. The
                // workspace root's level gets none: every row is under it,
                // so its guide would only be a second left edge.
                Some(t) if node.depth >= 2 => {
                    spans.push(Span::raw("  "));
                    for level in 1..node.depth {
                        let active = active_guide.is_some_and(|(l, first, end)| {
                            l == level && (first..end).contains(&idx)
                        });
                        spans.push(Span::styled(
                            "\u{258f} ",
                            Style::default().fg(if active { t.faint() } else { t.hairline() }),
                        ));
                    }
                }
                _ => spans.push(Span::raw("  ".repeat(node.depth))),
            }

            let name = node.label();

            // Git-ignored and changed rows colour the *name* only — icons
            // keep their color, matching VS Code's git decorations.
            let name_fg = self.name_fg(&node.path);
            // Croft Loch's quiet layer: chevrons, the rolled-up dot, the
            // root badge and note counts all in the muted ink.
            let mark_fg = tones.map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());

            if node.is_dir {
                let chev = crate::widgets::surface::chevron(self.theme, node.expanded);
                let icon = if node.expanded {
                    icons::FOLDER_OPEN
                } else {
                    icons::FOLDER_CLOSED
                };
                spans.push(Span::styled(
                    format!("{chev} "),
                    Style::default().fg(mark_fg),
                ));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(self.folder_icon_fg(icon.color)),
                ));
                // Bold marks a folder on a boxed theme. Croft Loch keeps
                // weight for the workspace root alone, so the tree reads as
                // one calm list under a single heading.
                let base = if tones.is_some() && node.depth > 0 {
                    Style::default().fg(name_fg)
                } else {
                    Style::default().fg(name_fg).add_modifier(Modifier::BOLD)
                };
                push_name_spans(&mut spans, &name, &query, base, self.theme);
                // A collapsed folder hiding an unreviewed agent write shows a
                // dimmer copy of the file dot (#345), so the lane's changes
                // stay findable without expanding every folder. Only while
                // collapsed: expanded, the file's own dot is on screen.
                if !node.expanded && self.hides_agent_touched(&node.path) {
                    spans.push(Span::styled(
                        format!(" {AGENT_DOT}"),
                        Style::default().fg(mark_fg),
                    ));
                }
                // A lane root wears its branch and its agent after the name
                // (#348), dim so the folder name stays the thing the eye
                // lands on; trailing for the same reason as the agent dot.
                if node.depth == 0
                    && let Some(badge) = self.root_badges.get(&node.path)
                {
                    // `Gray`, the dim the theme maps for light grounds;
                    // `DarkGray` is a named colour `Theme::ui` passes through.
                    spans.push(Span::styled(
                        format!("  {badge}"),
                        Style::default().fg(mark_fg),
                    ));
                }
            } else {
                let suffix = node
                    .path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let icon = icons::for_path(&name, &suffix);
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(self.theme.ui(icon.color)),
                ));
                push_name_spans(
                    &mut spans,
                    &name,
                    &query,
                    Style::default().fg(name_fg),
                    self.theme,
                );
                // The agent-lane dot (#345), AFTER the name rather than
                // before it: a leading marker would shift every name in the
                // tree by a column as writes land, so the eye loses the
                // alignment it reads the tree by. Trailing, it appears and
                // clears without moving anything.
                if self.is_agent_touched(&node.path) {
                    // Croft Loch takes its own palette's yellow rather than
                    // the named colour, which follows the host terminal.
                    let dot = match tones {
                        Some(_) => crate::gradient::rgb_color(self.theme.ansi()[3]),
                        None => self.theme.ui(Color::Yellow),
                    };
                    spans.push(Span::styled(
                        format!(" {AGENT_DOT}"),
                        Style::default().fg(dot),
                    ));
                }
                if let Some(n) = self.note_counts.get(&node.path) {
                    spans.push(Span::styled(
                        format!(" \u{270e}{n}"),
                        Style::default().fg(mark_fg),
                    ));
                }
            }

            let line = Line::from(spans);
            let row_rect = Rect {
                x: inner.x,
                y,
                width: row_width,
                height: 1,
            };
            let drop_bg = self.theme.ui(Color::Rgb(0x2c, 0x60, 0x2e));
            if let Some(t) = tones {
                // Croft Loch: the one selected-row anatomy, the band with the
                // accent bar standing in the pane's left padding column so
                // the names keep their column; greyed while another pane has
                // the keyboard. Marked rows wear a softer tint of the band,
                // hover a neutral lift.
                if is_drop_target {
                    buf.set_style(row_rect, Style::default().bg(drop_bg));
                } else if is_selected {
                    crate::widgets::surface::paint_selected_row(
                        buf,
                        Rect {
                            x: area.x,
                            width: row_width.saturating_add(inner.x - area.x),
                            ..row_rect
                        },
                        t,
                        t.chrome,
                        self.focused,
                    );
                } else if is_marked {
                    let tint = crate::gradient::lerp_rgb(t.chrome, t.accent, 0.12);
                    buf.set_style(
                        row_rect,
                        Style::default().bg(crate::gradient::rgb_color(tint)),
                    );
                } else if let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
                {
                    buf.set_style(row_rect, Style::default().bg(bg));
                }
            } else {
                // Black theme: the selected/marked rows wear the brand's
                // muted dark-teal fill (identical to the right-click menu's
                // selection), replacing the legacy bright-blue accent. Croft
                // Dark keeps the historical blue. The drop target stays
                // green either way.
                let (sel_bg, mark_bg) = if self.focus_gradient {
                    (
                        crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG),
                        self.theme.ui(Color::Rgb(0x18, 0x35, 0x32)),
                    )
                } else {
                    (
                        self.theme.ui(Color::Rgb(0x09, 0x4d, 0x77)),
                        self.theme.ui(Color::Rgb(0x07, 0x33, 0x55)),
                    )
                };
                let line_style = if is_drop_target {
                    Style::default().bg(drop_bg)
                } else if is_selected {
                    Style::default().bg(sel_bg)
                } else if is_marked {
                    Style::default().bg(mark_bg)
                } else if let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
                {
                    Style::default().bg(bg)
                } else {
                    Style::default()
                };
                buf.set_style(row_rect, line_style);
            }
            // A changed row's status sits at the right edge (#1513): the
            // file's letter, or a dot on a folder with changes inside. The
            // name is clipped short of it so a long name never covers it.
            let badge = self
                .git_decoration(&node.path)
                .filter(|_| row_width >= 8 && !self.is_ignored(&node.path));
            match badge {
                Some(dec) => {
                    let glyph = if node.is_dir {
                        '\u{25cf}'
                    } else {
                        dec.letter()
                    };
                    buf.set_line(inner.x, y, &line, row_width - 3);
                    buf.set_string(
                        inner.x + row_width - 2,
                        y,
                        glyph.to_string(),
                        Style::default().fg(self.theme.git_decoration_fg(dec)),
                    );
                }
                None => {
                    buf.set_line(inner.x, y, &line, row_width);
                }
            }
        }
        // Explorer header toolbar: New File / New Folder / Refresh / Collapse
        // Folders, right-aligned on the root folder row (the "croft" header),
        // mirroring VS Code's view actions, which live on the workspace-folder
        // header rather than the EXPLORER title. Revealed only while the
        // Explorer pane is focused and the root row sits at the top of the
        // viewport; hidden otherwise (VS Code's hover/focus reveal). Painted
        // after the rows so the pills win over the root row's text/fill.
        if self.focused && self.scroll == 0 && !self.nodes.is_empty() {
            use crate::widgets::header_pill;
            const GLYPHS: [char; 4] = [
                header_pill::NEW_FILE_GLYPH,
                header_pill::NEW_FOLDER_GLYPH,
                header_pill::REFRESH_GLYPH,
                header_pill::COLLAPSE_ALL_GLYPH,
            ];
            // Each pill is a single-cell glyph; `step` adds a one-cell gap so
            // the chips read as four distinct buttons. `right_pad` keeps the
            // block clear of the scrollbar / right border.
            let step: u16 = 2;
            let count = GLYPHS.len() as u16;
            let block_w = count * step - 1;
            let right_pad: u16 = 1;
            // Approximate the root label footprint (chevron + space + folder
            // icon + space + name) so a narrow panel withholds the toolbar
            // rather than overprinting the folder name.
            let root_name = self.nodes[0]
                .path
                .file_name()
                .map(|n| n.to_string_lossy().chars().count())
                .unwrap_or(0) as u16;
            let label_w = 4 + root_name;
            if row_width > label_w + block_w + right_pad + 2 {
                let start_x = inner.x + row_width - right_pad - block_w;
                let y = inner.y;
                for (i, &glyph) in GLYPHS.iter().enumerate() {
                    let x = start_x + i as u16 * step;
                    let rect = Rect {
                        x,
                        y,
                        width: step,
                        height: 1,
                    };
                    let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
                    header_pill::render(buf, x, y, glyph, self.theme, hovered);
                    match i {
                        0 => self.header_new_file_btn = rect,
                        1 => self.header_new_folder_btn = rect,
                        2 => self.header_refresh_btn = rect,
                        _ => self.header_collapse_btn = rect,
                    }
                }
            }
        }
        // Sticky band (#117): pin the off-screen ancestor directories of the
        // top visible row, overpainting the topmost content rows — the
        // editor sticky band's model, including its guard: never cover the
        // selected row (the scroll clamp above keeps it on screen, so its
        // band-relative row bounds the band's height).
        if scroll_pos > 0 {
            const STICKY_MAX: usize = 3;
            let top_idx = visible[scroll_pos];
            let chain = sticky_ancestors(&self.nodes, top_idx, STICKY_MAX);
            let sel_row = sel_pos.saturating_sub(scroll_pos);
            let band = chain
                .len()
                .min(sel_row)
                .min(visible_height.saturating_sub(1));
            let shown = &chain[chain.len() - band..];
            // Croft Loch lifts the band onto the raised tone: the theme's
            // sticky tint is a hair off the chrome and vanished against it.
            let bg = tones.map_or_else(|| self.theme.sticky_scroll_bg(), |t| t.raised());
            let mark_fg = tones.map_or_else(|| self.theme.ui(Color::Gray), |t| t.muted());
            for (i, &aidx) in shown.iter().enumerate() {
                let y = inner.y + i as u16;
                for x in inner.x..inner.x + row_width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
                let node = &self.nodes[aidx];
                let name = node.label();
                let name_fg = self.name_fg(&node.path);
                let name_style = if tones.is_some() && node.depth > 0 {
                    Style::default().fg(name_fg).bg(bg)
                } else {
                    Style::default()
                        .fg(name_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                };
                let spans = vec![
                    Span::raw("  ".repeat(node.depth)),
                    Span::styled(
                        format!("{} ", crate::widgets::surface::chevron(self.theme, true)),
                        Style::default().fg(mark_fg).bg(bg),
                    ),
                    Span::styled(
                        format!("{} ", icons::FOLDER_OPEN.glyph),
                        Style::default()
                            .fg(self.folder_icon_fg(icons::FOLDER_OPEN.color))
                            .bg(bg),
                    ),
                    Span::styled(name, name_style),
                ];
                buf.set_line(inner.x, y, &Line::from(spans), row_width);
                self.sticky_rows.push((y, aidx));
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical_on_chrome(buf, metrics, self.focused, self.theme);
        }
    }
}
