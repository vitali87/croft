//! Drawing each frame (`render`), the status-bar text, caret visibility and the
//! screen-reader descriptions.

use super::*;

/// The amber a status-bar item takes when it wants attention: the
/// uncommitted-changes dot beside the branch, and the operation git stopped
/// in (`main | MERGING`).
pub(super) const GIT_DIRTY_COLOR: Color = Color::Rgb(0xeb, 0xcb, 0x8b);

/// The dot after the branch name while the working tree has uncommitted
/// changes (VS Code's `main*`): the branch itself keeps its quiet colour, so
/// a dirty tree reads as one small mark rather than a recoloured segment.
pub(super) const GIT_DIRTY_DOT: &str = "\u{2022}";

/// Powerline half circles, the rounded ends of a status-bar pill (as on the
/// update pill, the PROBLEMS badge and the Extensions switches).
pub(super) const PILL_CAP_L: &str = "\u{e0b6}";

pub(super) const PILL_CAP_R: &str = "\u{e0b4}";

/// The terminal cells `s` paints in, as ratatui lays it out. A wide
/// character (`项`, an emoji) takes two, so a click target measured in
/// `char`s would sit left of the text it belongs to.
pub(super) fn status_cells(s: &str) -> u16 {
    unicode_width::UnicodeWidthStr::width(s) as u16
}

/// Cells left blank after the status bar's last right-hand segment: none,
/// since the language pill's rounded end already reads as an edge.
pub(super) const STATUS_RIGHT_MARGIN: u16 = 0;

impl StatusTones {
    pub(super) fn new(theme: crate::theme::Theme, bar: Color) -> Self {
        // The theme's own foreground is already chosen for its background,
        // light or dark; `theme.ui` would adapt it a second time (on a light
        // theme, flipping dark text to near-white).
        let (r, g, b) = theme.syntax().fg;
        let text = Color::Rgb(r, g, b);
        let muted = match theme.surfaces() {
            Some(s) => s.muted(),
            None => theme.ui(Color::Rgb(0x9a, 0xa4, 0xb8)),
        };
        // A high contrast theme resolves chrome greys to full strength on
        // purpose (#1548); blending toward the bar would undo that.
        let dim = if theme.high_contrast() {
            muted
        } else {
            mix_color(muted, bar, 0.6)
        };
        StatusTones { text, muted, dim }
    }
}

/// `a` blended toward `b` by `t` (0.0 keeps `a`); a non-RGB colour is kept.
pub(super) fn mix_color(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(m(ar, br), m(ag, bg), m(ab, bb))
        }
        _ => a,
    }
}

/// The status bar's background: Croft Loch's chrome surface, a near-black
/// seam on the gradient (OLED) themes, the navy band elsewhere.
pub(super) fn status_bar_bg(theme: crate::theme::Theme) -> Color {
    if let Some(s) = theme.surfaces() {
        s.chrome()
    } else if theme.gradient() {
        theme.ui(Color::Rgb(0x18, 0x18, 0x18))
    } else {
        theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e))
    }
}

