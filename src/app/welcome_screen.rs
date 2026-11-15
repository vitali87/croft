//! The welcome page (logo picture and its bake, wordmark, Start list, release-note card,
//! Loch layout) and the guided tour.

use super::*;

/// Cells reserved at the left of each highlight row for the kind glyph plus
/// a one-cell gap before the summary text.
pub(super) const WELCOME_NOTE_GUTTER: u16 = 2;

pub(super) fn welcome_note_summary_width(block_w: u16) -> u16 {
    block_w.saturating_sub(WELCOME_NOTE_GUTTER).max(1)
}

/// A note's summary wrapped to the card, its inline Markdown markers already
/// removed (#954) so the height measure and the paint wrap the same text.
pub(super) fn wrapped_welcome_note_summary(
    note: &crate::release_notes::ReleaseNote,
    block_w: u16,
) -> Vec<String> {
    let w = welcome_note_summary_width(block_w);
    let (text, _) = crate::release_notes::inline_markup(note.summary);
    wrap_cells_variable_width(&text, w, w)
}

/// The wrapped `lines` of a note as styled lines (#954): wrapping only moves
/// and collapses whitespace, so the non-space characters of `lines` are those
/// of the summary's text in order, and each takes its own style back.
pub(super) fn styled_welcome_note_lines(
    note: &crate::release_notes::ReleaseNote,
    lines: &[String],
    plain: Style,
    code: Style,
) -> Vec<ratatui::text::Line<'static>> {
    use crate::release_notes::Inline;
    let (text, styles) = crate::release_notes::inline_markup(note.summary);
    let mut marks = text
        .chars()
        .zip(styles)
        .filter(|(c, _)| !c.is_whitespace())
        .map(|(_, s)| s);
    lines
        .iter()
        .map(|line| {
            let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut run_style = plain;
            for c in line.chars() {
                let style = if c.is_whitespace() {
                    plain
                } else {
                    match marks.next().unwrap_or(Inline::Plain) {
                        Inline::Plain => plain,
                        Inline::Bold => plain.add_modifier(Modifier::BOLD),
                        Inline::Code => code,
                    }
                };
                if style != run_style && !run.is_empty() {
                    spans.push(ratatui::text::Span::styled(
                        std::mem::take(&mut run),
                        run_style,
                    ));
                }
                run_style = style;
                run.push(c);
            }
            if !run.is_empty() {
                spans.push(ratatui::text::Span::styled(run, run_style));
            }
            ratatui::text::Line::from(spans)
        })
        .collect()
}

pub(super) fn welcome_note_row_height(
    note: &crate::release_notes::ReleaseNote,
    block_w: u16,
) -> u16 {
    wrapped_welcome_note_summary(note, block_w).len().max(1) as u16
}

/// Width of the release-note card's content area for a card spanning
/// `block_w` cells: one border cell plus a one-cell inset on each side.
/// The height measure and the paint pass MUST both wrap through this —
/// measuring 2 cells wider than the paint made the paint produce more
/// wrapped lines than the card reserved rows for, and the overflow rows
/// were silently dropped, ending the note mid-sentence (#99).
pub(super) fn welcome_card_inner_width(block_w: u16) -> u16 {
    block_w.saturating_sub(4)
}

pub(super) fn welcome_recents_height(
    notes: &[crate::release_notes::ReleaseNote],
    block_w: u16,
) -> u16 {
    if notes.is_empty() {
        return 0;
    }
    // Header row + one blank row + each highlight (which may wrap).
    let notes_h: u16 = notes
        .iter()
        .map(|n| welcome_note_row_height(n, block_w))
        .sum();
    1 + 1 + notes_h
}

/// The blank rows between the blocks of Croft Loch's welcome stack (the
/// wordmark, the tagline, the Start list, the release card): one, the same
/// everywhere, so the page keeps one rhythm at any height.
pub(super) const LOCH_WELCOME_GAP: u16 = 1;

/// The rows of Croft Loch's release card around its notes: the top edge, the
/// heading, the blank under it, the row under the notes (padding, or the
/// "+N more" line when the card is shortened) and the bottom edge.
pub(super) const LOCH_WELCOME_CARD_FRAME: u16 = 5;

/// Blank cells between the `cr<>ft` wordmark and the version pill beside it
/// on Croft Loch's welcome page: close enough to read as one group, apart
/// enough that the pill never looks like part of the name.
pub(super) const WELCOME_VERSION_GAP: u16 = 2;

/// The room between the widest Start label and the keycaps' column, so the
/// widest row still reads as a label and a chord, not one run of text, while
/// every chord stays close enough to pair with its label.
pub(super) const WELCOME_START_KEY_GAP: u16 = 4;

/// Croft Loch's Start list columns, from its widest label and widest
/// keycaps: where the keycaps start, as an offset from the list's left edge
/// (one fixed column [`WELCOME_START_KEY_GAP`] cells past the widest label,
/// so each chord sits beside its label and the chords line up), and the
/// list's width, only what the labels and keycaps need. Spreading the list
/// over the card's whole text column left the chords some seventy cells
/// from their labels, where nothing tied a chord to its row.
pub(super) fn welcome_start_columns(label_w: u16, caps_w: u16) -> (u16, u16) {
    let keys_dx = label_w + WELCOME_START_KEY_GAP;
    (
        keys_dx,
        if caps_w > 0 {
            keys_dx + caps_w
        } else {
            label_w
        },
    )
}