/// Build the status-bar spans for the git segment: the branch glyph in the
/// theme accent, the branch name in the bar's text tone, an amber dot while
/// the tree is dirty, and quiet ahead/behind counts. Returns an empty vec
/// outside a git repo so the bar shows nothing.
pub(super) fn git_status_spans<'a>(
    status: &'a crate::git::GitStatus,
    theme: crate::theme::Theme,
) -> Vec<Span<'a>> {
    if !status.in_repo {
        return Vec::new();
    }
    let tones = StatusTones::new(theme, status_bar_bg(theme));
    let mut spans: Vec<Span> = Vec::with_capacity(7);
    spans.push(Span::raw("  "));
    // Codicon `cod-source-control` (U+EA68): the Y-fork that matches the
    // activity-bar Source Control icon, so the status-bar branch indicator
    // and the SCM panel share the same visual mark.
    spans.push(Span::styled(
        "\u{ea68} ",
        Style::default().fg(theme.accent()),
    ));
    spans.push(Span::styled(
        status.head_label(),
        Style::default().fg(tones.text),
    ));
    // The operation git stopped in (#1356), as `main | MERGING`, read as one
    // phrase: the dirty dot comes after it.
    if let Some(op) = status.operation {
        spans.push(Span::styled(
            format!(" | {}", op.label()),
            Style::default()
                .fg(theme.ui(GIT_DIRTY_COLOR))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if status.dirty {
        spans.push(Span::styled(
            format!(" {GIT_DIRTY_DOT}"),
            Style::default()
                .fg(theme.ui(GIT_DIRTY_COLOR))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if status.ahead > 0 {
        spans.push(Span::styled(
            format!(" \u{2191}{}", status.ahead),
            Style::default().fg(tones.muted),
        ));
    }
    if status.behind > 0 {
        spans.push(Span::styled(
            format!(" \u{2193}{}", status.behind),
            Style::default().fg(tones.muted),
        ));
    }
    spans
}

/// The longest workspace name the status bar's cap shows before it elides.
pub(super) const WORKSPACE_CAP_MAX: usize = 24;

/// Spans for the cap at the left edge of the status bar: the logo's `<>` in
/// the brand orange and the workspace folder's name, on the theme's
/// selection tint with a rounded right end. It anchors the bar the way VS
/// Code's remote indicator does, and says which workspace this window is.
pub(super) fn workspace_cap_spans(theme: crate::theme::Theme, name: &str) -> Vec<Span<'static>> {
    let fill = theme.selection();
    let tones = StatusTones::new(theme, status_bar_bg(theme));
    // Measured in terminal cells, not chars: a wide name (`项目`) paints
    // twice its char count, and every offset after the cap counts cells.
    let name: String = if status_cells(name) as usize > WORKSPACE_CAP_MAX {
        use unicode_width::UnicodeWidthChar;
        let mut s = String::new();
        let mut used = 0;
        for c in name.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > WORKSPACE_CAP_MAX - 1 {
                break;
            }
            used += w;
            s.push(c);
        }
        s.push('\u{2026}');
        s
    } else {
        name.to_string()
    };
    vec![
        Span::styled(" ", Style::default().bg(fill)),
        Span::styled(
            "<>",
            Style::default()
                .fg(BRAND_ORANGE)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {name} "),
            Style::default()
                .fg(tones.text)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(PILL_CAP_R, Style::default().fg(fill)),
    ]
}

/// A status-bar badge (a recording, vim's mode, an attention flag) as a
/// rounded pill: `label` padded by a cell each side, between half-circle
/// caps in `fill`.
pub(super) fn status_pill(label: &str, fill: Color, fg: Color) -> [Span<'static>; 3] {
    [
        Span::styled(PILL_CAP_L, Style::default().fg(fill)),
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(fg)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(PILL_CAP_R, Style::default().fg(fill)),
    ]
}

/// The Overwrite badge that follows the workspace cap in the status bar: the
/// continuous-underline symbol (⎁) in the brand orange, so a switched mode is
/// hard to miss. Insert, the default, shows nothing, as in VS Code, and keeps
/// the bar's cells for the segments after it. It sits on the left, away from
/// the clickable position, encoding and language segments on the right.
pub(super) fn overwrite_mode_span() -> Span<'static> {
    Span::styled(
        "\u{2381}",
        Style::default()
            .fg(BRAND_ORANGE)
            .add_modifier(Modifier::BOLD),
    )
}

/// Rotating-arc frames for the in-progress update glyph. Cycled to read as a
/// spinning circular arrow, the universal "working" indicator.
pub(super) const UPDATE_SPINNER_FRAMES: [&str; 6] = [
    "\u{25DC}", "\u{25E0}", "\u{25DD}", "\u{25DE}", "\u{25E1}", "\u{25DF}",
];

/// Milliseconds each spinner frame is held; ~10 frames/second reads as smooth
/// motion while keeping the status-bar-only redraws light over SSH.
pub(super) const UPDATE_SPINNER_FRAME_MS: u128 = 100;

/// Pick the spinner frame for a given elapsed time. Pure so the wrap-around
/// is unit-testable without a clock.
pub(super) fn update_spinner_frame(elapsed_ms: u128) -> &'static str {
    let n = UPDATE_SPINNER_FRAMES.len() as u128;
    UPDATE_SPINNER_FRAMES[((elapsed_ms / UPDATE_SPINNER_FRAME_MS) % n) as usize]
}

impl App {
    /// Mirrors the predicate used in `App::render` to decide whether to call
    /// `frame.set_cursor_position`. Used by the post-draw overlay writer so
    /// it can re-Show the caret after the OSC-1337 image emit only when the
    /// editor would have shown it this frame.
    pub(super) fn cursor_should_be_visible(&self) -> bool {
        if self.context_menu.is_some() || self.prompt.is_some() || !self.cursor_visible_phase() {
            return false;
        }
        // On a `surfaces` theme the quick input's query has the terminal's
        // caret (see `quick_input_owns_caret`).
        if self.quick_input_owns_caret() {
            return self.quick_input_caret().is_some();
        }
        // A list picker (View Bookmarks, Quick Fix, ...) is the thing being
        // typed into; the editor's caret behind it would only point at text it
        // is not about to edit.
        if self.list_picker.is_some() {
            return false;
        }
        let editor_caret = if self.editor.diff.is_some() {
            self.editor.diff_caret_screen_pos()
        } else {
            self.editor.cursor_screen_pos()
        };
        if self.focus == Pane::Editor && editor_caret.is_some() {
            return true;
        }
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::SourceControl
            && self.source_control.cursor_screen_pos().is_some()
        {
            return true;
        }
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::Search
            && self.search.cursor_screen_pos().is_some()
        {
            return true;
        }
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::Extensions
            && self.extensions.cursor_screen_pos().is_some()
        {
            return true;
        }
        false
    }

    /// Whether a quick input (the Command Palette, Go to File, a list
    /// picker or an input prompt) has the terminal's caret: on a `surfaces`
    /// theme, which draws no caret block in the query, the caret goes there
    /// and no pane's caret blinks through the card. A boxed theme paints its
    /// own caret and keeps its caret rules as they were.
    pub(super) fn quick_input_owns_caret(&self) -> bool {
        self.theme.surfaces().is_some()
            && (self.command_palette.is_some()
                || self.file_finder.is_some()
                || self.list_picker.is_some()
                || self.input_prompt.is_some())
    }

    /// Where the terminal's caret goes while a quick input is open: the
    /// query caret of the topmost one (drawn last: list picker, input
    /// prompt, palette, finder), as its last render placed it. `None` on a
    /// boxed theme, whose quick inputs paint their own caret block.
    pub(super) fn quick_input_caret(&self) -> Option<(u16, u16)> {
        if let Some(p) = &self.list_picker {
            return p.last_caret;
        }
        if let Some(p) = &self.input_prompt {
            return p.last_caret;
        }
        if let Some(p) = &self.command_palette {
            return p.last_caret;
        }
        self.file_finder.as_ref().and_then(|f| f.last_caret)
    }

    /// True iff the caret is currently in its visible half of the blink
    /// cycle. VS Code uses a 530ms half-period; we match. We blink in
    /// software (by toggling whether `frame.set_cursor_position` is called
    /// each frame) instead of relying on the host terminal's own blink
    /// support, which iTerm2 / Terminal.app may have disabled in user
    /// preferences.
    pub(super) fn cursor_visible_phase(&self) -> bool {
        // A screen reader follows the cursor; one that vanishes half the
        // time is lost half the time.
        self.screen_reader || self.cursor_blink.visible_phase()
    }

    /// Croft Loch's seams: a hairline on each boundary between two surfaces,
    /// drawn over the frame cells the panes left as padding. The layout
    /// knows where its boundaries are; the panes do not.
    pub(super) fn paint_surface_seams(
        &self,
        frame: &mut ratatui::Frame,
        s: crate::theme::Surfaces,
        activity_bar: Option<Rect>,
        side_bars: [Option<Rect>; 2],
        panel: Option<Rect>,
        status_bar: Option<Rect>,
    ) {
        use crate::widgets::surface::{Edge, paint_seam};
        let full = frame.area();
        let hair = s.hairline();
        let buf = frame.buffer_mut();
        let column = |x: u16, r: Rect| Rect {
            x,
            y: r.y,
            width: 1,
            height: r.height,
        };
        let row = |y: u16, r: Rect| Rect {
            x: r.x,
            y,
            width: r.width,
            height: 1,
        };
        // A side bar against the activity bar on one side and the editor
        // column on the other; nothing at the frame's own edge.
        for bar in side_bars.into_iter().flatten().filter(|r| !r.is_empty()) {
            if bar.x > full.x {
                paint_seam(buf, column(bar.x, bar), Edge::Left, hair);
            }
            if bar.right() < full.right() {
                paint_seam(buf, column(bar.right() - 1, bar), Edge::Right, hair);
            }
        }
        // Editor groups side by side or stacked: the seam the user drags.
        for seam in &self.editor_seams {
            let span = seam.cross_end.saturating_sub(seam.cross_start);
            let rect = match seam.dir {
                editor_layout::SplitDir::Horizontal => Rect {
                    x: seam.pos,
                    y: seam.cross_start,
                    width: 1,
                    height: span,
                },
                editor_layout::SplitDir::Vertical => Rect {
                    x: seam.cross_start,
                    y: seam.pos,
                    width: span,
                    height: 1,
                },
            };
            let edge = match seam.dir {
                editor_layout::SplitDir::Horizontal => Edge::Left,
                editor_layout::SplitDir::Vertical => Edge::Top,
            };
            paint_seam(buf, rect, edge, hair);
        }
        if let Some(p) = panel.filter(|p| !p.is_empty()) {
            // The panel against the editor above it.
            if p.y > full.y {
                paint_seam(buf, row(p.y - 1, p), Edge::Bottom, hair);
            }
            // Split terminal panes against each other and the terminal rail.
            for t in &self.terminals {
                let r = t.last_area;
                if !r.is_empty() && r.right() < p.right() {
                    paint_seam(buf, column(r.right() - 1, r), Edge::Right, hair);
                }
            }
        }
        // Everything above the status bar against it, except the activity
        // bar, which is chrome like the status bar and whose cells an icon
        // image may own.
        if let Some(bar) = status_bar.filter(|b| b.y > full.y) {
            let y = bar.y - 1;
            match activity_bar.filter(|a| !a.is_empty()) {
                Some(a) => {
                    let left = Rect::new(full.x, y, a.x - full.x, 1);
                    let right = Rect::new(a.right(), y, full.right() - a.right(), 1);
                    paint_seam(buf, left, Edge::Bottom, hair);
                    paint_seam(buf, right, Edge::Bottom, hair);
                }
                None => paint_seam(buf, row(y, full), Edge::Bottom, hair),
            }
        }
    }

    /// Render the frame the terminal is sent: [`Self::render`], with every
    /// colour fitted to the terminal's depth (#1433).
    pub(super) fn render_for_terminal(&mut self, frame: &mut ratatui::Frame) {
        self.render(frame);
        self.color_depth.fit_buffer(frame.buffer_mut());
    }

    pub(super) fn render(&mut self, frame: &mut ratatui::Frame) {
        // Set again by the welcome panel when it paints the button.
        self.welcome_tour_button = Rect::default();
        let size = frame.area();
        self.last_frame_area = size;
        if let Some(menu) = self.context_menu.as_mut() {
            menu.localize();
        }
        self.update_announcer();
        if self.notes.generation != self.notes_shown_gen {
            self.notes_shown_gen = self.notes.generation;
            let mut counts: std::collections::HashMap<PathBuf, usize> =
                std::collections::HashMap::new();
            for n in self.notes.live().filter(|n| !n.resolved) {
                *counts.entry(self.tree.root.join(&n.file)).or_default() += 1;
            }
            self.tree.note_counts = std::sync::Arc::new(counts);
        }
        // Redaction reveal is app state; each pane paints from its own copy.
        let reveal = self.redactions_revealed();
        for t in self.terminals.iter_mut() {
            t.reveal_redactions = reveal;
        }
        // Hit rects must describe the frame being painted, not the last
        // frame that happened to include a section. A section that stops
        // rendering (Explorer's OPEN EDITORS / TIMELINE / DEPENDENCIES /
        // OUTLINE after a switch to Source Control) otherwise keeps its
        // stale rect, and `handle_mouse`'s containment checks swallow every
        // click the current view paints in those cells — the COMMITS graph
        // sits exactly there and its clicks went dead (#103). Zero them all
        // up front; each section's render re-records the live ones this
        // frame (the OSK does the same for its collapsed band).
        for rect in [
            &mut self.open_editors.last_area,
            &mut self.open_editors.last_scrollbar,
            &mut self.timeline.last_area,
            &mut self.timeline.last_scrollbar,
            &mut self.dependencies.last_area,
            &mut self.dependencies.last_scrollbar,
            &mut self.outline.last_area,
            &mut self.outline.last_scrollbar,
            &mut self.commit_graph.last_area,
            &mut self.commit_graph.last_scrollbar,
        ] {
            *rect = Rect::default();
        }
        self.dress_host_accents();
        self.expire_ssh_offer();
        self.sync_lane_root_badges();
        // Theme background, whole frame. croft's chrome (the sidebar panels,
        // explorer sections, activity bar, gaps) mostly paints `Color::Reset`
        // and leans on the iTerm2 `SetColors` session bg to color it. Ghostty /
        // Kitty / sixel ignore `SetColors`, so those Reset cells fall through to
        // the host terminal's own background (black), leaving only the surfaces
        // that fill an explicit bg (editor, terminal) themed. Prefilling the
        // frame with the theme bg colors every unfilled cell; widgets that set
        // their own bg override it (ratatui patches bg only when `Some`). Gated
        // to exactly the hosts where `SetColors` is NOT emitted (see
        // `apply_theme`): on iTerm2 the session bg already handles this and a
        // full-frame fill would force the OSC-1337 activity images to re-emit
        // every frame.
        if !crate::iterm2_inline::detect_iterm2_inline_support() {
            frame.render_widget(
                ratatui::widgets::Block::default()
                    .style(Style::default().bg(self.theme.editor_bg())),
                size,
            );
        }
        // The on-screen keyboard docks as a band between the panes and the
        // status bar, scaling its keys to thumb size on tall frames; frames
        // too short to host it keep it collapsed so the workspace can't be
        // squeezed into nothing.
        let osk_h = if self.osk.is_some() && size.height >= OSK_MIN_FRAME_HEIGHT {
            crate::widgets::osk::band_height(size.height)
        } else {
            0
        };
        // The bottom status bar can be hidden (Customize Layout / Zen Mode),
        // reclaiming its row for the workspace.
        let status_h: u16 = if self.status_bar_visible { 1 } else { 0 };
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(osk_h),
                Constraint::Length(status_h),
            ])
            .split(size);
        let main_area = outer[0];

        // Reserve the activity bar (when shown) and the secondary side bar,
        // then clamp the primary side bar so the editor keeps RIGHT_PANE_MIN.
        let activity_w = if self.activity_bar_visible {
            ACTIVITY_BAR_WIDTH
        } else {
            0
        };
        let secondary_w = if self.secondary_side_bar_visible {
            SECONDARY_SIDEBAR_WIDTH
        } else {
            0
        };
        let max_sidebar = main_area
            .width
            .saturating_sub(activity_w)
            .saturating_sub(secondary_w)
            .saturating_sub(RIGHT_PANE_MIN);
        let sidebar_w = self
            .sidebar_width
            .clamp(SIDEBAR_WIDTH_MIN, max_sidebar.max(SIDEBAR_WIDTH_MIN));
        // Persist the clamped value so subsequent drags start from where
        // the user can actually see the splitter.
        self.sidebar_width = sidebar_w;

        // Panel Alignment = Justify spans the panel across the full content
        // width (under the side bar): carve it off the bottom of `main` BEFORE
        // the horizontal chrome split, so the side bars sit above it.
        let justify_panel = self.show_terminal
            && !self.terminal_maximized
            && osk_h == 0
            && self.panel_alignment == PanelAlignment::Justify;
        let (chrome_area, justify_terminal) = if justify_panel {
            let total_h = main_area.height;
            let max_h = total_h
                .saturating_sub(EDITOR_HEIGHT_MIN)
                .max(TERMINAL_HEIGHT_MIN);
            let term_h = self
                .terminal_height
                .unwrap_or(total_h / 3)
                .clamp(TERMINAL_HEIGHT_MIN, max_h);
            self.terminal_height = Some(term_h);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(EDITOR_HEIGHT_MIN),
                    Constraint::Length(term_h),
                ])
                .split(main_area);
            (rows[0], Some(rows[1]))
        } else {
            (main_area, None)
        };

        // Pure horizontal split of the workspace chrome (honors activity-bar
        // visibility, side bar position, and the secondary side bar).
        let chrome = compute_chrome_layout(
            chrome_area,
            self.activity_bar_visible,
            self.show_tree,
            self.secondary_side_bar_visible,
            self.side_bar_position,
            sidebar_w,
            SECONDARY_SIDEBAR_WIDTH,
        );
        let activity_area = chrome.activity;
        let side_area = chrome.primary_side;
        let secondary_area = chrome.secondary_side;
        let right_area = chrome.right;
        // Remember the Explorer column so the zoxide jump popup can anchor
        // over it instead of centering on the whole frame.
        self.last_sidebar_area = side_area;
        self.sidebar_splitter_x = chrome.sidebar_splitter_x;
        self.last_content_width = right_area.width;
        self.last_content_height = right_area.height;

        // Editor ↔ terminal split within the editor column. `terminal_band` is
        // the full bottom strip; `terminal_area` is where the terminals draw
        // (narrower than the band under Left/Right alignment).
        let (editor_area, terminal_area, terminal_band) = if let Some(jt) = justify_terminal {
            // The panel was already carved full-width from `main`; the editor
            // fills the rest of its column and the splitter sits at the seam.
            self.terminal_splitter_y = Some(jt.y);
            (right_area, Some(jt), Some(jt))
        } else if osk_h > 0 && self.focus == Pane::Terminal && self.show_terminal {
            let zero_editor = Rect {
                x: right_area.x,
                y: right_area.y,
                width: right_area.width,
                height: 0,
            };
            self.terminal_splitter_y = None;
            (zero_editor, Some(right_area), Some(right_area))
        } else if osk_h > 0 {
            self.terminal_splitter_y = None;
            (right_area, None, None)
        } else if self.show_terminal {
            if self.terminal_maximized {
                // Editor / welcome collapses; terminal fills the entire
                // right column next to the Explorer. Splitter is hidden
                // because there is nothing to drag against.
                let zero_editor = Rect {
                    x: right_area.x,
                    y: right_area.y,
                    width: right_area.width,
                    height: 0,
                };
                self.terminal_splitter_y = None;
                (zero_editor, Some(right_area), Some(right_area))
            } else {
                let total_h = right_area.height;
                let pinned = self.terminal_height.map(|h| {
                    h.clamp(
                        TERMINAL_HEIGHT_MIN,
                        total_h
                            .saturating_sub(EDITOR_HEIGHT_MIN)
                            .max(TERMINAL_HEIGHT_MIN),
                    )
                });
                let rows = if let Some(term_h) = pinned {
                    self.terminal_height = Some(term_h);
                    Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(EDITOR_HEIGHT_MIN),
                            Constraint::Length(term_h),
                        ])
                        .split(right_area)
                } else {
                    Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                        .split(right_area)
                };
                self.terminal_splitter_y = Some(rows[1].y);
                let band = rows[1];
                // Left/Right alignment hugs the panel to that half of the band.
                let term = panel_band_rect(band, self.panel_alignment);
                (rows[0], Some(term), Some(band))
            }
        } else {
            self.terminal_splitter_y = None;
            (right_area, None, None)
        };
        // Centered Layout (#1585): the editor keeps its width in the middle
        // of the column and the margins stay plain background.
        let editor_area = if self.centered_layout_active() {
            centered_editor_rect(editor_area, self.centered_layout_width)
        } else {
            editor_area
        };

        if let Some(activity_area) = activity_area {
            self.render_activity_bar(frame, activity_area);
        } else {
            // Hidden activity bar: zero the icon hit-rects so clicks and the
            // post-draw OSC image flush both treat it as absent (no ghosting).
            self.sidebar_areas = SidebarAreas::default();
        }

        // Secondary side bar: the active file's Outline, on the edge opposite
        // the primary side bar.
        self.render_secondary_side_bar(frame, secondary_area);

        if let Some(area) = side_area {
            // Croft Loch's side bar is one chrome surface: the views and the
            // Explorer's stacked sections paint on it.
            if let Some(s) = self.theme.surfaces() {
                frame.render_widget(
                    ratatui::widgets::Block::default().style(Style::default().bg(s.chrome())),
                    area,
                );
            }
            let usable_area = area;
            // Feed the render-time pointer cell to every side panel so the row
            // under the cursor (and the remote header pills) can light up,
            // mirroring the editor tab strip and terminal pane buttons.
            let panel_pointer = self.pointer_cell;
            self.tree.hover_pointer = panel_pointer;
            self.search.hover_pointer = panel_pointer;
            self.source_control.hover_pointer = panel_pointer;
            self.run_debug.hover_pointer = panel_pointer;
            self.remote.hover_pointer = panel_pointer;
            self.outline.hover_pointer = panel_pointer;
            self.open_editors.hover_pointer = panel_pointer;
            self.timeline.hover_pointer = panel_pointer;
            self.agent_lane_panel.hover_pointer = panel_pointer;
            self.dependencies.hover_pointer = panel_pointer;
            self.extensions.hover_pointer = panel_pointer;
            self.commit_graph.hover_pointer = panel_pointer;
            // Explorer stacks its toggled sub-views (Open Editors / Folders /
            // Outline / Timeline / Dependencies) inside the usable strip.
            // Every other sidebar view fills it with its single activity widget.
            match self.sidebar_view {
                SidebarView::Explorer => self.render_explorer_sections(frame, usable_area),
                SidebarView::Search => frame.render_widget(&mut self.search, usable_area),
                SidebarView::SourceControl => {
                    // The COMMITS graph section docks under the change list,
                    // capped at half the strip (like the Explorer sub-views).
                    // Hidden entirely while the repo empty-state hero shows.
                    let graph_h = if self.source_control.status.in_repo {
                        self.commit_graph
                            .desired_height(usable_area.height)
                            .min(usable_area.height.saturating_sub(6))
                    } else {
                        0
                    };
                    let scm_area = Rect {
                        height: usable_area.height - graph_h,
                        ..usable_area
                    };
                    self.sync_nested_repos();
                    frame.render_widget(&mut self.source_control, scm_area);
                    if graph_h > 0 {
                        let graph_area = Rect {
                            y: usable_area.y + usable_area.height - graph_h,
                            height: graph_h,
                            ..usable_area
                        };
                        frame.render_widget(&mut self.commit_graph, graph_area);
                    } else {
                        self.commit_graph.last_area = Rect::default();
                    }
                }
                SidebarView::Remote => frame.render_widget(&mut self.remote, usable_area),
                SidebarView::RunDebug => {
                    // Breakpoints change from many places (F9, the gutter,
                    // the condition and logpoint editors); syncing here keeps
                    // the list current without a refresh at each of them.
                    self.sync_breakpoint_list();
                    frame.render_widget(&mut self.run_debug, usable_area)
                }
                SidebarView::Extensions => frame.render_widget(&mut self.extensions, usable_area),
                SidebarView::Testing => frame.render_widget(&mut self.testing, usable_area),
                SidebarView::CodeQL => {
                    self.codeql.focused = self.focus == Pane::Tree;
                    let running = self.codeql_run.as_ref().map(|(_, since)| {
                        (self.codeql_run_name.clone(), since.elapsed().as_secs())
                    });
                    self.codeql.set_running(running);
                    frame.render_widget(&mut self.codeql, usable_area)
                }
            }
        }
        if !self.editor_layout.is_split() && self.editor.is_blank_initial() {
            self.render_welcome(frame, editor_area);
            // The previous frame may have rendered an image-preview tab
            // whose OSC-1337 pixels are still cached in iTerm's image
            // store. Closing that tab brings us here, but ratatui's diff
            // alone doesn't tell iTerm to drop the image — flag it so the
            // main loop wipes the screen on the next pass. Both slots, in
            // case a split was just collapsed back to the welcome screen.
            self.disable_editor_image(0);
            self.disable_editor_image(1);
            // The welcome screen has no minimap (no document to preview).
            self.disable_minimap_image();
            self.editor_seams.clear();
            // Keep the editor's hit-test rectangles fresh so the activity-bar
            // / tree click logic still works even though we skipped the
            // EditorTabs widget this frame.
            self.editor.last_full_area = editor_area;
            self.editor.last_area = Rect {
                x: editor_area.x,
                y: editor_area.y,
                width: editor_area.width,
                height: 0,
            };
        } else {
            // The editor just overdrew whatever cells the welcome image
            // occupied; if the user reopens the welcome screen we'll need
            // to re-emit it.
            self.overlays.welcome.mark_dirty();
            // Feed the render-time pointer cell to every editor group so the
            // tab strip can light the hovered tab body / close-cross pill,
            // mirroring the terminal pane buttons' hover affordance.
            let pointer = self.pointer_cell;
            self.editor.hover_pointer = pointer;
            // A modal quick input takes the keyboard without moving focus
            // (no focus sync runs when one opens), so the editor learns each
            // frame to drop its accent marks while one is up.
            self.editor.overlay_has_keyboard = self.modal_overlay_open();
            // Without an inline-image protocol an image tab draws itself in
            // half blocks (#1591).
            let cell_images = !self.inline_images_enabled();
            self.editor.cell_images = cell_images;
            // Every group gets the pointer, not only the focused one, so a
            // preview beside the source lifts its links too (#1939).
            for group in self.editor_layout.inactive_groups_mut() {
                group.cell_images = cell_images;
                group.hover_pointer = pointer;
            }
            // Breadcrumb bar: recompute the path + symbol trail for the active
            // file each frame so it tracks caret moves and outline updates.
            self.editor.breadcrumbs = self.build_breadcrumbs();
            // Sticky scroll: recompute the pinned scope headers from the top
            // visible line each frame so they track scrolling.
            self.editor.sticky_lines = self.build_sticky_lines();
            // Multiplayer ghost carets: other participants' parked carets in
            // the active file, in their colors. The current typist's caret is
            // the live cursor, so it never ghosts; departed participants drop
            // out with the roster.
            self.editor.ghost_carets = match self.editor.path.clone() {
                Some(path) if !self.session_carets.is_empty() => self
                    .session_carets
                    .iter()
                    .filter(|(id, parked)| {
                        Some(**id) != self.session_typist
                            && parked.path == path
                            && self.session_participants.iter().any(|p| p.id == **id)
                    })
                    .map(|(id, parked)| (parked.row, parked.col, participant_color(*id)))
                    .collect(),
                _ => Vec::new(),
            };
            // Collab peers' carets (Phase D independent viewports): same
            // ghost rendering, keyed by workspace-relative file, plus a name
            // tag while the caret is inside its fade window. The seated
            // navigator's caret wears its identity accent.
            self.editor.ghost_caret_labels.clear();
            if !self.collab_carets.is_empty()
                && let Some(active) = self
                    .editor
                    .path
                    .as_ref()
                    .and_then(|p| collab_file_key(&self.tree.root, p))
            {
                let navigator_sites = self
                    .pair_host
                    .as_ref()
                    .map(|h| h.caret_sites())
                    .unwrap_or_default();
                let now = std::time::Instant::now();
                for (site, c) in &self.collab_carets {
                    if c.file != active {
                        continue;
                    }
                    let color = collab_caret_color(&navigator_sites, *site);
                    self.editor.ghost_carets.push((c.row, c.col, color));
                    if !c.name.is_empty() && now.duration_since(c.last_moved) < CARET_LABEL_FADE {
                        self.editor
                            .ghost_caret_labels
                            .push((c.row, c.col, c.name.clone(), color));
                    }
                }
            }
            // Gutter stop button for an AI stream into the active file; the
            // hoisted group is the only one that shows it (clear the rest so
            // a group swap never leaves a stale button behind).
            self.editor.stream_stop_line = self.stream_stop_row();
            // The navigator's comment boxes for the active file, same
            // hoisting rule (rows come from poll_pair's per-tick snapshot).
            let author = self
                .pair_host
                .as_ref()
                .map(|h| h.name().to_string())
                .unwrap_or_default();
            self.editor.comment_boxes = self
                .editor
                .path
                .as_ref()
                .and_then(|p| collab_file_key(&self.tree.root, p))
                .and_then(|file| self.navigator_notes.get(&file))
                .map(|notes| {
                    notes
                        .iter()
                        .map(|(id, row, body)| crate::widgets::editor::CommentBox {
                            id: *id,
                            line: *row,
                            author: author.clone(),
                            body: body.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Review threads (#366) join the navigator's rather than
            // replacing them: both are comments on the same lines, and a
            // reviewer reading a PR with a navigator session open wants
            // both. Appended after, so a navigator note and a review
            // comment on one line keep a stable order.
            if let Some((path, threads)) = &self.review_boxes
                && self.editor.path.as_deref() == Some(path.as_path())
            {
                let lines = self.editor.lines.len();
                self.editor.comment_boxes.extend(threads.iter().map(|t| {
                    crate::widgets::editor::CommentBox {
                        id: t.id,
                        line: t.box_line(lines),
                        author: t.title_for(lines),
                        body: t.body.clone(),
                    }
                }));
            }
            // Sticky notes (#367) on this file, at their re-found lines.
            self.note_box_ids.clear();
            if let Some(file) = self.note_file_key() {
                let lines = &self.editor.lines;
                let boxes: Vec<(u64, String, crate::widgets::editor::CommentBox)> = self
                    .notes
                    .on_file(&file)
                    .map(|n| {
                        let id = Self::note_box_id(&n.id);
                        (
                            id,
                            n.id.clone(),
                            crate::widgets::editor::CommentBox {
                                id,
                                line: n.place(lines),
                                author: n.title(),
                                body: n.text(),
                            },
                        )
                    })
                    .collect();
                for (id, note_id, b) in boxes {
                    self.note_box_ids.insert(id, note_id);
                    self.editor.comment_boxes.push(b);
                }
            }
            // Pending review comments (#366) on this file, until submitted.
            if !self.review_pending.is_empty()
                && let Some((root, _)) = &self.review_pr
                && let Some(rel) = self.editor.path.as_deref().and_then(|p| {
                    let top = self.git_worker_for_root(root).status().repo_root.clone()?;
                    p.strip_prefix(&top).ok().map(|r| {
                        r.components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                })
            {
                let lines = self.editor.lines.len();
                for (i, c) in self.review_pending.iter().enumerate() {
                    if c.path == rel {
                        self.editor
                            .comment_boxes
                            .push(crate::widgets::editor::CommentBox {
                                id: Self::PENDING_REVIEW_ID + i as u64,
                                line: c.line.min(lines.saturating_sub(1)),
                                author: String::from("you \u{b7} pending"),
                                body: c.body.clone(),
                            });
                    }
                }
            }
            // A focused box that vanished (ignored elsewhere, cleared, or
            // re-anchored away) releases the keyboard back to the buffer.
            if let Some(focus) = &self.editor.comment_focus
                && !self.editor.comment_boxes.iter().any(|b| b.id == focus.id)
            {
                self.editor.comment_focus = None;
            }
            for group in self.editor_layout.inactive_groups_mut() {
                for ed in &mut group.editors {
                    ed.stream_stop_line = None;
                    ed.comment_boxes.clear();
                    ed.comment_focus = None;
                }
            }
            // Render the editor group layout tree. The active group is hoisted
            // into `self.editor`; every other group lives in `editor_layout`.
            // Lay the leaves out and paint each at its rect (depth-first order),
            // reporting the active group's rect so popups anchor there.
            self.editor_layout_area = editor_area;
            let rects = self.editor_layout.leaf_rects(editor_area, EDITOR_SPLIT_MIN);
            // The layout icons sit over the right end of the editor area's top
            // row; any group's tab strip that reaches it stops short of them.
            let icons_left = if editor_area.width >= LAYOUT_ICONS_W {
                editor_area.x + editor_area.width - LAYOUT_ICONS_W
            } else {
                u16::MAX
            };
            let reserve = |r: Rect| -> u16 {
                if r.y == editor_area.y {
                    (r.x + r.width).saturating_sub(icons_left).min(r.width)
                } else {
                    0
                }
            };
            if self.scrubber.is_some() && self.scrub_for != self.editor.path {
                self.rebuild_scrub_view();
            }
            let paint_history = self.scrub_view.is_some();
            let active_idx = self.editor_layout.active_dfs_index();
            let focused_area = if self.editor_layout.is_split() {
                let mut rects = rects;
                // The active group carries the minimap (#1231).
                let minimap = self.carve_minimap(rects[active_idx]);
                if let Some((ed_rect, full_strip, _)) = minimap {
                    frame.render_widget(
                        ratatui::widgets::Block::default()
                            .style(Style::default().bg(self.theme.editor_bg())),
                        full_strip,
                    );
                    rects[active_idx] = ed_rect;
                }
                let active_area = rects[active_idx];
                self.editor.set_tab_reserved_right(reserve(active_area));
                frame.render_widget(&mut self.editor, active_area);
                if let Some((_, full_strip, _)) = minimap {
                    self.paint_minimap_header(frame.buffer_mut(), full_strip);
                }
                let inactive_rects: Vec<Rect> = rects
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != active_idx)
                    .map(|(_, r)| *r)
                    .collect();
                for (group, rect) in self
                    .editor_layout
                    .inactive_groups_mut()
                    .into_iter()
                    .zip(inactive_rects)
                {
                    group.hover_pointer = pointer;
                    group.set_tab_reserved_right(reserve(rect));
                    frame.render_widget(group, rect);
                }
                // Every boundary in the grid is a draggable seam (one per
                // internal child edge of every split node, any depth).
                self.editor_seams = self.editor_layout.seams(editor_area, EDITOR_SPLIT_MIN);
                // Image overlays stay keyed by physical column (0 = left).
                if rects.len() == 2 {
                    self.update_editor_image_overlay(0, rects[0]);
                    self.update_editor_image_overlay(1, rects[1]);
                } else {
                    self.update_editor_image_overlay(0, active_area);
                    self.disable_editor_image(1);
                }
                match minimap {
                    Some((_, _, img_rect)) => self.paint_minimap(frame, img_rect),
                    None => self.disable_minimap_image(),
                }
                active_area
            } else if let Some((ed_rect, full_strip, img_rect)) = self.carve_minimap(editor_area) {
                // Bg-fill the whole strip column so no stale cells show
                // through, then paint the editor into the narrowed rect and
                // bake the minimap raster into the text-aligned sub-rect.
                frame.render_widget(
                    ratatui::widgets::Block::default()
                        .style(Style::default().bg(self.theme.editor_bg())),
                    full_strip,
                );
                self.editor.set_tab_reserved_right(reserve(ed_rect));
                frame.render_widget(&mut self.editor, ed_rect);
                self.paint_minimap_header(frame.buffer_mut(), full_strip);
                self.editor_seams.clear();
                self.update_editor_image_overlay(0, ed_rect);
                self.disable_editor_image(1);
                self.paint_minimap(frame, img_rect);
                ed_rect
            } else {
                self.editor.set_tab_reserved_right(reserve(editor_area));
                frame.render_widget(&mut self.editor, editor_area);
                self.editor_seams.clear();
                self.update_editor_image_overlay(0, editor_area);
                // No right pane: make sure its slot can't ghost an image.
                self.disable_editor_image(1);
                self.disable_minimap_image();
                editor_area
            };
            // The history scrubber's view stands in for the live editor's
            // text (#371); the tab strip stays the live one, while the breadcrumbs
            // and Outline follow the commit (see `sync_outline`).
            if paint_history
                && let Some(view) = self.scrub_view.as_mut()
                && self.editor.last_body.width > 0
            {
                view.theme = self.theme;
                // The editor paints only the rows it has text for, so a
                // shorter historical file would leave the live buffer's
                // lower lines showing through: blank the body first.
                let body = self.editor.last_body;
                frame.render_widget(ratatui::widgets::Clear, body);
                frame.render_widget(
                    ratatui::widgets::Block::default()
                        .style(Style::default().bg(self.theme.editor_bg())),
                    body,
                );
                frame.render_widget(view, body);
            }
            self.render_scrub_slider(frame);
            if self.focus == Pane::Editor
                && self.completion_popup.is_some()
                && let Some((cx, cy)) = self.editor.cursor_screen_pos()
            {
                if let Some(popup) = self.completion_popup.as_mut() {
                    popup.anchor = (cx, cy);
                    popup.theme = self.theme;
                }
                let popup_ref = self.completion_popup.as_ref().unwrap();
                let area = popup_ref.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(popup_ref, area);
                    // The selected item's signature and documentation (#1527).
                    if let Some(details) = popup_ref.details_area_for(area, focused_area) {
                        frame.render_widget(popup_ref.details().beside(area), details);
                    }
                }
            }
            // Parameter hints sit ABOVE the caret, so they coexist with the
            // completion popup below it.
            if self.focus == Pane::Editor
                && self.signature_help_popup.is_some()
                && let Some((cx, cy)) = self.editor.cursor_screen_pos()
            {
                if let Some(popup) = self.signature_help_popup.as_mut() {
                    popup.anchor = (cx, cy);
                    popup.theme = self.theme;
                }
                let popup_ref = self.signature_help_popup.as_ref().unwrap();
                let area = popup_ref.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(popup_ref, area);
                }
            }
            if let Some(popup) = self.tab_tooltip.as_mut() {
                popup.theme = self.theme;
                let area = popup.area_for(focused_area);
                if area.width > 0 && area.height > 0 {
                    frame.render_widget(&*popup, area);
                }
            }
            // Layout-control icons at the top-right of the editor tab strip.
            // A collapsed editor (the panel maximized over it) has no strip,
            // and its row is the panel's tab row: a one-row strip there would
            // leave the icons' hit rects over the panel's buttons (#1980).
            let strip_row = Rect {
                x: editor_area.x,
                y: editor_area.y,
                width: editor_area.width,
                height: editor_area.height.min(1),
            };
            self.paint_layout_icons(frame, strip_row);
        }
        if let Some(area) = terminal_area {
            // Left/Right panel alignment narrows the terminal to one half of
            // its band; paint the leftover strip with the editor background so
            // the vacated cells don't ghost the previous frame.
            if let Some(band) = terminal_band
                && (band.width != area.width || band.x != area.x)
            {
                frame.render_widget(
                    ratatui::widgets::Block::default()
                        .style(Style::default().bg(self.theme.editor_bg())),
                    band,
                );
            }
            // The panel group's tab strip (PROBLEMS / TERMINAL) takes the top
            // row; the active tab's view fills the rest. Mirrors VS Code's
            // bottom-panel tab bar.
            // At a one-row window the band is zero rows high just below the
            // frame, and a one-row strip there writes outside the buffer
            // (#1133). A panel of one row gives that row to its view below, so
            // it has no tab row either: one painted there is drawn over, and
            // its tabs and buttons would stay clickable under the panes'
            // cells (#1980 review).
            let strip = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: u16::from(area.height > 1),
            };
            let content = if area.height > 1 {
                Rect {
                    x: area.x,
                    y: area.y + 1,
                    width: area.width,
                    height: area.height - 1,
                }
            } else {
                area
            };
            self.paint_panel_tabs(frame, strip);
            // The render-time pointer cell, fed to the panel's views as it is
            // to the side bar's above, so the row under the mouse lights up on
            // every move and goes dark when the pointer leaves (#2068).
            let panel_pointer = self.pointer_cell;
            self.problems.hover_pointer = panel_pointer;
            self.output.hover_pointer = panel_pointer;
            self.ports.hover_pointer = panel_pointer;
            self.captures.hover_pointer = panel_pointer;
            match self.bottom_panel_tab {
                BottomPanelTab::Terminal => {
                    let n = self.terminals.len().max(1);
                    let multi = self.terminals.len() > 1;
                    let maximized = self.terminal_pane_maximized && multi;
                    self.clear_terminal_rail();
                    // Maximize mode: the active pane owns the panel's width
                    // minus a right rail listing every terminal (VS Code's
                    // terminal-tabs list); hidden panes drop their hit rects
                    // so clicks can't phantom-match. Otherwise: even split.
                    let cols: Vec<Rect> = if maximized {
                        // The rail is a fixed, very narrow switcher column:
                        // icon + 4 label characters, nothing adaptive.
                        let rail_w = TERMINAL_RAIL_W.min(content.width.saturating_sub(20));
                        let pane = Rect {
                            x: content.x,
                            y: content.y,
                            width: content.width - rail_w,
                            height: content.height,
                        };
                        let rail = Rect {
                            x: content.x + pane.width,
                            y: content.y,
                            width: rail_w,
                            height: content.height,
                        };
                        self.paint_terminal_rail(frame, rail);
                        (0..n)
                            .map(|i| {
                                if i == self.active_terminal {
                                    pane
                                } else {
                                    Rect::default()
                                }
                            })
                            .collect()
                    } else {
                        // Collapsed panes (#313) keep a one-column strip and
                        // the rest divide what is left. Read off the panes
                        // rather than a list held here, so a reorder or a
                        // close can never leave the flags naming the wrong
                        // terminal.
                        let collapsed: Vec<bool> = (0..n)
                            .map(|i| self.terminals.get(i).is_some_and(|t| t.collapsed))
                            .collect();
                        Layout::default()
                            .direction(Direction::Horizontal)
                            .constraints(terminal_pane_constraints(&collapsed))
                            .split(content)
                            .to_vec()
                    };
                    // Theme the terminal background like VS Code's integrated
                    // terminal: prefill each pane with the theme bg before the
                    // grid paints. Shell-colored cells override it; default-bg
                    // cells (Style bg = None) keep the fill. On iTerm2 this
                    // matches the SetColors session bg; on Ghostty/Kitty (which
                    // ignores SetColors) it's what makes the terminal follow the
                    // theme at all instead of showing the host's own bg.
                    let term_bg = self.theme.editor_bg();
                    for (i, t) in self.terminals.iter_mut().enumerate() {
                        // A collapsed pane is not rendered, and that is what
                        // keeps its shell intact: rendering is what resizes
                        // the PTY, and reflowing a running shell into one
                        // column would mangle everything already on its
                        // screen. It keeps the geometry it had, so expanding
                        // brings the pane back as it was rather than as a
                        // column of broken wrapping. Its `last_area` is
                        // cleared so a click on the strip cannot land in a
                        // grid that is no longer painted there.
                        if cols[i].width == 0 || (t.collapsed && !maximized) {
                            t.last_area = Rect::default();
                            // `last_inner` too (#510): it anchors the image
                            // overlay's raw CUP write, so a stale rect sends
                            // the payload outside the pane with nothing to
                            // catch it. The other terminal-pane clears below
                            // need no counterpart - the overlay returns early
                            // when the panel is hidden or off the Terminal
                            // tab, which is what makes them unreachable for
                            // it. A consumer that drops that guard inherits
                            // this obligation.
                            t.last_inner = Rect::default();
                            t.redacted_on_screen = 0;
                        } else {
                            frame.render_widget(
                                ratatui::widgets::Block::default()
                                    .style(Style::default().bg(term_bg)),
                                cols[i],
                            );
                            frame.render_widget(t, cols[i]);
                        }
                    }
                    let show_close = multi;
                    self.terminal_add_buttons.clear();
                    self.terminal_profile_buttons.clear();
                    self.terminal_close_buttons.clear();
                    self.terminal_label_rects.clear();
                    self.terminal_collapse_buttons.clear();
                    self.terminal_strip_rects.clear();
                    // Croft Loch hangs a lone terminal's `∨ +` on the tab row,
                    // just left of the panel chevron, as VS Code does: the
                    // panel's controls then share one row instead of stepping
                    // down onto the pane, whose top edge stays a clean rule.
                    // Only when both fit between the last tab and the chevron;
                    // otherwise they keep the pane's top row.
                    let header_slot = (crate::widgets::surface::tones(self.theme).is_some()
                        && self.terminals.len() == 1
                        && !self.terminals[0].collapsed
                        && strip.height == 1
                        && !self.panel_maximize_button.is_empty())
                    .then(|| {
                        let tabs_end = [
                            self.problems_tab_rect,
                            self.output_tab_rect,
                            self.terminal_tab_rect,
                            self.ports_tab_rect,
                            self.captures_tab_rect,
                        ]
                        .iter()
                        .map(|r| r.right())
                        .max()
                        .unwrap_or(strip.x);
                        let x0 = tabs_end + 1;
                        // The buttons keep one blank column at their right
                        // edge; ending the slot one cell into the chevron
                        // lays that column on the chevron's own padding, so
                        // `+` sits two cells from the chevron's glyph, the
                        // same step as between `∨` and `+`.
                        let x1 = self.panel_maximize_button.x + 1;
                        Rect::new(x0, strip.y, x1.saturating_sub(x0), 1)
                    })
                    .filter(|slot| {
                        let need = TERMINAL_ADD_LABEL.chars().count()
                            + TERMINAL_PROFILE_LABEL.chars().count()
                            + 2;
                        usize::from(slot.width) >= need
                    });
                    for (i, col) in cols.iter().enumerate().take(self.terminals.len()) {
                        // Hidden panes still push placeholders so a button's
                        // position in each vec names its terminal index.
                        if col.width == 0 {
                            self.terminal_add_buttons.push(Rect::default());
                            self.terminal_profile_buttons.push(Rect::default());
                            self.terminal_close_buttons.push(Rect::default());
                            self.terminal_label_rects.push(Rect::default());
                            self.terminal_collapse_buttons.push(Rect::default());
                            self.terminal_strip_rects.push(Rect::default());
                            continue;
                        }
                        // A collapsed pane's whole chrome is its strip: no
                        // header buttons fit in one column, and the only
                        // gesture it offers is the way back out.
                        if self.terminals[i].collapsed && !maximized {
                            let strip = self.paint_terminal_collapsed_strip(frame, *col, i);
                            self.terminal_add_buttons.push(Rect::default());
                            self.terminal_profile_buttons.push(Rect::default());
                            self.terminal_close_buttons.push(Rect::default());
                            self.terminal_label_rects.push(Rect::default());
                            self.terminal_collapse_buttons.push(Rect::default());
                            self.terminal_strip_rects.push(strip);
                            continue;
                        }
                        self.terminal_strip_rects.push(Rect::default());
                        let buttons = paint_terminal_pane_buttons(
                            frame,
                            header_slot.unwrap_or(*col),
                            show_close,
                            maximized,
                            self.theme,
                            self.pointer_cell,
                        );
                        self.terminal_add_buttons
                            .push(buttons.add.unwrap_or_default());
                        self.terminal_profile_buttons
                            .push(buttons.profile.unwrap_or_default());
                        self.terminal_close_buttons
                            .push(buttons.close.unwrap_or_default());
                        self.terminal_collapse_buttons
                            .push(buttons.collapse.unwrap_or_default());
                        // Auto/manual pane label at top-left, only with 2+ panes
                        // (a lone pane needs no name). Kept clear of the buttons.
                        // The painted pill doubles as the drag-reorder handle,
                        // so its rect is recorded index-aligned with the pane.
                        let mut label_rect = Rect::default();
                        if multi {
                            let label = self.terminals[i].label().to_string();
                            // The pill is painted AFTER the header buttons and
                            // wins the shared cells, so its budget has to
                            // clear the button strip plus the pill's own
                            // widest decoration (a leading `⚠` or `⇶` and the
                            // two spaces every pill carries).
                            let room =
                                col.width.saturating_sub(TERMINAL_BUTTON_STRIP_W + 4) as usize;
                            // While broadcast input is on, every pane that
                            // receives the mirrored keystrokes wears an
                            // unmissable red pill with a ⇶ marker
                            // (Terminator's role colouring); excluded panes
                            // keep the normal pill. The marker paints even
                            // while the pane's auto-label is still empty.
                            let receiving = self.broadcast_input
                                && (i == self.active_terminal
                                    || !self.terminals[i].broadcast_excluded);
                            // An agent pane wears its badge in the pill
                            // (`◆ claude ◐`, #344); the badge takes the room
                            // first and the label is clipped to what is left.
                            let badge = self.terminals[i]
                                .agent()
                                .map(|a| format!("\u{25c6} {} {}", a.name, a.status.glyph()));
                            let label = match &badge {
                                Some(b) if label.is_empty() => b.clone(),
                                Some(b) => format!("{b} {label}"),
                                None => label,
                            };
                            if (!label.is_empty() || receiving) && room >= 3 {
                                let shown: String = label.chars().take(room).collect();
                                let mut text = if receiving && shown.is_empty() {
                                    String::from(" \u{21f6} ")
                                } else if receiving {
                                    format!(" \u{21f6} {shown} ")
                                } else {
                                    format!(" {shown} ")
                                };
                                // A live OSC 9;4 percent joins the pill (the
                                // border gauge is the primary drawing; this
                                // keeps unfocused panes glanceable). The
                                // indeterminate state has no number to show.
                                if let Some((state, pct)) = self.terminals[i].progress()
                                    && state != 3
                                    && text.chars().count() + 6 <= room
                                {
                                    text = format!("{}\u{b7} {pct}% ", text);
                                }
                                let accent = self.terminals[i].accent;
                                let text = if accent.is_some() && !receiving {
                                    format!(" \u{26a0}{text}")
                                } else {
                                    text
                                };
                                let style = if receiving {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)))
                                        .add_modifier(Modifier::BOLD)
                                } else if let Some((r, g, b)) = accent {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(Color::Rgb(r, g, b))
                                        .add_modifier(Modifier::BOLD)
                                } else if let Some(t) = crate::widgets::surface::tones(self.theme) {
                                    // Croft Loch names the pane with the
                                    // keyboard in the strong ink, the rest
                                    // muted, so the labels say where focus is.
                                    Style::default().fg(if self.terminals[i].focused {
                                        t.strong()
                                    } else {
                                        t.muted()
                                    })
                                } else {
                                    crate::widgets::header_pill::action_style(self.theme, false)
                                };
                                // `room` bounds how much of the name is worth
                                // showing, but it counts CHARACTERS while the
                                // buffer advances by display CELLS, so a CJK
                                // or emoji name is twice as wide as its budget
                                // and would still reach the buttons. Clip the
                                // paint to the cells before the button strip —
                                // `set_stringn` measures grapheme width, which
                                // is the same clamp the rail rows use — and
                                // take the drag handle's width from what was
                                // actually painted.
                                let budget =
                                    col.width.saturating_sub(TERMINAL_BUTTON_STRIP_W) as usize;
                                let (end_x, _) = frame
                                    .buffer_mut()
                                    .set_stringn(col.x, col.y, text, budget, style);
                                label_rect = Rect {
                                    x: col.x,
                                    y: col.y,
                                    width: end_x.saturating_sub(col.x),
                                    height: 1,
                                };
                            }
                        }
                        self.terminal_label_rects.push(label_rect);
                    }
                }
                BottomPanelTab::Problems => {
                    // The terminals are hidden behind the PROBLEMS view; drop
                    // their hit rects so a click never phantom-matches a pane
                    // that isn't painted (mirrors the hidden-panel reset).
                    self.terminal_add_buttons.clear();
                    self.terminal_profile_buttons.clear();
                    self.terminal_close_buttons.clear();
                    self.terminal_label_rects.clear();
                    self.terminal_collapse_buttons.clear();
                    self.terminal_strip_rects.clear();
                    self.clear_terminal_rail();
                    for t in self.terminals.iter_mut() {
                        t.last_area = Rect::default();
                        t.redacted_on_screen = 0;
                    }
                    frame.render_widget(&mut self.problems, content);
                    self.paint_panel_view_top(frame, content);
                }
                BottomPanelTab::Output => {
                    // Same hidden-terminal reset as PROBLEMS: the OUTPUT view
                    // covers the panes, so their hit rects must not linger.
                    self.terminal_add_buttons.clear();
                    self.terminal_profile_buttons.clear();
                    self.terminal_close_buttons.clear();
                    self.terminal_label_rects.clear();
                    self.terminal_collapse_buttons.clear();
                    self.terminal_strip_rects.clear();
                    self.clear_terminal_rail();
                    for t in self.terminals.iter_mut() {
                        t.last_area = Rect::default();
                        t.redacted_on_screen = 0;
                    }
                    frame.render_widget(&mut self.output, content);
                    self.paint_panel_view_top(frame, content);
                }
                BottomPanelTab::Ports => {
                    // Same hidden-terminal reset: the PORTS view covers the
                    // panes, so their hit rects must not linger.
                    self.terminal_add_buttons.clear();
                    self.terminal_profile_buttons.clear();
                    self.terminal_close_buttons.clear();
                    self.terminal_label_rects.clear();
                    self.terminal_collapse_buttons.clear();
                    self.terminal_strip_rects.clear();
                    self.clear_terminal_rail();
                    for t in self.terminals.iter_mut() {
                        t.last_area = Rect::default();
                        t.redacted_on_screen = 0;
                    }
                    frame.render_widget(&mut self.ports, content);
                    self.paint_panel_view_top(frame, content);
                }
                BottomPanelTab::Captures => {
                    // Same hidden-terminal reset as the PORTS arm above.
                    self.terminal_add_buttons.clear();
                    self.terminal_profile_buttons.clear();
                    self.terminal_close_buttons.clear();
                    self.terminal_label_rects.clear();
                    self.terminal_collapse_buttons.clear();
                    self.terminal_strip_rects.clear();
                    self.clear_terminal_rail();
                    for t in self.terminals.iter_mut() {
                        t.last_area = Rect::default();
                        t.redacted_on_screen = 0;
                    }
                    frame.render_widget(&mut self.captures, content);
                    self.paint_panel_view_top(frame, content);
                }
            }
        } else {
            self.terminal_add_buttons.clear();
            self.terminal_profile_buttons.clear();
            self.terminal_close_buttons.clear();
            self.terminal_focus_button = Rect::default();
            self.panel_maximize_button = Rect::default();
            self.terminal_label_rects.clear();
            self.terminal_collapse_buttons.clear();
            self.terminal_strip_rects.clear();
            self.clear_terminal_rail();
            self.problems_tab_rect = Rect::default();
            self.output_tab_rect = Rect::default();
            self.ports_tab_rect = Rect::default();
            self.captures_tab_rect = Rect::default();
            self.terminal_tab_rect = Rect::default();
            // A hidden terminal must not keep stale hit rects alive, or
            // `terminal_at_pos` would phantom-match clicks meant for
            // whatever now occupies those cells (mirrors the SYSTEM
            // panel's hidden-state reset).
            for t in self.terminals.iter_mut() {
                t.last_area = Rect::default();
                t.redacted_on_screen = 0;
            }
        }

        if let Some(s) = self.theme.surfaces() {
            let status_bar = (status_h > 0 && osk_h == 0).then_some(outer[2]);
            self.paint_surface_seams(
                frame,
                s,
                activity_area,
                [side_area, secondary_area],
                terminal_area,
                status_bar,
            );
        }

        // Owned snapshot: the spans borrow the status, and borrowing it
        // through scm_worker() would pin all of &self for the whole render.
        let scm_status = self.scm_worker().status().clone();
        let status_bg = status_bar_bg(self.theme);
        let tones = StatusTones::new(self.theme, status_bg);
        let mut spans: Vec<Span> = Vec::with_capacity(26);
        let workspace_name = self
            .workspace_root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.workspace_root().display().to_string());
        spans.extend(workspace_cap_spans(self.theme, &workspace_name));
        if self.overwrite_mode {
            spans.push(Span::raw(" "));
            spans.push(overwrite_mode_span());
        }
        let redacted = self.redacted_on_screen();
        if redacted > 0 || self.redactions_revealed() {
            // How much of what is on screen is masked right now (#360): a
            // screen-share host wants to know the count is non-zero, and
            // that a reveal window is open.
            let (label, bg) = if self.redactions_revealed() {
                (
                    format!("\u{1f441} {redacted} revealed"),
                    Color::Rgb(0xc0, 0x39, 0x2b),
                )
            } else {
                (
                    format!("\u{2022} {redacted} redacted"),
                    Color::Rgb(0x3a, 0x3f, 0x4b),
                )
            };
            spans.extend(status_pill(&label, self.theme.ui(bg), Color::White));
            spans.push(Span::raw(" "));
        }
        if let Some(span) = self.perf.status_span() {
            spans.push(span);
        }
        // The pill the update card rises from and Later folds it into; a
        // click opens or folds the card. Recorded as (start column, width).
        let offer_chip_x: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
        if let Some(toast) = self.update_toast.as_ref().filter(|t| !t.ready) {
            let hovered =
                crate::widgets::hover::contains(self.status_offer_rect, self.pointer_cell);
            spans.extend(crate::widgets::update_popup::pill_spans(
                &format!(
                    "{} Update Available: v{}",
                    crate::widgets::update_popup::PACKAGE_GLYPH,
                    toast.release.version
                ),
                crate::widgets::update_popup::pill_fill(self.theme, hovered),
                Color::White,
            ));
        }
        let offer_chip_end: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
        let offer_chip =
            (offer_chip_end > offer_chip_x).then(|| (offer_chip_x, offer_chip_end - offer_chip_x));
        if offer_chip.is_some() {
            spans.push(Span::raw(" "));
        }
        // Both update pills are one click target, doing what the updater
        // chord does (#865); recorded as (start column, width).
        let update_chip_x: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
        if self.any_update_ready() {
            spans.extend(crate::widgets::update_popup::pill_spans(
                &format!(
                    "{} Update ready - {UPDATE_CROFT_CHORD} to relaunch",
                    crate::widgets::update_popup::READY_GLYPH
                ),
                self.theme.ui(Color::Rgb(0x1f, 0x7a, 0x33)),
                Color::White,
            ));
        }
        if let Some(current) = self.local_drift.as_ref()
            && self.update_status == UpdateStatus::Idle
            && self.self_install.is_none()
            // The same latch as `update_armed`: while the staged release
            // builds, the chord is not a rebuild, so the hint must not say so.
            && self.staged_status != UpdateStatus::InProgress
            && self.staged_install.is_none()
        {
            // Amber, persistent: the binary predates its own source tree
            // (#242). One keypress rebuilds it in the background.
            spans.push(Span::styled(
                format!(
                    " ⟳ croft {} < repo {current} - {UPDATE_CROFT_CHORD} to rebuild ",
                    env!("CROFT_GIT_HASH")
                ),
                Style::default()
                    .bg(self.theme.ui(Color::Rgb(0x8a, 0x60, 0x00)))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let update_chip_end: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
        let update_chip = (update_chip_end > update_chip_x)
            .then(|| (update_chip_x, update_chip_end - update_chip_x));
        if self.any_update_in_progress() {
            // Red + a spinning circular-arrow glyph to draw the eye to the
            // in-progress background update. It is the update's one progress
            // indicator (#1852), so a staged release names the version the
            // popup offered before it closed.
            let target = self
                .staged_install
                .as_ref()
                .filter(|_| self.staged_status == UpdateStatus::InProgress)
                .map(|s| format!(" to v{}", s.version))
                .unwrap_or_default();
            spans.push(Span::styled(
                format!(
                    " {} Updating croft{target} in the background ",
                    self.update_spinner_glyph()
                ),
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0xff, 0x45, 0x4d)))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(progress) = self.lsp_progress_status() {
            // A busy language server (rust-analyzer priming its crate graph,
            // etc.) is otherwise invisible: hover/completion just return empty
            // until it finishes. Surfacing "rust-analyzer: Indexing " tells
            // the user to wait rather than assume the server is dead.
            spans.push(Span::styled(
                format!(" ⟳ {progress} "),
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(label) = &self.mcp_busy_label {
            // An in-flight extension command (install + tool call on a worker
            // thread) is otherwise invisible; an animated badge says it's working.
            spans.push(Span::styled(
                format!(" {} {label} ", self.mcp_spinner_glyph()),
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0x2d, 0xd4, 0xbf)))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some((agent, until)) = &self.auto_approve {
            let left = until
                .saturating_duration_since(std::time::Instant::now())
                .as_secs();
            spans.extend(status_pill(
                &format!(
                    "\u{2713} auto-approving {agent} {}:{:02}",
                    left / 60,
                    left % 60
                ),
                self.theme.ui(Color::Rgb(0x8a, 0x60, 0x00)),
                Color::White,
            ));
            spans.push(Span::raw(" "));
        }
        if let Some(chip) = self.notebook_kernel_chip() {
            spans.push(Span::styled(
                chip,
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0xf3, 0x9c, 0x12)))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        // A running CodeQL query (#578): what runs and for how long, and a
        // click on it cancels the run.
        let mut codeql_chip: Option<(u16, u16)> = None;
        if let Some((_, since)) = &self.codeql_run {
            let secs = since.elapsed().as_secs();
            let pill = status_pill(
                &format!(
                    "\u{27f3} CodeQL {} {}:{:02} \u{2715}",
                    self.codeql_run_name,
                    secs / 60,
                    secs % 60
                ),
                self.theme.ui(Color::Rgb(0x2b, 0x5d, 0x8a)),
                Color::White,
            );
            let x: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
            let w: u16 = pill.iter().map(|s| status_cells(&s.content)).sum();
            codeql_chip = Some((x, w));
            spans.extend(pill);
            spans.push(Span::raw(" "));
        }
        if let Some((_, _, started)) = &self.recording {
            // A terminal recording (#356) is the other state a user can
            // leave on; it is not the macro recorder, so it has its own
            // badge and says how long it has run.
            let secs = started.elapsed().as_secs();
            spans.extend(status_pill(
                &format!("\u{25cf} CAST {}:{:02}", secs / 60, secs % 60),
                self.theme.ui(Color::Rgb(0x8a, 0x1c, 0x5a)),
                Color::White,
            ));
            spans.push(Span::raw(" "));
        }
        if let Some(rec) = &self.macro_recording {
            // Recording is state a user can forget they left on, so it gets a
            // filled red badge rather than a dim hint (#255). A named register
            // shows which one, since vim users keep several.
            let label = match rec.register {
                Some(r) => format!("● REC @{r}"),
                None => String::from("● REC"),
            };
            spans.extend(status_pill(
                &label,
                self.theme.ui(Color::Rgb(0xc0, 0x2b, 0x2b)),
                Color::White,
            ));
            spans.push(Span::raw(" "));
        }
        if self.vim.enabled {
            let (text, bg) = match self.vim.command_line() {
                Some(cmd) => (cmd.to_string(), self.theme.ui(Color::Rgb(0x33, 0x33, 0x33))),
                None => {
                    let label = self.vim.mode_label().unwrap_or("NORMAL");
                    let bg = match self.vim.mode() {
                        crate::vim::VimMode::Insert => self.theme.ui(Color::Rgb(0x1f, 0x7a, 0x33)),
                        crate::vim::VimMode::Visual | crate::vim::VimMode::VisualLine => {
                            self.theme.ui(Color::Rgb(0x8a, 0x4f, 0xbf))
                        }
                        _ => self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)),
                    };
                    (label.to_string(), bg)
                }
            };
            spans.extend(status_pill(&text, bg, Color::White));
            spans.push(Span::raw(" "));
        }
        // Zones are set apart by air rather than by drawn dividers, the way
        // VS Code's status bar spaces its items: the bar reads as a few
        // quiet groups instead of a ruled table. The spacer keeps its one
        // cell, so no click target moves.
        let zone_div = Style::default();
        let div = " ";
        let scm_spans = git_status_spans(&scm_status, self.theme);
        // The git spans open with their own air and the cap ends on its
        // rounded edge, so the spacer takes one more cell only when the
        // branch segment painted.
        let scm_present = !scm_spans.is_empty();
        spans.extend(scm_spans);
        spans.push(Span::styled(
            if scm_present {
                format!(" {div}")
            } else {
                div.to_string()
            },
            zone_div,
        ));
        // Persistent orange badge for a remote session with no dtach: it can't
        // survive a transport drop, so flag it for the whole session (rendered
        // before the transient status so a status update never hides it).
        if let Some(warning) = self.persistence_warning {
            spans.extend(status_pill(
                &format!("\u{26a0} {warning}"),
                self.theme.ui(Color::Rgb(0xff, 0x9d, 0x2f)),
                self.theme.ui(Color::Rgb(0x10, 0x13, 0x1a)),
            ));
            spans.push(Span::raw(" "));
        }
        // Multiplayer roster badge: how many clients are attached to this
        // persistent session, painted only when someone else is here
        // (manage them via Session: Participants, Cmd+K A).
        if self.session_participants.len() > 1 {
            spans.extend(status_pill(
                &format!("\u{f0c0} {} attached", self.session_participants.len()),
                self.theme.ui(Color::Rgb(0x2f, 0x6f, 0xd0)),
                self.theme.ui(Color::Rgb(0xff, 0xff, 0xff)),
            ));
            spans.push(Span::raw(" "));
        }
        // AI-stream badge: a pilot is streaming an edit into a shared file;
        // orange like the persistence warning (attention, not decoration),
        // naming the cancel chord so the stop path is one glance away.
        if let Some(stream) = &self.collab_stream {
            spans.extend(status_pill(
                &format!(
                    "\u{23f9} {} streaming into {} (Cmd+K X to stop)",
                    stream.name, stream.file
                ),
                self.theme.ui(Color::Rgb(0xff, 0x9d, 0x2f)),
                self.theme.ui(Color::Rgb(0x10, 0x13, 0x1a)),
            ));
            spans.push(Span::raw(" "));
        } else if let Some(host) = &self.pair_host {
            // Resident navigator seated and idle: presence, not attention —
            // the stream badge above takes over whenever it is typing.
            spans.push(Span::styled(
                format!(" \u{25c6} {} seated ", host.title()),
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0xff, 0x9d, 0x2f)))
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        // Diagnostics counts (click → PROBLEMS), VS Code-style, after git.
        // Always shown so the bar reads as a health indicator at a glance,
        // but a count of zero stays dim: red and amber are kept for a count
        // that has something to say.
        let err_count = self.problems.error_count();
        let warn_count = self.problems.warning_count();
        let diag_start_col: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
        // diag_len is measured from the very strings that render, so the hit
        // rect can never drift from the visible text.
        let diag_parts: [(String, Style); 4] = [
            (
                String::from(" \u{ea87} "),
                Style::default().fg(if err_count > 0 {
                    self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c))
                } else {
                    tones.dim
                }),
            ),
            (
                err_count.to_string(),
                Style::default().fg(if err_count > 0 { tones.text } else { tones.dim }),
            ),
            (
                String::from(" \u{ea6c} "),
                Style::default().fg(if warn_count > 0 {
                    self.theme.ui(Color::Rgb(0xe5, 0xb5, 0x4a))
                } else {
                    tones.dim
                }),
            ),
            (
                format!("{warn_count} "),
                Style::default().fg(if warn_count > 0 {
                    tones.text
                } else {
                    tones.dim
                }),
            ),
        ];
        let diag_len: u16 = diag_parts.iter().map(|(t, _)| status_cells(t)).sum();
        spans.extend(diag_parts.into_iter().map(|(t, st)| Span::styled(t, st)));
        // Transient status message (e.g. "Saved editor.rs"), no keybinding
        // cheat-sheet — those live in F1 / the Command Palette now. Set apart
        // and muted so commentary reads apart from the clickable clusters
        // around it.
        // Screen reader mode shows the announcement in the status slot, and
        // every status passes through the UI catalog (#621).
        let status_text = one_line(if self.screen_reader && !self.announcer.line.is_empty() {
            self.announcer.line.clone()
        } else {
            crate::i18n::tr(&self.status).into_owned()
        });
        // Paths inside the workspace read relative to it, as Quick Open and
        // the tabs name them: `Saved src/main.rs`, not the absolute path
        // with the workspace's own location in front. The Messages channel
        // below keeps the message as it was said.
        let status_text = workspace_relative(&status_text, self.workspace_root());
        // A message seen for the first time is kept, in full, in the Messages
        // output channel, and may take the whole bar until the next key or
        // click (#1609): elided to fit, its middle was gone for good.
        if self.status != self.status_logged {
            self.status_logged = self.status.clone();
            // The idle "Ready" says nothing worth the cluster's room.
            self.status_wide = !self.status.is_empty() && self.status != "Ready";
            if !self.status.is_empty() {
                crate::output::push(
                    crate::output::CHANNEL_MESSAGES,
                    crate::output::OutputLevel::Info,
                    &crate::i18n::tr(&self.status),
                );
            }
        }
        self.announce_pos = None;
        self.status_message_rect = Rect::default();
        let mut status_x: u16 = 0;
        if !status_text.is_empty() {
            spans.push(Span::styled(format!("{div} "), zone_div));
            let x: u16 = spans.iter().map(|s| status_cells(&s.content)).sum();
            status_x = x;
            if status_h > 0 && x < outer[2].width {
                self.announce_pos = Some((outer[2].x + x, outer[2].y));
            }
            spans.push(Span::styled(
                status_text.clone(),
                Style::default().fg(tones.muted),
            ));
        }

        if let Some(osk) = self.osk.as_mut() {
            if osk_h > 0 {
                crate::widgets::osk::render_osk(osk, outer[1], frame.buffer_mut(), self.theme);
            } else {
                // Collapsed (frame too short): zero the hit rect so the
                // invisible band can't swallow clicks.
                osk.last_area = Rect::default();
            }
        }

        let status_rect = outer[2];
        // The F1 cheat-sheet is gone; F1 / the palette still open Shortcuts.
        self.shortcuts_hit_rect = None;
        self.status_codeql_rect = match codeql_chip {
            Some((x, w)) if status_h > 0 && x < status_rect.width => Rect {
                x: status_rect.x + x,
                y: status_rect.y,
                width: w.min(status_rect.width - x),
                height: 1,
            },
            _ => Rect::default(),
        };
        self.status_update_rect = match update_chip {
            Some((x, w)) if status_h > 0 && x < status_rect.width => Rect {
                x: status_rect.x + x,
                y: status_rect.y,
                width: w.min(status_rect.width - x),
                height: 1,
            },
            _ => Rect::default(),
        };
        self.status_offer_rect = match offer_chip {
            Some((x, w)) if status_h > 0 && x < status_rect.width => Rect {
                x: status_rect.x + x,
                y: status_rect.y,
                width: w.min(status_rect.width - x),
                height: 1,
            },
            _ => Rect::default(),
        };
        // Diagnostics hit rect (left group), for click-to-PROBLEMS.
        self.status_diag_rect = if status_h > 0 {
            Rect {
                x: status_rect.x.saturating_add(diag_start_col),
                y: status_rect.y,
                width: diag_len.min(status_rect.width.saturating_sub(diag_start_col)),
                height: 1,
            }
        } else {
            Rect::default()
        };

        // ---- Right cluster: document facts, right-justified. Indentation /
        // encoding / EOL / language are clickable to change (hit rects recorded
        // below). A hairline │ marks each seam so the segments read as
        // separate controls, one shade brighter than the transient message so
        // controls and commentary stop sharing a costume; the clickable ones
        // take a hover fill under the pointer (spans are built after `rx` is
        // known, below, so hover can be computed against this frame's rects).
        // The active file's coverage rides the Ln/Col readout (#263): that
        // segment is not a click target, so the clickable ones keep their
        // indices.
        let coverage = self
            .editor
            .coverage
            .as_ref()
            .and_then(|l| l.percent)
            .map(|p| format!(" \u{b7} {p:.0}% covered"))
            .unwrap_or_default();
        // Column Selection Mode (#1560) rides the same segment, as VS Code
        // shows it beside the position.
        let column_mode = if self.editor.column_selection_mode {
            " \u{b7} Column Selection"
        } else {
            ""
        };
        let language_icon = {
            let name = self
                .editor
                .path
                .as_deref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let suffix = name
                .rfind('.')
                .map(|i| name[i..].to_string())
                .unwrap_or_default();
            crate::icons::for_path(&name, &suffix)
        };
        let mut seg_texts: Vec<String> = vec![
            format!(
                " Ln {}, Col {}{coverage}{}{column_mode} ",
                self.editor.cursor_row + 1,
                self.editor.cursor_visual_col(),
                // The file refuses this user's writes (#1379).
                if self.editor.write_protected {
                    " \u{b7} Read-only"
                } else {
                    ""
                }
            ),
            format!(" {} ", self.editor.indent_style().label()),
            format!(" {} ", self.editor.encoding.name()),
            format!(" {} ", self.editor.eol.label()),
            // The language rides a pill with the file's own icon, the
            // right-hand counterpart of the workspace cap.
            format!(
                "{PILL_CAP_L}{} {}{PILL_CAP_R}",
                language_icon.glyph,
                self.editor.language_label()
            ),
        ];
        // `2 agents · 1 waiting` (#344), only while an agent is seated; a
        // click focuses the waiting pane.
        let (seated, waiting) = self.agent_counts();
        // The review queue rides the same chip (#345): a file an agent
        // changed while you were looking elsewhere is the thing you most
        // need to be told about, and it outlives the agent's own pane.
        // The set the Explorer is drawing, not a fresh count off the ledger:
        // this is the per-frame render path, and `unreviewed_files` clones
        // every path to build a set it then throws away. Reading the cache
        // also makes the chip and the dots the SAME number by construction
        // rather than by two code paths agreeing.
        let unreviewed = self.tree.agent_touched.len();
        if seated > 0 || unreviewed > 0 {
            // With nothing seated the agent half would read "0 agents",
            // which is not what the chip is reporting any more: the queue
            // outlives the pane, so the chip becomes the queue's.
            let agents_part = if seated > 0 {
                format!(
                    "{seated} agent{}{}",
                    if seated == 1 { "" } else { "s" },
                    if waiting > 0 {
                        format!(" \u{b7} {waiting} waiting")
                    } else {
                        String::new()
                    }
                )
            } else {
                String::new()
            };
            let review_part = if unreviewed > 0 {
                // A "+" when the watcher dropped events: the count is a
                // floor, and a bare number would read as a total.
                format!(
                    "{unreviewed}{} to review",
                    if self.agent_ledger.may_be_incomplete() {
                        "+"
                    } else {
                        ""
                    }
                )
            } else {
                String::new()
            };
            let joined = match (agents_part.is_empty(), review_part.is_empty()) {
                (false, false) => format!("{agents_part} \u{b7} {review_part}"),
                (false, true) => agents_part,
                _ => review_part,
            };
            seg_texts.push(format!(" \u{25c6} {joined} "));
        }
        let widths: Vec<u16> = seg_texts.iter().map(|s| status_cells(s)).collect();
        // One cell of margin after the last segment, so the language pill's
        // rounded end does not touch the frame edge.
        let mut right_total: u16 =
            widths.iter().sum::<u16>() + (seg_texts.len() as u16 - 1) + STATUS_RIGHT_MARGIN;
        // The right cluster paints OVER the left paragraph, so an over-long
        // transient loses exactly its tail — for "Opened <deep path>" that
        // was the filename, the one informative part (#100). Middle-elide
        // the transient (always the last span) into the room that survives.
        if !status_text.is_empty() {
            let left_fixed: u16 = spans
                .iter()
                .take(spans.len().saturating_sub(1))
                .map(|s| status_cells(&s.content))
                .sum();
            let fits_beside = |right: u16| {
                status_rect
                    .width
                    .saturating_sub(right)
                    .saturating_sub(left_fixed)
                    .saturating_sub(1)
            };
            // A fresh message that does not fit beside the cluster takes the
            // cluster's room until the next key or click (#1609).
            let text_cells = |t: &str| unicode_width::UnicodeWidthStr::width(t) as u16;
            if self.status_wide && text_cells(&status_text) > fits_beside(right_total) {
                right_total = 0;
            }
            let avail = fits_beside(right_total);
            if let Some(last) = spans.last_mut()
                && text_cells(&last.content) > avail
            {
                // Keep the message's dim style through the elision so a
                // truncated transient doesn't suddenly brighten.
                *last = Span::styled(elide_middle(&status_text, avail as usize), last.style);
            }
            if status_h > 0 && status_x < status_rect.width {
                let shown = spans.last().map_or(0, |l| text_cells(&l.content));
                self.status_message_rect = Rect {
                    x: status_rect.x + status_x,
                    y: status_rect.y,
                    width: shown.min(status_rect.width - status_x),
                    height: 1,
                };
            }
        }

        // Left fills the whole bar (background + left text); the right cluster
        // overwrites the right portion.
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(status_bg)),
            status_rect,
        );
        // Reset clickable rects; re-record only when the right cluster fits.
        self.status_indent_rect = Rect::default();
        self.status_encoding_rect = Rect::default();
        self.status_eol_rect = Rect::default();
        self.status_language_rect = Rect::default();
        self.status_agents_rect = Rect::default();
        self.status_position_rect = Rect::default();
        if status_h > 0 && right_total > 0 && right_total <= status_rect.width {
            let rx = status_rect.right() - right_total;
            if self.status_message_rect.right() > rx {
                self.status_message_rect.width = rx.saturating_sub(self.status_message_rect.x);
            }
            // The right cluster paints OVER the left paragraph, so the
            // diagnostics hit rect must stop where it starts. Clipped only to
            // the frame edge, it went on answering for cells that now read
            // "Ln 1, Col 1" — opening PROBLEMS from a click on the position
            // readout, with the counts it claimed not even on screen.
            if self.status_codeql_rect.x >= rx {
                self.status_codeql_rect = Rect::default();
            } else if self.status_codeql_rect.right() > rx {
                self.status_codeql_rect.width = rx - self.status_codeql_rect.x;
            }
            if self.status_update_rect.x >= rx {
                self.status_update_rect = Rect::default();
            } else if self.status_update_rect.right() > rx {
                self.status_update_rect.width = rx - self.status_update_rect.x;
            }
            if self.status_offer_rect.x >= rx {
                self.status_offer_rect = Rect::default();
            } else if self.status_offer_rect.right() > rx {
                self.status_offer_rect.width = rx - self.status_offer_rect.x;
            }
            if self.status_diag_rect.x >= rx {
                // Entirely behind the cluster: not on screen, so not a target.
                self.status_diag_rect = Rect::default();
            } else if self.status_diag_rect.right() > rx {
                self.status_diag_rect.width = rx - self.status_diag_rect.x;
            }
            // Build the spans now that x positions are known. The settings
            // (indentation, encoding, line endings) are muted; the position
            // reads its numbers in the text tone over dim labels; the
            // language sits in a pill. A clickable segment takes a fill
            // under the pointer, the same affordance as the activity bar.
            // Ln/Col (index 0) opens Go to Line, so it hovers like the rest.
            // Hit rects recorded in the same pass.
            let seg_style = Style::default().fg(tones.muted);
            let lift = match self.theme.surfaces() {
                Some(s) => s.hairline(),
                None => self.theme.ui(Color::Rgb(0x2a, 0x31, 0x40)),
            };
            let hover_style = Style::default().bg(lift).fg(tones.text);
            let mut right: Vec<Span> = Vec::with_capacity(seg_texts.len() * 4);
            let mut cx = rx;
            for (i, text) in seg_texts.iter().enumerate() {
                if i > 0 {
                    right.push(Span::styled(div, zone_div));
                    cx = cx.saturating_add(1);
                }
                let r = Rect {
                    x: cx,
                    y: status_rect.y,
                    width: widths[i],
                    height: 1,
                };
                let hovered = self
                    .pointer_cell
                    .is_some_and(|(pc, pr)| rect_contains(r, pc, pr));
                if i == 4 {
                    // The language pill: lit in the selection tint under
                    // the pointer, so it hovers like the workspace cap reads.
                    let fill = if hovered {
                        self.theme.selection()
                    } else {
                        lift
                    };
                    let label = self.editor.language_label();
                    right.push(Span::styled(PILL_CAP_L, Style::default().fg(fill)));
                    right.push(Span::styled(
                        language_icon.glyph.to_string(),
                        Style::default().fg(language_icon.color).bg(fill),
                    ));
                    right.push(Span::styled(
                        format!(" {label}"),
                        Style::default().fg(tones.text).bg(fill),
                    ));
                    right.push(Span::styled(PILL_CAP_R, Style::default().fg(fill)));
                } else if i == 0 && !hovered {
                    // Numbers in the text tone, the words around them dim.
                    let mut run = String::new();
                    let mut run_digit = false;
                    for c in text.chars() {
                        let d = c.is_ascii_digit();
                        if d != run_digit && !run.is_empty() {
                            right.push(Span::styled(
                                std::mem::take(&mut run),
                                Style::default().fg(if run_digit { tones.text } else { tones.dim }),
                            ));
                        }
                        run_digit = d;
                        run.push(c);
                    }
                    if !run.is_empty() {
                        right.push(Span::styled(
                            run,
                            Style::default().fg(if run_digit { tones.text } else { tones.dim }),
                        ));
                    }
                } else {
                    right.push(Span::styled(
                        text.clone(),
                        if hovered { hover_style } else { seg_style },
                    ));
                }
                match i {
                    0 => self.status_position_rect = r,
                    1 => self.status_indent_rect = r,
                    2 => self.status_encoding_rect = r,
                    3 => self.status_eol_rect = r,
                    4 => self.status_language_rect = r,
                    5 => self.status_agents_rect = r,
                    _ => {}
                }
                cx = cx.saturating_add(widths[i]);
            }
            frame.render_widget(
                Paragraph::new(Line::from(right)).style(Style::default().bg(status_bg)),
                Rect {
                    x: rx,
                    y: status_rect.y,
                    width: right_total,
                    height: status_rect.height,
                },
            );
        }

        // Overlays render last so they sit on top of everything else. The port
        // toast goes first so modals/menus paint above it.
        //
        // The hover note popup (editor symbol hover / terminal annotation
        // note) renders here against the WHOLE frame, not the editor's
        // focused_area: its anchor may sit in the terminal panel, and the
        // welcome screen skips the editor-tabs branch entirely, which used to
        // drop a terminal annotation's note without painting it at all.
        if let Some(popup) = self.peek_popup.as_mut() {
            popup.theme = self.theme;
            let area = popup.area_for(frame.area());
            if area.width > 0 && area.height > 0 {
                frame.render_widget(&*popup, area);
            }
        }
        if let Some(popup) = self.hover_popup.as_mut() {
            popup.theme = self.theme;
            let area = popup.area_for(frame.area());
            popup.last_area = area;
            if area.width > 0 && area.height > 0 {
                frame.render_widget(&*popup, area);
            }
        }
        self.render_port_toast(frame);
        self.render_update_toast(frame);
        self.render_zen_hint(frame);
        self.render_context_menu(frame);
        self.render_commit_dropdown(frame);
        self.render_prompt(frame);
        self.render_local_open_confirm(frame);
        self.render_discard_confirm(frame);
        self.render_revert_hunk_confirm(frame);
        self.render_discard_all_confirm(frame);
        self.render_stash_drop_confirm(frame);
        self.render_unsaved_scm_confirm(frame);
        self.render_replace_all_confirm(frame);
        self.render_broadcast_confirm(frame);
        self.render_terminal_paste_confirm(frame);
        self.render_run_block_confirm(frame);
        self.render_unsaved_confirm(frame);
        self.render_lossy_save_confirm(frame);
        // Terminal-pane inline image: sync after the panes have painted so
        // last_inner and the scroll offset are this frame's (all gating —
        // hidden panel, alt screen, off-screen anchor — is inside).
        self.update_terminal_image_overlay();
        self.update_markdown_image_overlay();
        self.render_editor_find(frame);
        self.render_terminal_find(frame);
        self.render_file_finder(frame);
        self.render_command_palette(frame);
        // Above the pickers a tour step opens (#863), so they never hide
        // the caption explaining them.
        self.render_tour_caption(frame);
        self.render_go_to_symbol(frame);
        self.render_workspace_symbols(frame);
        self.render_process_picker(frame);
        self.render_zoxide_jump(frame);
        self.render_command_history_popup(frame);
        self.render_branch_picker(frame);
        self.render_scm_menu(frame);
        // Below the input prompt: Enter on a number or text setting opens
        // the prompt over the editor, and drawn after it the editor hid it.
        self.render_settings_editor(frame);
        self.render_input_prompt(frame);
        self.render_list_picker(frame);
        self.render_shortcuts_modal(frame);
        self.render_connect_dialog(frame);
        // The startup unsupported-terminal nudge renders last so it sits above
        // every other overlay until the user dismisses it.
        self.render_approval_popup(frame);
        self.render_terminal_warning(frame);
        self.drop_covered_update_controls(frame.buffer_mut());

        // Chrome button-hint tooltip. Rendered at the top level against the
        // whole frame (not the editor's `focused_area`) because the controls it
        // labels live in the activity bar and sidebar, outside the editor pane;
        // clamping it to the editor area would yank it away from the control or,
        // on the welcome screen, drop it entirely.
        let mut tooltip_area = None;
        if let Some(popup) = self.ui_tooltip.as_mut() {
            popup.theme = self.theme;
            let full = frame.area();
            let mut area = popup.area_for(full);
            // Keep the box clear of the activity bar's inline-image column. Those
            // codicon glyphs are a terminal graphics layer that always composites
            // ABOVE text cells, so any overlap would paint over (hide) the left of
            // the tooltip. Shift it to the right of the bar instead, like VS Code
            // floats an activity-icon hint beside the icon.
            if area.x < ACTIVITY_BAR_WIDTH {
                area.x = ACTIVITY_BAR_WIDTH;
                area.width = area.width.min(full.width.saturating_sub(area.x));
            }
            if area.width > 0 && area.height > 0 {
                frame.render_widget(&*popup, area);
                tooltip_area = Some(area);
            }
        }
        // Record the box so the post-draw flush can RE-PAINT it on top of any
        // sidebar OSC-1337 illustration (the no-repo hero / SSH empty-state).
        // Those images are emitted after ratatui's text and would otherwise
        // composite over the tooltip; re-emitting the box last wins the cell.
        self.ui_tooltip_area = tooltip_area;

        self.paint_terminal_suggestion(frame);
        self.render_screencast(frame, main_area);

        // A quick input owns the caret while it is open on a `surfaces`
        // theme: its query's, and no pane's caret blinking through the card.
        let quick_input = self.quick_input_owns_caret();
        if quick_input
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
            && let Some((cx, cy)) = self.quick_input_caret()
        {
            frame.set_cursor_position((cx, cy));
        }

        // Show the host terminal's hardware caret only when the editor is
        // focused and has no modal overlay. The DECSCUSR style is set to
        // BlinkingBar at startup, so the terminal blinks a thin vertical
        // line over the cursor cell without replacing its character.
        if self.focus == Pane::Editor
            && !quick_input
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.list_picker.is_none()
            && self.cursor_visible_phase()
        {
            let pos = if self.editor.diff.is_some() {
                self.editor.diff_caret_screen_pos()
            } else {
                self.editor.cursor_screen_pos()
            };
            if let Some((cx, cy)) = pos {
                frame.set_cursor_position((cx, cy));
            }
        }

        // Source-control commit-message caret. Same software-blink rule:
        // the user can't see where they're typing in the message box
        // without a visible caret, since `set_string` just lays the text
        // flat with no native caret cell.
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::SourceControl
            && !quick_input
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
            && let Some((cx, cy)) = self.source_control.cursor_screen_pos()
        {
            frame.set_cursor_position((cx, cy));
        }

        // Search-panel field caret and Extensions filter caret. Same software
        // blink as the commit box so all three side-panel inputs share one
        // blinking hardware caret instead of a static block (Search) or no
        // caret at all (Extensions).
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::Search
            && !quick_input
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
            && let Some((cx, cy)) = self.search.cursor_screen_pos()
        {
            frame.set_cursor_position((cx, cy));
        }
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::Extensions
            && !quick_input
            && self.context_menu.is_none()
            && self.prompt.is_none()
            && self.cursor_visible_phase()
            && let Some((cx, cy)) = self.extensions.cursor_screen_pos()
        {
            frame.set_cursor_position((cx, cy));
        }
        self.park_screen_reader_cursor(frame);
    }

    /// What screen reader mode describes this frame (#621).
    pub(super) fn a11y_snapshot(&self) -> crate::a11y::Snapshot {
        use crate::a11y::Snapshot;
        let tr = |s: &str| crate::i18n::tr(s).into_owned();
        let status = tr(&self.status);
        if let Some((focus, item)) = self.modal_a11y() {
            return Snapshot {
                focus,
                item: Some(item),
                status,
                ..Default::default()
            };
        }
        if let Some(p) = &self.prompt {
            return Snapshot {
                focus: p.label.clone(),
                item: p.error.clone(),
                status,
                ..Default::default()
            };
        }
        if let Some(menu) = &self.context_menu {
            let item = match menu.cursor_entry() {
                Some(MenuEntry::Item { label, .. } | MenuEntry::Submenu { label, .. }) => {
                    Some(label.clone())
                }
                _ => None,
            };
            return Snapshot {
                focus: tr("Menu"),
                item,
                status,
                ..Default::default()
            };
        }
        if let Some(p) = &self.command_palette {
            return Snapshot {
                focus: tr("Command Palette"),
                item: p.selected_item().map(|i| tr(i.title())),
                status,
                ..Default::default()
            };
        }
        if let Some(f) = &self.file_finder {
            return Snapshot {
                focus: tr("Go to File"),
                item: f.selected_entry().map(|e| e.rel.clone()),
                status,
                ..Default::default()
            };
        }
        if let Some(p) = &self.list_picker {
            return Snapshot {
                focus: p.title.clone(),
                item: p.selected_row().map(|r| r.label.clone()),
                status,
                ..Default::default()
            };
        }
        match self.focus {
            Pane::Editor => {
                let name = self
                    .editor
                    .path
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| tr("Untitled"));
                let row = self.editor.cursor_row;
                let diagnostic = self
                    .editor
                    .diagnostics_at(row, self.editor.cursor_col, self.workspace_root())
                    .into_iter()
                    .next()
                    .map(|(_, m)| m);
                Snapshot {
                    focus: format!("{} {name}", tr("Editor")),
                    line: Some((
                        row + 1,
                        self.editor.lines.get(row).cloned().unwrap_or_default(),
                    )),
                    diagnostic,
                    item: self
                        .completion_popup
                        .as_ref()
                        .and_then(|c| c.selected_item())
                        .map(|c| c.label.clone()),
                    status,
                    event: None,
                }
            }
            Pane::Terminal => {
                let (focus, item) = self.bottom_panel_a11y();
                Snapshot {
                    focus,
                    item,
                    status,
                    ..Default::default()
                }
            }
            Pane::Tree => {
                let (focus, item) = match self.sidebar_view {
                    SidebarView::Explorer => (
                        tr("Explorer"),
                        // The row's label, so a compact row (#1486) reads
                        // as shown, `src/main/java`, not its last folder.
                        self.tree
                            .nodes
                            .get(self.tree.selected)
                            .map(crate::widgets::file_tree::Node::label),
                    ),
                    SidebarView::Search => self.search_a11y(),
                    SidebarView::SourceControl => self.source_control_a11y(),
                    SidebarView::Remote => (tr("Remote"), None),
                    SidebarView::RunDebug => (tr("Run and Debug"), None),
                    SidebarView::Extensions => (tr("Extensions"), None),
                    SidebarView::Testing => (tr("Testing"), None),
                    SidebarView::CodeQL => (tr("CodeQL"), None),
                };
                Snapshot {
                    focus,
                    item,
                    status,
                    ..Default::default()
                }
            }
        }
    }

    /// The dialog waiting for a key, if one is open, as screen reader mode
    /// reads it (#1584): its title, then what it says and which key does
    /// what. Without this a confirmation was silent, so a reader heard
    /// nothing after Ctrl+W and could not know that D throws edits away.
    /// Topmost first, in the order they are drawn; a new dialog that waits
    /// for a key belongs here too.
    pub(super) fn modal_a11y(&self) -> Option<(String, String)> {
        let tr = |s: &str| crate::i18n::tr(s).into_owned();
        // The title, and its sentences joined for speech: a full stop
        // between them unless one already ends in a question mark.
        let join = |parts: &[String]| -> String {
            let mut item = String::new();
            for part in parts.iter().map(|p| p.trim().trim_end_matches('.')) {
                if part.is_empty() {
                    continue;
                }
                if !item.is_empty() {
                    item.push_str(if item.ends_with('?') { " " } else { ". " });
                }
                item.push_str(part);
            }
            item
        };
        // What it says, then which key does what. The announcement is cut
        // at `a11y::MAX_LINE`, so a long path, command or file list is cut
        // here instead: the keys at the end are always read.
        let said = |title: &str, body: &[String], actions: &[String]| -> (String, String) {
            let actions = join(actions);
            let mut item = join(body);
            let room = crate::a11y::MAX_LINE.saturating_sub(actions.chars().count() + 2);
            if item.chars().count() > room {
                item = if room < 2 {
                    String::new()
                } else {
                    let mut cut: String = item.chars().take(room - 1).collect();
                    cut.truncate(cut.trim_end().len());
                    cut.push('…');
                    cut
                };
            }
            if !item.is_empty() && !actions.is_empty() {
                item.push_str(if item.ends_with('?') || item.ends_with('…') {
                    " "
                } else {
                    ". "
                });
            }
            item.push_str(&actions);
            (tr(title), item)
        };
        let yes_no = |yes: &str| format!("{}. {}", tr(yes), tr("N or Escape, no"));
        if self.pending_terminal_warning {
            return Some(said(
                "Unsupported terminal",
                &[tr(
                    "Croft's icons and images need an inline-image protocol this terminal does not support",
                )],
                &[tr("Any key dismisses. D, don't show again")],
            ));
        }
        // An agent's edit proposal takes every key while it is up, so it is
        // read before anything it covers: who wants to change which file,
        // and the keys the popup's footer offers in its current state.
        if let (Some(ui), Some(head)) = (&self.approval_ui, self.approvals.front()) {
            let title = crate::widgets::approval_popup::title(
                head,
                self.approvals.len(),
                self.workspace_root(),
            )
            .trim()
            .replace(" · ", ", ");
            let actions = if ui.reason.is_some() {
                "Type the reason. Enter, send. Escape, back"
            } else if ui.target_dirty {
                "Unsaved edits here. Enter, merge them with the agent's. Escape, deny. R, deny with a reason"
            } else {
                "Enter, approve. Escape, deny. A, approve all from this agent for 10 minutes. R, deny with a reason. E, edit, then approve"
            };
            return Some((title, tr(actions)));
        }
        // The field's own instruction ("Remote URL") is read with the keys,
        // so a prompt replaced by the next step under the same title is
        // still heard; what is typed is left to the reader's echo.
        if let Some(p) = &self.input_prompt {
            let hint = p
                .hint
                .clone()
                .unwrap_or_else(|| String::from("Enter to confirm, Escape to cancel"));
            return Some(said(&p.title, &[tr(&p.placeholder)], &[tr(&hint)]));
        }
        if let Some(ed) = &self.settings_editor {
            let target = if ed.target == crate::config_layers::LayerKind::Workspace {
                tr("Settings, writing to workspace")
            } else {
                tr("Settings, writing to user")
            };
            let row = ed.selected_row().map_or_else(
                || tr("No matching setting"),
                |r| {
                    format!(
                        "{}, {}, {}",
                        r.key.replace('_', " "),
                        crate::settings_editor::display(&r.value),
                        tr(r.layer.label())
                    )
                },
            );
            return Some((target, row));
        }
        // Over the unsaved-changes prompt when both are up, as it is drawn.
        if let Some(asked) = &self.pending_lossy_save {
            return Some(said(
                "Save would lose data",
                &asked.spoken(),
                &[tr("S, save anyway"), tr("Escape, cancel")],
            ));
        }
        if let Some(exit) = &self.pending_unsaved {
            let (headline, names, save, discard) = self.unsaved_confirm_text(exit);
            // The tab label carries the dirty dot, which a reader would read
            // out as "bullet"; every file named here is unsaved anyway.
            let names = names.replace("● ", "");
            return Some(said(
                "Unsaved changes",
                &[format!("{} {names}", tr(&headline))],
                &[
                    format!("S, {}", tr(&format!("S{save}"))),
                    format!("D, {}", tr(&format!("D{discard}"))),
                    tr("Escape, cancel"),
                ],
            ));
        }
        if let Some(block) = &self.pending_run_block {
            let title = if block.destructive {
                "Run this block? It looks destructive"
            } else {
                "Run this block?"
            };
            return Some(said(
                title,
                &[
                    format!(
                        "{} {} {} {}",
                        tr("in pane"),
                        block.pane_name,
                        tr("at"),
                        block.cwd.display()
                    ),
                    block.code.lines().collect::<Vec<_>>().join("; "),
                ],
                &[yes_no("Y, yes, run")],
            ));
        }
        if self.pending_broadcast_enable {
            let receiving = self
                .terminals
                .iter()
                .filter(|t| !t.broadcast_excluded)
                .count()
                .max(1);
            return Some(said(
                "Broadcast input to all panes?",
                &[format!(
                    "{} {receiving} {}",
                    tr("Every keystroke and paste will go to"),
                    tr("terminal panes at once")
                )],
                &[yes_no("Y, yes, broadcast")],
            ));
        }
        if let Some((occurrences, files, skipped)) = self.pending_replace_all {
            let mut parts = vec![
                format!(
                    "Replace {occurrences} occurrence(s) across {files} file(s) with \"{}\"?",
                    self.search.replace
                ),
                tr("Files are rewritten on disk"),
            ];
            if skipped > 0 {
                parts.push(format!(
                    "{skipped} open file(s) with unsaved changes will be skipped"
                ));
            }
            return Some(said(
                "Replace all?",
                &parts,
                &[tr("Enter or Y, replace. Escape, cancel")],
            ));
        }
        if let Some(pending) = &self.pending_unsaved_scm {
            let verb = pending.op.verb();
            let names: Vec<String> = pending
                .files
                .iter()
                .map(|p| {
                    p.strip_prefix(&pending.root)
                        .unwrap_or(p)
                        .display()
                        .to_string()
                })
                .collect();
            let n = names.len();
            return Some(said(
                "Unsaved changes",
                &[format!(
                    "{n} file{} {} unsaved changes: {}",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "has" } else { "have" },
                    names.join(", ")
                )],
                &[
                    format!("S, Save All & {verb}"),
                    format!("C, {verb} Anyway"),
                    tr("Escape, cancel"),
                ],
            ));
        }
        if self.pending_discard_all {
            return Some(said(
                "Discard all changes?",
                &[tr(
                    "This reverts every tracked file to HEAD. Untracked files are kept.",
                )],
                &[yes_no("Y, yes, discard all")],
            ));
        }
        if let Some(pr) = &self.pending_revert_hunk {
            return Some(said(
                "Revert hunk?",
                &[
                    tr("This will undo the change hunk under the cursor on disk."),
                    pr.rel_path.clone(),
                ],
                &[yes_no("Y, yes, revert")],
            ));
        }
        if let Some(pd) = &self.pending_discard {
            let warn = if pd.untracked {
                "This will permanently delete the file from disk."
            } else {
                "This will overwrite your local changes from HEAD."
            };
            return Some(said(
                "Discard changes?",
                &[tr(warn), pd.rel_path.clone()],
                &[yes_no("Y, yes, discard")],
            ));
        }
        if let Some(url) = &self.pending_local_open {
            return Some(said(
                "Open on local Mac?",
                &[
                    tr("This URL will open in YOUR LOCAL MAC's browser via the croft relay."),
                    url.clone(),
                ],
                &[format!(
                    "{}. {}",
                    tr("Y, yes once. A, always for this session"),
                    tr("N or Escape, no")
                )],
            ));
        }
        None
    }

    /// What screen reader mode says for the bottom panel's front tab
    /// (#1295): the tab's name with what it holds, and its row in focus.
    /// The panel is one `Pane` whichever tab is in front, so the tab, not
    /// the pane, decides.
    pub(super) fn bottom_panel_a11y(&self) -> (String, Option<String>) {
        let tr = |s: &str| crate::i18n::tr(s).into_owned();
        let with = |name: &str, what: String| format!("{}, {what}", tr(name));
        match self.bottom_panel_tab {
            BottomPanelTab::Terminal => (
                format!("{} {}", tr("Terminal"), self.active_terminal + 1),
                None,
            ),
            BottomPanelTab::Problems => {
                let errors = self.problems.error_count();
                let warnings = self.problems.warning_count();
                let others = self.problems.total_count() - errors - warnings;
                let counts: Vec<String> = [
                    (errors, "error", "errors"),
                    (warnings, "warning", "warnings"),
                    (others, "info", "infos"),
                ]
                .into_iter()
                .filter(|(n, ..)| *n > 0)
                .map(|(n, one, many)| a11y_count(n, one, many))
                .collect();
                let counts = if counts.is_empty() {
                    String::from("no problems")
                } else {
                    counts.join(", ")
                };
                let item = self.problems.top_diagnostic().map(|(group, it)| {
                    let source = match &it.code {
                        Some(code) => code.label(&it.source),
                        None => it.source.clone(),
                    };
                    let source = if source.is_empty() {
                        String::new()
                    } else {
                        format!(" ({source})")
                    };
                    format!(
                        "{} line {}: {}{source}",
                        group.name,
                        it.line + 1,
                        it.message
                    )
                });
                (with("Problems", counts), item)
            }
            BottomPanelTab::Output => (
                match self.output.selected_name() {
                    Some(channel) => with("Output", channel),
                    None => tr("Output"),
                },
                None,
            ),
            BottomPanelTab::Ports => {
                let n = self.ports.len();
                let item = self.ports.selected().map(|p| {
                    let mut said = format!("Port {}", p.port);
                    for part in [&p.process, &p.url].into_iter().flatten() {
                        said.push_str(", ");
                        said.push_str(part);
                    }
                    said
                });
                let count = if n == 0 {
                    String::from("none")
                } else {
                    a11y_count(n, "port", "ports")
                };
                (with("Ports", count), item)
            }
            BottomPanelTab::Captures => {
                let n = self.captures.len();
                let item = self
                    .captures
                    .selected_entry()
                    .map(|c| format!("{}, from {}", c.message, c.pane));
                let count = if n == 0 {
                    String::from("none")
                } else {
                    a11y_count(n, "capture", "captures")
                };
                (with("Captures", count), item)
            }
        }
    }

    /// The Search sidebar for screen reader mode (#1295): how many matches
    /// in how many files, and the selected match as `file line N: text`.
    pub(super) fn search_a11y(&self) -> (String, Option<String>) {
        let name = crate::i18n::tr("Search").into_owned();
        let hits = &self.search.hits;
        if hits.is_empty() {
            return (name, None);
        }
        // The Search header's own totals: a line with two matches counts two.
        let (matches, files) = self.search.result_counts();
        let focus = format!(
            "{name}, {} in {}",
            a11y_count(matches, "match", "matches"),
            a11y_count(files, "file", "files")
        );
        let item = self.search.selected_hit().map(|h| {
            let rel = h
                .path
                .strip_prefix(self.workspace_root())
                .unwrap_or(&h.path);
            format!("{} line {}: {}", rel.display(), h.line_no, h.line_text)
        });
        (focus, item)
    }

    /// The Source Control sidebar for screen reader mode (#1295): how many
    /// changes, and the selected one as `path, what happened to it`.
    pub(super) fn source_control_a11y(&self) -> (String, Option<String>) {
        use crate::git::ChangeKind;
        let name = crate::i18n::tr("Source Control").into_owned();
        let entries = &self.source_control.entries;
        if entries.is_empty() {
            return (name, None);
        }
        let focus = format!("{name}, {}", a11y_count(entries.len(), "change", "changes"));
        let item = self
            .source_control
            .selected_change
            .and_then(|i| entries.get(i))
            .map(|e| {
                let what = match e.kind {
                    ChangeKind::StagedAdded => "added, staged",
                    ChangeKind::StagedModified => "modified, staged",
                    ChangeKind::StagedDeleted => "deleted, staged",
                    ChangeKind::StagedRenamed => "renamed, staged",
                    ChangeKind::Modified => "modified",
                    ChangeKind::Deleted => "deleted",
                    ChangeKind::Untracked => "untracked",
                    ChangeKind::Conflicted => "conflicted",
                };
                format!("{}, {what}", e.path)
            });
        (focus, item)
    }

    /// Feed this frame's snapshot to the announcer (#621).
    pub(super) fn update_announcer(&mut self) {
        if !self.screen_reader {
            return;
        }
        let mut snap = self.a11y_snapshot();
        snap.event = self.a11y_event.clone();
        let speak = self.screen_reader_command.clone();
        self.announcer.update(snap, speak.as_deref());
    }

    /// Have screen reader mode read `text` out whatever has focus (#1547):
    /// something that happened, like a debugger pause, rather than
    /// something the user moved to. Kept only while the mode is on, so
    /// turning it on never reads out an old event.
    pub(super) fn announce_event(&mut self, text: String) {
        if !self.screen_reader {
            return;
        }
        let n = self.a11y_event.as_ref().map_or(0, |(n, _)| n + 1);
        self.a11y_event = Some((n, crate::i18n::tr(&text).into_owned()));
    }

    /// What a debugger pause reads as (#1547): why it stopped, where, and
    /// the line's text, as in `Paused on breakpoint, app.py line 3: total
    /// += n`. The line comes from the buffer when the file is the one being
    /// edited, else from disk. A stop with no source location (`at` is
    /// `None`) names its frame instead, as in `Paused on step, in memcpy`.
    pub(super) fn debug_pause_announcement(
        &self,
        reason: &str,
        at: Option<&(PathBuf, usize)>,
        frame: &str,
    ) -> String {
        let what = match reason {
            "" | "pause" => String::from("Paused"),
            r => format!("Paused on {r}"),
        };
        let Some((path, line)) = at else {
            return match frame.trim() {
                "" => what,
                f => format!("{what}, in {f}"),
            };
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let index = line.saturating_sub(1);
        let text = if self.editor.path.as_deref() == Some(path.as_path()) {
            self.editor.lines.get(index).cloned()
        } else {
            use std::io::BufRead;
            std::fs::File::open(path).ok().and_then(|f| {
                std::io::BufReader::new(f)
                    .lines()
                    .nth(index)
                    .and_then(Result::ok)
            })
        };
        match text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => format!("{what}, {name} line {line}: {t}"),
            None => format!("{what}, {name} line {line}"),
        }
    }

    /// Accessibility: Toggle Screen Reader Mode (#621), persisted.
    pub(super) fn toggle_screen_reader(&mut self) {
        self.screen_reader = !self.screen_reader;
        self.announcer.reset();
        self.a11y_event = None;
        self.status = String::from(if self.screen_reader {
            "Screen reader mode: on"
        } else {
            "Screen reader mode: off"
        });
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_screen_reader(self.screen_reader));
        }
    }

    /// Where screen reader mode parks the cursor when no caret is showing:
    /// the terminal's own cursor, else the start of the announcement.
    pub(super) fn park_screen_reader_cursor(&self, frame: &mut ratatui::Frame) {
        if !self.screen_reader {
            return;
        }
        // A picker covers the caret: read its selection from the status bar.
        // So does a dialog waiting for a key (#1584), except a text prompt,
        // whose own caret is where the typing is.
        let picker = self.command_palette.is_some()
            || self.file_finder.is_some()
            || self.list_picker.is_some()
            || (self.input_prompt.is_none() && self.modal_a11y().is_some());
        if picker && let Some(pos) = self.announce_pos {
            // A `surfaces` quick input draws no caret of its own and lends
            // the query the terminal's caret, which the announcement takes
            // here: paint a block where the typing lands, as boxed themes do.
            if self.quick_input_owns_caret()
                && let Some((cx, cy)) = self.quick_input_caret()
                && frame
                    .area()
                    .contains(ratatui::layout::Position { x: cx, y: cy })
            {
                frame.buffer_mut()[(cx, cy)]
                    .modifier
                    .insert(ratatui::style::Modifier::REVERSED);
            }
            frame.set_cursor_position(pos);
            return;
        }
        if self.cursor_should_be_visible() {
            return;
        }
        if self.focus == Pane::Terminal
            && self.prompt.is_none()
            && self.context_menu.is_none()
            && self.command_palette.is_none()
            && self.file_finder.is_none()
            && self.list_picker.is_none()
            && let Some(pos) = self
                .terminals
                .get(self.active_terminal)
                .and_then(|t| t.screen_cursor())
        {
            frame.set_cursor_position(pos);
            return;
        }
        if let Some(pos) = self.announce_pos {
            frame.set_cursor_position(pos);
        }
    }
}