/// Lay out Croft Loch's welcome stack in `height` rows, in priority order:
/// the logo (one row for the text wordmark, the picture's 4-row floor
/// otherwise), the tagline, the Start list (`start_rows` action rows under a
/// heading), then the release card, which is shortened to the notes that fit
/// whole (`note_heights`, each note's wrapped rows) rather than pushing the
/// Start list out. When the Start list has no room, a one-row tour chip
/// (`tour_chip`) takes its place. The picture grows (to 14 rows) only once
/// every note fits, and the whole stack is centred vertically, its blocks
/// [`LOCH_WELCOME_GAP`] apart.
pub(super) fn loch_welcome_plan(
    height: u16,
    text_logo: bool,
    start_rows: u16,
    tour_chip: bool,
    note_heights: &[u16],
) -> LochWelcomePlan {
    let gap = LOCH_WELCOME_GAP;
    let logo_floor = if text_logo { 1 } else { 4 };
    // The rows each optional block adds, its leading gap included.
    let tagline = gap + 1;
    let start = if start_rows > 0 {
        gap + 1 + start_rows
    } else {
        0
    };
    let chip = if tour_chip { gap + 1 } else { 0 };
    let (start, chip) = if start > 0 && logo_floor + tagline + start <= height {
        (start, 0)
    } else if chip > 0 && logo_floor + tagline + chip <= height {
        (0, chip)
    } else {
        (0, 0)
    };
    let above_card = tagline + start + chip;
    let full_card = if note_heights.is_empty() {
        0
    } else {
        gap + LOCH_WELCOME_CARD_FRAME + note_heights.iter().sum::<u16>()
    };
    let logo_h = if text_logo {
        1
    } else {
        height
            .saturating_sub(above_card)
            .saturating_sub(full_card)
            .clamp(4, 14)
    };
    let card_room = height.saturating_sub(logo_h + above_card + gap);
    let (mut notes, mut card_h) = (0, 0);
    let mut h = LOCH_WELCOME_CARD_FRAME;
    for &nh in note_heights {
        if h + nh > card_room {
            break;
        }
        h += nh;
        notes += 1;
        card_h = h;
    }
    let total = logo_h + above_card + if notes > 0 { gap + card_h } else { 0 };
    let logo_y = height.saturating_sub(total) / 2;
    let tagline_y = logo_y + logo_h + gap;
    let next = tagline_y + 1 + gap;
    LochWelcomePlan {
        logo_y,
        logo_h,
        tagline_y,
        start_y: (start > 0).then_some(next),
        chip_y: (chip > 0).then_some(next),
        card_y: tagline_y + 1 + start + chip + gap,
        card_h,
        notes,
    }
}

pub(super) const WELCOME_TAGLINE: &str = "LIGHTWEIGHT.  BLAZINGLY FAST.  BUILT FOR DEVELOPERS.";

impl App {
    /// Returns true exactly once after the welcome OSC-1337 image has been
    /// emitted and the editor pane has stopped being blank (i.e. a file is
    /// now open). The caller — the main draw loop — must respond by
    /// invalidating the prev buffer so ratatui repaints every cell on the
    /// next draw, wiping iTerm's image cache for the welcome region.
    pub fn consume_welcome_image_clear(&mut self) -> bool {
        let opened_file = !self.editor.is_blank_initial();
        self.overlays.welcome.consume_clear_or(opened_file)
    }

    pub(super) fn render_welcome(&mut self, frame: &mut ratatui::Frame, outer_area: Rect) {
        // A welcome screen with no rows (the panel maximized over it) paints
        // no layout icons, so it must not keep the last frame's: their cells
        // now belong to the panel's tab row and its buttons (#1980).
        self.layout_icon_areas = LayoutIconAreas::default();
        // Frame truth for the welcome's click targets: set again below by
        // whatever this frame paints.
        self.welcome_tour_button = Rect::default();
        self.welcome.clear_start_rows();
        if outer_area.width == 0 || outer_area.height == 0 {
            return;
        }
        self.render_welcome_panel(frame, outer_area);
        // The first-launch tour (#377), until it has been taken or skipped.
        // Croft Loch places it in its own stack (a Start row, or a chip in
        // the Start list's place), so this bottom button is the boxed
        // themes' alone.
        if crate::widgets::surface::tones(self.theme).is_none()
            && !self.tour_done
            && self.tour.is_none()
            && outer_area.height > 6
            && self.welcome_tour_button.width == 0
        {
            let label = " Take the Tour ";
            let w = label.chars().count() as u16;
            if outer_area.width > w + 4 {
                let rect = Rect {
                    x: outer_area.x + (outer_area.width - w) / 2,
                    y: outer_area.y + outer_area.height - 3,
                    width: w,
                    height: 1,
                };
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(label).style(
                        Style::default()
                            .fg(self.theme.accent_contrast_fg())
                            .bg(self.theme.accent())
                            .add_modifier(Modifier::BOLD),
                    ),
                    rect,
                );
                self.welcome_tour_button = rect;
            }
        }
    }

    /// Croft Loch's welcome tagline: the same promise as [`WELCOME_TAGLINE`]
    /// in sentence case, set quietly in the muted ink under the wordmark.
    pub(super) const WELCOME_TAGLINE_CALM: &'static str =
        "Lightweight. Blazingly fast. Built for developers.";

    /// The chord shown beside `cmd` on the welcome screen's Start list,
    /// exactly as the Command Palette shows it: a user's own binding, no
    /// chord when a user binding took the built-in one, else the built-in
    /// hint as this platform spells it.
    pub(super) fn welcome_chord(&self, cmd: crate::widgets::command_palette::Command) -> String {
        match self.keymap.chord_for(cmd) {
            Some(user) => crate::keymap::display_chord(&user),
            None if self.keymap.shadows_builtin(cmd) => String::new(),
            None => {
                crate::widgets::command_palette::platform_hint(cmd.keybinding_hint()).into_owned()
            }
        }
    }

    /// The Start list Croft Loch's welcome screen offers, top to bottom:
    /// each row's label, the chord beside it, and what a click runs (`None`
    /// for the tour, whose click target is `welcome_tour_button`). The tour
    /// is offered until it has been taken or skipped.
    pub(super) fn welcome_start_rows(
        &self,
    ) -> Vec<(&'static str, String, Option<welcome::StartAction>)> {
        use crate::widgets::command_palette::Command;
        use welcome::StartAction;
        let mut rows = vec![
            (
                "New File",
                self.welcome_chord(Command::NewUntitledFile),
                Some(StartAction::Command(Command::NewUntitledFile)),
            ),
            (
                "Go to File",
                self.welcome_chord(Command::QuickOpen),
                Some(StartAction::Command(Command::QuickOpen)),
            ),
            (
                "Command Palette",
                crate::widgets::command_palette::platform_hint("Cmd+Shift+P").into_owned(),
                Some(StartAction::CommandPalette),
            ),
        ];
        if !self.tour_done && self.tour.is_none() {
            rows.push(("Take the Tour", String::new(), None));
        }
        rows
    }

    /// Run the welcome Start row a click landed on, if the welcome screen is
    /// what the editor area shows.
    pub(super) fn click_welcome_start(&mut self, x: u16, y: u16) -> bool {
        if self.editor_layout.is_split() || !self.editor.is_blank_initial() {
            return false;
        }
        match self.welcome.start_action_at(x, y) {
            Some(welcome::StartAction::Command(cmd)) => self.run_command(cmd),
            Some(welcome::StartAction::CommandPalette) => self.open_command_palette(),
            None => return false,
        }
        true
    }

    pub(super) fn render_welcome_panel(&mut self, frame: &mut ratatui::Frame, outer_area: Rect) {
        // In iTerm2 image mode we let the iTerm session bg (forced to
        // sRGB(EDITOR_BG_RGB) by `SetColors=bg=srgb:…` at startup) show
        // through the welcome cells via SGR 49 (default bg). The OSC-1337
        // PNG canvas is filled with the same sRGB hex, so both surfaces
        // display the same physical pixel — no Generic-RGB vs sRGB seam.
        // Outside iTerm we fall back to explicit truecolor since there's
        // no PNG to match against.
        let bg = if crate::iterm2_inline::detect_iterm2_inline_support() {
            Style::default().bg(Color::Reset)
        } else {
            Style::default().bg(self.theme.editor_bg())
        };
        // Give the welcome area the same visible envelope as the editor
        // pane when a file is open: a full bordered box on a boxed theme
        // (Borders::ALL on the editor widget), padding on the canvas under
        // a `surfaces` theme, whose seams then mark where the side bar and
        // the panel meet it. Without the box, a boxed theme's welcome bg
        // bleeds into the tree, the terminal, and the window edge with no
        // perceptible seam — and the sidebar splitter looks unreachable.
        // The bg style is applied to the same block so the inside is filled
        // in one pass. The welcome content then renders into
        // `block.inner(outer_area)`, the same inset either way, so a tall
        // recents list can never paint over the frame rows.
        let outer_block = crate::widgets::surface::view_block(
            self.theme,
            false,
            Style::default().fg(Color::DarkGray),
        )
        .style(bg);
        let area = outer_block.inner(outer_area);
        frame.render_widget(outer_block, outer_area);
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Layout-control icons at the welcome screen's top-right, inside the
        // border row — same controls as the editor tab strip (placement A).
        let icon_row = Rect {
            x: outer_area.x + 1,
            y: outer_area.y,
            width: outer_area.width.saturating_sub(2),
            height: 1,
        };
        self.paint_layout_icons(frame, icon_row);

        // Layout regions, top-to-bottom:
        //   [logo + version badge]
        //   [tagline]
        //   [gradient box: header + release highlights]
        //   [footer chevron line]
        let block_left = area.x + area.width / 8;
        let block_right = area.x + area.width - area.width / 8;
        let block_w = block_right.saturating_sub(block_left);
        // Croft Loch composes its own page: one centred column with a Start
        // list, the release card shortened rather than the list dropped.
        if let Some(t) = crate::widgets::surface::tones(self.theme) {
            self.render_welcome_loch(frame, area, t, block_left, block_w);
            return;
        }
        let has_recent_panel = self.welcome.has_recent_panel();
        let recents_inner_h =
            welcome_recents_height(self.welcome.notes(), welcome_card_inner_width(block_w));
        let tagline_h = 1u16;
        let footer_h = 1u16;
        // Gaps: blank row after logo, after tagline, after box.
        let gaps_h = 3u16;
        let desired_card_h = if has_recent_panel {
            recents_inner_h.saturating_add(4)
        } else {
            0
        };

        let logo_max_w = (area.width as u32).saturating_sub(4) as u16;
        let logo_w_cells = logo_max_w.clamp(8, 48);
        // Box content needs at least the 4-cell border+inset envelope to be
        // worth drawing.
        let desired_box_h = desired_card_h;
        // Size the card first, then give the logo whatever remains (still
        // capped to its 4..14 band, so the stack can never overrun the
        // fixed-size ratatui buffer). The release note is the panel's
        // information and the logo is decoration: in a height-starved
        // welcome area (a 40-row window with the terminal panel open) the
        // old logo-first order kept a full 14-row logo above a card that
        // was clipped mid-sentence or dropped entirely (#99).
        let logo_h_cells = area
            .height
            .saturating_sub(tagline_h + footer_h + gaps_h)
            .saturating_sub(desired_box_h)
            .clamp(4, 14);
        // logo, gap, tagline, gap
        let used_above_box = logo_h_cells + 1 + tagline_h + 1;
        let used_below_box = 1 + footer_h; // gap, footer
        let max_box_h = area
            .height
            .saturating_sub(used_above_box)
            .saturating_sub(used_below_box);
        let box_h = desired_box_h.min(max_box_h);

        let total_h = used_above_box + box_h + used_below_box;
        let block_top = area.y + area.height.saturating_sub(total_h) / 2;
        let logo_x = area.x + area.width.saturating_sub(logo_w_cells) / 2;
        let logo_y = block_top;
        let area_max_y = area.y + area.height;

        self.place_welcome_logo(WelcomeLayout {
            cell_x: logo_x,
            cell_y: logo_y,
            cell_w: logo_w_cells,
            cell_h: logo_h_cells,
        });

        if !self.overlays.welcome.has_image() && self.welcome_bake.is_none() {
            // Text fallback for non-iTerm2 terminals.
            let label = " croft ";
            let lx = logo_x
                .saturating_add(logo_w_cells.saturating_sub(label.chars().count() as u16) / 2);
            let ly = logo_y + logo_h_cells / 2;
            frame.buffer_mut().set_string(
                lx,
                ly,
                label,
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)))
                    .add_modifier(Modifier::BOLD),
            );
        }

        // Version badge to the right of the wordmark (the wordmark sits
        // in the lower portion of the logo PNG; the badge tracks that row).
        // Plain `vX.Y.Z` (#282). The hash told two builds of one version
        // apart; the drift banner already does that, with an actionable
        // message, at the moment it matters.
        //
        // It never shares a cell with the logo or the tagline at any size
        // (#1843): it starts past the logo's cells, which the image fills
        // edge to edge, and its three rows end on the logo's last row, so
        // the tagline below (whose last column reaches the badge's first)
        // can never paint over its corner. Where there is no room beside
        // the logo, the version goes in the blank row under it instead.
        let version_label = format!(" v{} ", env!("CARGO_PKG_VERSION"));
        let version_w = version_label.chars().count() as u16;
        let badge_x = logo_x + logo_w_cells + 1;
        let badge_y = logo_y + ((logo_h_cells * 5) / 6).min(logo_h_cells.saturating_sub(3));
        let gap_y = logo_y + logo_h_cells;
        let badge_fits = badge_x + version_w + 2 <= area.x + area.width && badge_y + 2 < area_max_y;
        // Clear of the Take the Tour button, two rows above the border.
        if !badge_fits && gap_y + 2 < area_max_y && version_w <= area.width {
            frame.buffer_mut().set_string(
                area.x + (area.width - version_w) / 2,
                gap_y,
                &version_label,
                Style::default()
                    .fg(self.theme.ui(rgb_color(GRAD_TL)))
                    .add_modifier(Modifier::BOLD),
            );
        }
        if badge_fits {
            let badge_style = Style::default()
                .fg(self.theme.ui(rgb_color(GRAD_TL)))
                .add_modifier(Modifier::BOLD);
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y, "\u{256d}", badge_style);
            for i in 0..version_w {
                frame
                    .buffer_mut()
                    .set_string(badge_x + 1 + i, badge_y, "\u{2500}", badge_style);
            }
            frame.buffer_mut().set_string(
                badge_x + 1 + version_w,
                badge_y,
                "\u{256e}",
                badge_style,
            );
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y + 1, "\u{2502}", badge_style);
            frame.buffer_mut().set_string(
                badge_x + 1,
                badge_y + 1,
                &version_label,
                Style::default()
                    .fg(self.theme.ui(Color::White))
                    .add_modifier(Modifier::BOLD),
            );
            frame.buffer_mut().set_string(
                badge_x + 1 + version_w,
                badge_y + 1,
                "\u{2502}",
                badge_style,
            );
            frame
                .buffer_mut()
                .set_string(badge_x, badge_y + 2, "\u{2570}", badge_style);
            for i in 0..version_w {
                frame.buffer_mut().set_string(
                    badge_x + 1 + i,
                    badge_y + 2,
                    "\u{2500}",
                    badge_style,
                );
            }
            frame.buffer_mut().set_string(
                badge_x + 1 + version_w,
                badge_y + 2,
                "\u{256f}",
                badge_style,
            );
        }

        let tagline_y = logo_y + logo_h_cells + 1;
        let tagline_style = Style::default().fg(self.theme.ui(Color::Rgb(0x88, 0xc0, 0xd0)));
        let tagline_w = WELCOME_TAGLINE.chars().count() as u16;
        let tagline_x = area.x + area.width.saturating_sub(tagline_w) / 2;
        if tagline_y < area_max_y && tagline_w <= area.width {
            frame
                .buffer_mut()
                .set_string(tagline_x, tagline_y, WELCOME_TAGLINE, tagline_style);
        }

        let box_y = tagline_y + tagline_h + 1;
        if has_recent_panel && box_h >= 4 && block_w >= 4 && box_y + box_h <= area_max_y {
            let box_rect = Rect {
                x: block_left,
                y: box_y,
                width: block_w,
                height: box_h,
            };
            if self.theme.is_light() {
                // The brand gradient's teal leg washes out on white; paint
                // the card border in the light widget-border grey instead.
                let border = Style::default().fg(self.theme.ui(Color::Rgb(0x3b, 0x42, 0x52)));
                let b = frame.buffer_mut();
                for x in box_rect.x..box_rect.x + box_rect.width {
                    b.set_string(x, box_rect.y, "\u{2500}", border);
                    b.set_string(x, box_rect.y + box_rect.height - 1, "\u{2500}", border);
                }
                for y in box_rect.y..box_rect.y + box_rect.height {
                    b.set_string(box_rect.x, y, "\u{2502}", border);
                    b.set_string(box_rect.x + box_rect.width - 1, y, "\u{2502}", border);
                }
                b.set_string(box_rect.x, box_rect.y, "\u{256d}", border);
                b.set_string(
                    box_rect.x + box_rect.width - 1,
                    box_rect.y,
                    "\u{256e}",
                    border,
                );
                b.set_string(
                    box_rect.x,
                    box_rect.y + box_rect.height - 1,
                    "\u{2570}",
                    border,
                );
                b.set_string(
                    box_rect.x + box_rect.width - 1,
                    box_rect.y + box_rect.height - 1,
                    "\u{256f}",
                    border,
                );
            } else {
                paint_gradient_box(frame.buffer_mut(), box_rect);
            }

            // Inner content area: 1-cell inset from each border. The width
            // comes from the same helper the height measure used, so the
            // wrap below reproduces exactly the line count the card
            // reserved rows for (#99).
            let inner_x = box_rect.x + 2;
            let inner_y = box_rect.y + 1;
            let inner_w_actual = welcome_card_inner_width(box_rect.width);

            let row_style = Style::default().fg(self.theme.ui(Color::Rgb(0xc5, 0xcd, 0xd9)));

            // Header row: "> IN THIS RELEASE (vX.Y.Z)", or "> IN THIS BUILD
            // (vX.Y.Z+)" for a build carrying changes past that release. The
            // leading chevron echoes the cr<>ft wordmark's bracket. The
            // highlights below are hand-curated data baked into this build
            // (see `crate::release_notes`), so the heading names the exact
            // version.
            let header_y = inner_y;
            frame.buffer_mut().set_string(
                inner_x,
                header_y,
                "> ",
                Style::default()
                    .fg(self.theme.ui(rgb_color(GRAD_TR)))
                    .add_modifier(Modifier::BOLD),
            );
            frame.buffer_mut().set_string(
                inner_x + 2,
                header_y,
                crate::release_notes::heading(
                    crate::release_notes::unreleased(),
                    env!("CARGO_PKG_VERSION"),
                ),
                Style::default()
                    .fg(self.theme.ui(Color::White))
                    .add_modifier(Modifier::BOLD),
            );

            // Each highlight: a kind glyph in the gutter (rocket = new feature,
            // bug = fix) tinted by kind, then the wrapped one-line summary.
            // Continuation lines align under the summary, leaving the gutter
            // clear so the glyph reads as a single bullet.
            let summary_x = inner_x + WELCOME_NOTE_GUTTER;
            let summary_w = inner_w_actual.saturating_sub(WELCOME_NOTE_GUTTER);
            let last_row = box_rect.y + box_rect.height - 1;
            let mut row_y = header_y + 2;
            for note in self.welcome.notes() {
                if row_y >= last_row {
                    break;
                }
                let (mark, mark_fg) = note.kind.marker_for(self.theme);
                frame.buffer_mut().set_string(
                    inner_x,
                    row_y,
                    mark,
                    Style::default().fg(mark_fg).add_modifier(Modifier::BOLD),
                );
                let lines = wrapped_welcome_note_summary(note, inner_w_actual);
                let code_style = row_style.fg(self.theme.ui(crate::markdown::CODE));
                let styled = styled_welcome_note_lines(note, &lines, row_style, code_style);
                for (line_idx, line) in styled.iter().enumerate() {
                    let line_y = row_y + line_idx as u16;
                    if line_y >= last_row {
                        break;
                    }
                    frame
                        .buffer_mut()
                        .set_line(summary_x, line_y, line, summary_w);
                }
                row_y = row_y.saturating_add(lines.len().max(1) as u16);
            }
        }
    }

    /// Re-bake the welcome logo's picture whenever its layout shifts
    /// (resize, sidebar toggle, font size change). The canvas is filled with
    /// the raw `EDITOR_BG_RGB` bytes interpreted as sRGB (iTerm2 decodes PNG
    /// bytes as sRGB by default). The pane's SGR-painted cells use
    /// `Color::Reset` so they fall back to the iTerm session bg, which we
    /// force to the same sRGB hex via `SetColors=bg=srgb:…` at startup. Both
    /// surfaces therefore display the same physical pixel pair-for-pair.
    pub(super) fn place_welcome_logo(&mut self, desired: WelcomeLayout) {
        if !self.overlays.welcome.layout_matches(&desired) {
            // The picture baked for the old layout is the wrong size for
            // this one. Its replacement bakes off the UI thread (#1973), and
            // the logo's cells stay blank until it lands; one still baking
            // for an older layout is followed up when it lands.
            self.overlays.welcome.disable();
            self.overlays.welcome.set_layout(desired);
            self.overlays.welcome.mark_dirty();
            if self.welcome_bake.is_none()
                && let Some(key) = self.welcome_bake_key()
            {
                self.start_welcome_bake(key);
            }
        }
    }

    /// Croft Loch's welcome page inside the padding `render_welcome_panel`
    /// painted: one column of blocks centred on the area's axis, laid out by
    /// [`loch_welcome_plan`] in priority order. The `cr<>ft` wordmark with
    /// the version pill two cells after it (the picture logo keeps the
    /// badge beside it), the calm tagline, the Start list, and the release
    /// card, shortened to the notes that fit whole with a "+N more" line
    /// rather than pushing the Start list out of a short editor.
    pub(super) fn render_welcome_loch(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
        t: crate::widgets::surface::Tones,
        block_left: u16,
        block_w: u16,
    ) {
        use crate::widgets::surface::keycap_spans;
        let area_max_y = area.y + area.height;
        // The wordmark is the logo for good when no picture can be shown (no
        // image protocol, or no cell size to bake one for), and it needs one
        // row where the picture needs four. A picture that fails to bake
        // falls back to the wordmark inside the picture's rows.
        let text_logo = !self.inline_images_enabled() || self.cell_pixel.is_none();
        let card_inner_w = welcome_card_inner_width(block_w);
        let note_heights: Vec<u16> = if block_w >= 4 {
            self.welcome
                .notes()
                .iter()
                .map(|n| welcome_note_row_height(n, card_inner_w))
                .collect()
        } else {
            Vec::new()
        };

        // The Start list hangs from the release card's text column, so
        // "Start" and "What's new" share a left edge, and is only as wide as
        // its labels and keycaps need, the keycaps in one column a fixed gap
        // past the widest label ([`welcome_start_columns`]). Where the
        // card's column cannot hold it, it centres on the axis instead. It
        // shows only where it and a cell of hover lift either side fit.
        let start_rows = self.welcome_start_rows();
        let start_label_w = start_rows
            .iter()
            .map(|(label, _, _)| label.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let start_caps_w = start_rows
            .iter()
            .map(|(_, chord, _)| {
                ratatui::text::Line::from(keycap_spans(chord, t, t.canvas(), false)).width() as u16
            })
            .max()
            .unwrap_or(0);
        let (start_keys_dx, start_col_w) = welcome_start_columns(start_label_w, start_caps_w);
        let start_fits = start_col_w + 2 <= area.width;
        let tour_offered = !self.tour_done && self.tour.is_none();
        let plan = loch_welcome_plan(
            area.height,
            text_logo,
            if start_fits {
                start_rows.len() as u16
            } else {
                0
            },
            tour_offered,
            &note_heights,
        );

        // The logo: the wordmark and the version pill as one group centred
        // on the axis, or the picture's cells with the badge beside them.
        let version_label = format!(" v{} ", env!("CARGO_PKG_VERSION"));
        let version_w = version_label.chars().count() as u16;
        let pill_w = version_w + 2;
        let mark_w = 6; // "cr<>ft"
        let logo_y = area.y + plan.logo_y;
        let logo_w_cells = ((area.width as u32).saturating_sub(4) as u16).clamp(8, 48);
        let (logo, pill_x) = if text_logo {
            let group_w = mark_w + WELCOME_VERSION_GAP + pill_w;
            let (mark_x, pill_x) = if group_w <= area.width {
                let x = area.x + (area.width - group_w) / 2;
                (x, Some(x + mark_w + WELCOME_VERSION_GAP))
            } else {
                (area.x + area.width.saturating_sub(mark_w) / 2, None)
            };
            let logo = WelcomeLayout {
                cell_x: mark_x,
                cell_y: logo_y,
                cell_w: mark_w,
                cell_h: 1,
            };
            (logo, pill_x)
        } else {
            let logo = WelcomeLayout {
                cell_x: area.x + area.width.saturating_sub(logo_w_cells) / 2,
                cell_y: logo_y,
                cell_w: logo_w_cells,
                cell_h: plan.logo_h,
            };
            (logo, None)
        };
        self.place_welcome_logo(logo);

        if !self.overlays.welcome.has_image() && self.welcome_bake.is_none() {
            // The wordmark itself: the letters in the strong ink and the
            // `<>` (the stylised "o") in the brand orange, the one place that
            // orange appears.
            let letter = Style::default().fg(t.strong()).add_modifier(Modifier::BOLD);
            let mark = ratatui::text::Line::from(vec![
                Span::styled("cr", letter),
                Span::styled(
                    "<>",
                    Style::default()
                        .fg(BRAND_ORANGE)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("ft", letter),
            ]);
            let lx = logo.cell_x + logo.cell_w.saturating_sub(mark_w) / 2;
            let ly = logo.cell_y + logo.cell_h / 2;
            frame.buffer_mut().set_line(lx, ly, &mark, mark_w);
        }

        // The version: metadata, not a headline. A one-row pill on the
        // raised tone with half-block caps, beside the wordmark. Beside the
        // picture it keeps the badge's place (#1843): past the logo's cells,
        // on the badge's middle row, or in the blank row under a logo too
        // wide to sit beside.
        let pill = |buf: &mut ratatui::buffer::Buffer, x: u16, y: u16| {
            let caps = Style::default().fg(t.raised());
            buf.set_string(x, y, "\u{2590}", caps);
            buf.set_string(
                x + 1,
                y,
                &version_label,
                Style::default().fg(t.muted()).bg(t.raised()),
            );
            buf.set_string(x + 1 + version_w, y, "\u{258c}", caps);
        };
        if let Some(x) = pill_x {
            pill(frame.buffer_mut(), x, logo_y);
        } else if !text_logo {
            let badge_x = logo.cell_x + logo.cell_w + 1;
            let badge_y = logo_y + ((logo.cell_h * 5) / 6).min(logo.cell_h.saturating_sub(3));
            let gap_y = logo_y + logo.cell_h;
            if badge_x + version_w + 2 <= area.x + area.width && badge_y + 2 < area_max_y {
                pill(frame.buffer_mut(), badge_x, badge_y + 1);
            } else if gap_y + 2 < area_max_y && version_w <= area.width {
                frame.buffer_mut().set_string(
                    area.x + (area.width - version_w) / 2,
                    gap_y,
                    &version_label,
                    Style::default().fg(t.muted()),
                );
            }
        }

        // The tagline in sentence case, muted and italic, so it reads as a
        // line under the wordmark rather than a second banner.
        let tagline_y = area.y + plan.tagline_y;
        let tagline = Self::WELCOME_TAGLINE_CALM;
        let tagline_w = tagline.chars().count() as u16;
        if tagline_y < area_max_y && tagline_w <= area.width {
            frame.buffer_mut().set_string(
                area.x + (area.width - tagline_w) / 2,
                tagline_y,
                tagline,
                Style::default()
                    .fg(t.muted())
                    .add_modifier(Modifier::ITALIC),
            );
        }

        // The Start list: a heading in the strong ink (a heading outranks the
        // rows it heads), then one row per action, its label in the body ink
        // and its chord as keycaps in the list's keycap column, read from
        // the keymap as the palette reads it. A row lifts under the pointer
        // (a cell of lift either side of the list) and runs its action on a
        // click; the lift is the row's click rect.
        if let Some(start_y) = plan.start_y {
            let start_y = area.y + start_y;
            // The card's text column starts two cells into the card (its
            // rim and a cell of inset); with no card under it, or a card too
            // narrow to hold the list, the list centres on the axis.
            let card_text_x = block_left + 2;
            let col_x = if plan.notes > 0 && start_col_w <= card_inner_w {
                card_text_x
            } else {
                area.x + (area.width - start_col_w) / 2
            };
            let buf = frame.buffer_mut();
            buf.set_string(
                col_x,
                start_y,
                "Start",
                Style::default().fg(t.strong()).add_modifier(Modifier::BOLD),
            );
            for (i, (label, chord, action)) in start_rows.iter().enumerate() {
                let y = start_y + 1 + i as u16;
                let row = Rect {
                    x: col_x - 1,
                    y,
                    width: start_col_w + 2,
                    height: 1,
                };
                let hovered = crate::widgets::hover::contains(row, self.pointer_cell);
                let row_bg = if hovered {
                    t.hover_on(t.canvas)
                } else {
                    t.canvas()
                };
                if hovered {
                    buf.set_style(row, Style::default().bg(row_bg));
                }
                buf.set_string(
                    col_x,
                    y,
                    label,
                    Style::default().fg(if hovered { t.strong() } else { t.text() }),
                );
                if !chord.is_empty() {
                    let caps = ratatui::text::Line::from(keycap_spans(chord, t, row_bg, false));
                    let caps_w = caps.width() as u16;
                    buf.set_line(col_x + start_keys_dx, y, &caps, caps_w);
                }
                match action {
                    Some(action) => self.welcome.push_start_row(row, *action),
                    None => self.welcome_tour_button = row,
                }
            }
        }

        // No room for the Start list: the tour as a calm chip in its place,
        // on the raised tone with half-block caps, lifting under the
        // pointer; the accent stays for focus and selection.
        if let Some(chip_y) = plan.chip_y {
            let label = " Take the Tour ";
            let w = label.chars().count() as u16;
            if w + 2 <= area.width {
                let rect = Rect {
                    x: area.x + (area.width - w) / 2,
                    y: area.y + chip_y,
                    width: w,
                    height: 1,
                };
                let hovered = crate::widgets::hover::contains(rect, self.pointer_cell);
                let fill = if hovered {
                    t.hover_on(t.raised)
                } else {
                    t.raised()
                };
                let buf = frame.buffer_mut();
                buf.set_string(rect.x - 1, rect.y, "\u{2590}", Style::default().fg(fill));
                buf.set_string(
                    rect.x,
                    rect.y,
                    label,
                    Style::default()
                        .fg(if hovered { t.strong() } else { t.text() })
                        .bg(fill),
                );
                buf.set_string(rect.right(), rect.y, "\u{258c}", Style::default().fg(fill));
                self.welcome_tour_button = rect;
            }
        }

        // The release card: a raised surface that floats on the canvas, its
        // rim all round with no accent lip (it never takes the keyboard),
        // holding the notes that fit whole and, when that is not all of
        // them, a muted "+N more" line in the row under them.
        if plan.notes == 0 {
            return;
        }
        let box_rect = Rect {
            x: block_left,
            y: area.y + plan.card_y,
            width: block_w,
            height: plan.card_h,
        };
        let buf = frame.buffer_mut();
        buf.set_style(box_rect, Style::default().bg(t.raised()));
        crate::widgets::surface::paint_quiet_frame(buf, box_rect, self.theme);
        let inner_x = box_rect.x + 2;
        let header_y = box_rect.y + 1;
        // "What's new" in the strong ink, bold, so the heading outranks the
        // notes under it; the version it describes right-aligned in the
        // muted ink, metadata said once.
        let unreleased = crate::release_notes::unreleased();
        let heading = if unreleased {
            "What's new in this build"
        } else {
            "What's new"
        };
        let version = format!(
            "v{}{}",
            env!("CARGO_PKG_VERSION"),
            if unreleased { "+" } else { "" }
        );
        buf.set_stringn(
            inner_x,
            header_y,
            heading,
            usize::from(card_inner_w),
            Style::default().fg(t.strong()).add_modifier(Modifier::BOLD),
        );
        let (heading_w, card_version_w) = (
            heading.chars().count() as u16,
            version.chars().count() as u16,
        );
        if card_inner_w >= heading_w + card_version_w + 2 {
            buf.set_string(
                inner_x + card_inner_w - card_version_w,
                header_y,
                &version,
                Style::default().fg(t.muted()),
            );
        }
        // Each note: its typographic mark (`+` a feature, `•` a fix) in the
        // gutter, then its summary wrapped to the card, continuation lines
        // under the summary. The plan measured these very wraps, so every
        // note shown ends inside the card (#99).
        let row_style = Style::default().fg(t.text());
        let code_style = row_style.fg(self.theme.ui(crate::markdown::CODE));
        let summary_x = inner_x + WELCOME_NOTE_GUTTER;
        let summary_w = card_inner_w.saturating_sub(WELCOME_NOTE_GUTTER);
        let notes = self.welcome.notes();
        let mut row_y = header_y + 2;
        for note in &notes[..plan.notes] {
            let (mark, mark_fg) = note.kind.marker_for(self.theme);
            buf.set_string(inner_x, row_y, mark, Style::default().fg(mark_fg));
            let lines = wrapped_welcome_note_summary(note, card_inner_w);
            let styled = styled_welcome_note_lines(note, &lines, row_style, code_style);
            for (i, line) in styled.iter().enumerate() {
                buf.set_line(summary_x, row_y + i as u16, line, summary_w);
            }
            row_y += lines.len().max(1) as u16;
        }
        if plan.notes < notes.len() {
            buf.set_stringn(
                summary_x,
                row_y,
                format!("+{} more", notes.len() - plan.notes),
                usize::from(summary_w),
                Style::default().fg(t.muted()),
            );
        }
    }

    /// Start the guided tour (#377) in a fresh scratch project, never the
    /// user's workspace, and perform its first step.
    pub fn start_demo(&mut self) {
        // A second start would record the first tour's scratch project as
        // the workspace to return to, and the user's own would be lost.
        if self.tour.is_some() {
            self.status = String::from("The tour is already running (Esc leaves it)");
            return;
        }
        let tour = match crate::tour::Tour::parse(&crate::tour::tour_source()) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("The tour could not start: {e}");
                return;
            }
        };
        // A tour ended by closing croft's terminal left its scratch folder
        // behind (#863): clear those whose croft is gone before adding one.
        let demo_dir = croft_cache_dir().join("demo");
        crate::tour::sweep_dead_scratch(&demo_dir, process_is_alive);
        let scratch = match crate::tour::create_scratch(&demo_dir) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("The tour could not create its sample project: {e}");
                return;
            }
        };
        let previous_root = self.workspace_root().to_path_buf();
        self.change_workspace_root(scratch.clone());
        let first = tour.current().map(|s| s.action.clone());
        self.tour = Some(TourRun {
            tour,
            scratch,
            previous_root,
            step_opened_modal: false,
        });
        if let Some(action) = first {
            self.perform_tour_action(action);
        }
    }

    /// Perform the next tour step, or finish after the last one.
    pub fn advance_tour(&mut self) {
        let Some(run) = self.tour.as_mut() else {
            return;
        };
        match run.tour.advance().map(|s| s.action.clone()) {
            Some(action) => self.perform_tour_action(action),
            None => self.finish_tour(),
        }
    }

    /// Leave the tour: back to the workspace it started from, and the
    /// scratch project deleted.
    pub fn finish_tour(&mut self) {
        let Some(run) = self.tour.take() else {
            return;
        };
        // Finished or skipped, the welcome panel stops offering it.
        self.tour_done = true;
        let saved = crate::prefs::save_tour_done_in(&self.config_dir);
        // The sample's tabs go with it, edited or not: left open they name
        // deleted files, and saving one would write a scratch file back.
        let mut sample: Vec<PathBuf> = self
            .open_tab_paths()
            .into_iter()
            .filter(|p| p.starts_with(&run.scratch))
            .collect();
        sample.dedup();
        for path in sample {
            self.close_tabs_with_path_everywhere(&path);
        }
        self.change_workspace_root(run.previous_root);
        self.status = match crate::tour::remove_scratch(&run.scratch) {
            Ok(()) => String::from("Tour finished"),
            Err(e) => format!("Tour finished, but its sample project was not removed: {e}"),
        };
        self.report_unsaved_setting(saved);
    }

    pub(super) fn perform_tour_action(&mut self, action: crate::tour::TourAction) {
        use crate::tour::TourAction as A;
        let Some(run) = self.tour.as_mut() else {
            return;
        };
        run.step_opened_modal = matches!(
            action,
            A::QuickOpen | A::FindFile(_) | A::CommandPalette | A::Palette(_) | A::ThemePicker
        );
        let scratch = run.scratch.clone();
        match action {
            A::Open(rel) => {
                if let Err(e) = self.open_at(&scratch.join(rel), 0, 0) {
                    self.status = format!("Tour: {e}");
                }
            }
            A::QuickOpen => self.open_file_finder(),
            A::FindFile(query) => {
                self.open_file_finder();
                if let Some(finder) = self.file_finder.as_mut() {
                    finder.set_query(&query);
                }
            }
            A::SplitEditor => self.split_editor(),
            A::Terminal => {
                if !self.show_terminal {
                    self.toggle_terminal();
                }
                self.focus_pane(Pane::Terminal);
            }
            A::Run(cmd) => {
                if !self.show_terminal {
                    self.toggle_terminal();
                }
                if let Some(term) = self.terminals.get_mut(self.active_terminal) {
                    term.write_input(format!("{cmd}\r").as_bytes());
                }
            }
            A::JumpToError => {
                let lines = self
                    .terminals
                    .get(self.active_terminal)
                    .map(|t| t.visible_lines().0)
                    .unwrap_or_default();
                match crate::tour::first_error_ref(&lines) {
                    Some((file, line)) => {
                        let path = if std::path::Path::new(&file).is_absolute() {
                            PathBuf::from(file)
                        } else {
                            scratch.join(file)
                        };
                        let _ = self.open_at(&path, line.saturating_sub(1), 0);
                    }
                    // The command may still be running: open where the
                    // sample's mistake is, which is what it would find.
                    None => {
                        let _ = self.open_at(&scratch.join("app.py"), 3, 0);
                    }
                }
            }
            A::CommandPalette => self.open_command_palette(),
            A::Palette(query) => {
                self.open_command_palette();
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.set_query(&query);
                }
            }
            A::ThemePicker => self.open_theme_picker(),
            A::Done => {}
        }
    }

    /// The caption panel (#377): the step's caption, wrapped, over a row of
    /// its own for the progress and the keys, above the status bar.
    ///
    /// The keys have that row so no caption can push them off (#863): one
    /// clamped line of progress, caption, then keys cut the keys first, and
    /// at 80 columns every step lost them.
    pub(super) fn render_tour_caption(&mut self, frame: &mut ratatui::Frame) {
        let Some((rect, caption)) = self.tour_caption_panel(frame.area()) else {
            return;
        };
        let Some(run) = self.tour.as_ref() else {
            return;
        };
        let style = Style::default()
            .fg(self.theme.accent_contrast_fg())
            .bg(self.theme.accent())
            .add_modifier(Modifier::BOLD);
        let body = Rect {
            x: rect.x + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height - 1,
            ..rect
        };
        let keys = Rect {
            y: rect.bottom() - 1,
            height: 1,
            ..body
        };
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(ratatui::widgets::Block::new().style(style), rect);
        frame.render_widget(
            Paragraph::new(caption).wrap(ratatui::widgets::Wrap { trim: true }),
            body,
        );
        let progress = run.tour.progress();
        let gap = (keys.width as usize)
            .saturating_sub(progress.len() + Line::from(Self::TOUR_KEYS).width());
        // Too narrow for both, the keys win: they are what moves the tour.
        let row = if gap >= 2 {
            Line::from(format!("{progress}{}{}", " ".repeat(gap), Self::TOUR_KEYS))
        } else {
            Line::from(Self::TOUR_KEYS).right_aligned()
        };
        frame.render_widget(Paragraph::new(row), keys);
    }

    /// The caption panel's key hint, on the panel's last row.
    pub(super) const TOUR_KEYS: &str = "Enter next \u{b7} Esc leave";

    /// Where the tour's caption panel goes in `area`, with the caption it
    /// shows: centred above the status bar, as tall as the wrapped caption
    /// plus the keys row. A menu it would overlap moves it beside the menu
    /// (#863): the theme picker a step opens stands the frame's full height
    /// on the left, and covered the caption's start. `None` between steps,
    /// or in a frame too small to hold it.
    pub(super) fn tour_caption_panel(&self, area: Rect) -> Option<(Rect, String)> {
        let run = self.tour.as_ref()?;
        let step = run.tour.current()?;
        if area.width < 20 || area.height < 4 {
            return None;
        }
        let caption = step.caption_text();
        let status_h: u16 = if self.status_bar_visible { 1 } else { 0 };
        let bottom = area.bottom().saturating_sub(status_h);
        // As wide as the caption on one row, or as the keys row, within the
        // columns `left..right`; a column of padding on each side.
        let needed = Line::from(caption.as_str())
            .width()
            .max(run.tour.progress().len() + 2 + Line::from(Self::TOUR_KEYS).width())
            + 2;
        let panel = |left: u16, right: u16| {
            let room = right.saturating_sub(left);
            let width = u16::try_from(needed).unwrap_or(u16::MAX).min(room);
            let rows = Paragraph::new(caption.as_str())
                .wrap(ratatui::widgets::Wrap { trim: true })
                .line_count(width.saturating_sub(2)) as u16;
            let height = rows.saturating_add(1).min(bottom.saturating_sub(area.y));
            Rect {
                x: left + (room - width) / 2,
                y: bottom - height,
                width,
                height,
            }
        };
        let mut rect = panel(area.x + 1, area.right() - 1);
        if let Some(menu) = self.menu_rect()
            && menu.intersects(rect)
        {
            // The wider side of the menu, while it leaves a readable column;
            // on a frame too narrow for that, the panel covers the menu.
            let before = (area.x + 1, menu.x.saturating_sub(1));
            let after = (menu.right() + 1, area.right() - 1);
            let span = |(l, r): (u16, u16)| r.saturating_sub(l);
            let side = if span(after) >= span(before) {
                after
            } else {
                before
            };
            if span(side) >= 40 {
                rect = panel(side.0, side.1);
            }
        }
        Some((rect, caption))
    }

    /// Close every picker and palette a tour step may have opened.
    #[cfg(test)]
    pub fn close_all_modals_for_test(&mut self) {
        self.command_palette = None;
        self.file_finder = None;
        self.context_menu = None;
    }

    pub fn welcome_image_emit_payload(&self) -> Option<(&str, &WelcomeLayout)> {
        if self.shortcuts_modal.is_some()
            || self.file_finder.is_some()
            || self.command_palette.is_some()
            || self.go_to_symbol.is_some()
            || self.workspace_symbols.is_some()
            || self.process_picker.is_some()
            || self.zoxide_jump.is_some()
            || self.command_history_popup.is_some()
            || self.branch_picker.is_some()
            || self.input_prompt.is_some()
            || self.list_picker.is_some()
            || self.context_menu.is_some()
            || self.connect_dialog.is_some()
            || !self.editor.is_blank_initial()
            || self.terminal_maximized
        {
            return None;
        }
        self.overlays.welcome.emit_payload()
    }

    pub fn mark_welcome_image_emitted(&mut self) {
        self.overlays.welcome.mark_emitted();
    }

    /// What the welcome wordmark would be baked for now: its layout, the
    /// cell size and the theme. `None` while it is not laid out, or on a
    /// terminal whose cell size is unknown.
    pub(super) fn welcome_bake_key(&self) -> Option<WelcomeBakeKey> {
        Some(WelcomeBakeKey {
            layout: *self.overlays.welcome.layout()?,
            cell: self.cell_pixel?,
            bg: self.theme_bg_pixel(),
            light: self.theme.is_light(),
            protocol: self.inline_protocol,
        })
    }

    /// Bake the welcome wordmark for `key` on a thread of its own: decoding,
    /// scaling and re-encoding the logo is tens of milliseconds, more on a
    /// Retina canvas, and the frame laying it out must not wait (#1973).
    pub(super) fn start_welcome_bake(&mut self, key: WelcomeBakeKey) {
        let WelcomeLayout { cell_w, cell_h, .. } = key.layout;
        let (cw, ch) = key.cell;
        let is_tmux = crate::iterm2_inline::detect_tmux();
        // On a light background the asset's white "cr"/"ft" letters would
        // vanish; the ink recolor turns just those pixels dark while the
        // orange/teal strokes pass through.
        let ink = key.light.then_some((0x3b_u8, 0x3b_u8, 0x3b_u8));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // The logo's cells stay blank until it lands.
            crate::startup_gate::wait();
            let osc = crate::iterm2_inline::fit_image_with_ink(
                crate::iterm2_inline::WELCOME_LOGO_PNG,
                cell_w as u32 * cw,
                cell_h as u32 * ch,
                key.bg,
                ink,
            )
            .ok()
            .and_then(|baked| {
                crate::iterm2_inline::build_inline_image(
                    key.protocol,
                    &baked,
                    cell_w,
                    cell_h,
                    false,
                    crate::iterm2_inline::KITTY_ID_WELCOME,
                )
            })
            .map(|raw| crate::iterm2_inline::maybe_tmux_wrap(key.protocol, is_tmux, raw));
            let _ = tx.send(osc);
        });
        self.welcome_bake = Some((key, rx));
    }

    /// Put up the welcome wordmark once the bake `render_welcome` started
    /// lands (#1973), or bake again when the screen has moved on from what it
    /// was baked for. `true` when the screen changes: the picture is due, or
    /// the bake failed and the text label takes its place.
    pub(super) fn poll_welcome_bake(&mut self) -> bool {
        let Some((key, rx)) = self.welcome_bake.as_ref() else {
            return false;
        };
        let osc = match rx.try_recv() {
            Ok(osc) => osc,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let key = *key;
        self.welcome_bake = None;
        if let Some(now) = self.welcome_bake_key()
            && now != key
        {
            self.start_welcome_bake(now);
            return false;
        }
        if let Some(osc) = osc {
            self.overlays.welcome.set_image(osc);
        }
        self.overlays.welcome.mark_dirty();
        true
    }
}
