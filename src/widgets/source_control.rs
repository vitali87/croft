use crate::git::{ChangeEntry, ChangeKind, ChangeSection, GitStatus};
use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget},
};

const INPUT_PROMPT_RGB: (u8, u8, u8) = (0x6c, 0x7d, 0x9c);
const BUTTON_BG_RGB: (u8, u8, u8) = (0x09, 0x67, 0xb8);
const BUTTON_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);
const SECTION_HEADER_RGB: (u8, u8, u8) = (0xcc, 0xcc, 0xcc);

pub struct SourceControlPanel {
    pub focused: bool,
    pub message: String,
    pub message_cursor: usize,
    pub status: GitStatus,
    pub entries: Vec<ChangeEntry>,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_input_area: Rect,
    pub last_button_area: Rect,
    pub last_list_area: Rect,
    pub last_scrollbar: Rect,
    /// Hit-test rect for the empty-state "Initialize Repository" button.
    /// Empty when the panel is in a git repo or when the panel is too
    /// small to draw the empty-state card.
    pub last_init_repo_button_area: Rect,
    pub scroll: usize,
    /// Index into `entries` of the currently-selected change, if any.
    /// Drives the row-highlight bg so the user can tell at a glance
    /// which entry the editor is showing the diff for.
    pub selected_change: Option<usize>,
    /// Status / error line painted below the button after a commit attempt.
    /// Cleared on the next refresh.
    pub commit_feedback: Option<String>,
    pub commit_feedback_is_error: bool,
}

impl SourceControlPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            message: String::new(),
            message_cursor: 0,
            status: GitStatus::default(),
            entries: Vec::new(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_input_area: Rect::default(),
            last_button_area: Rect::default(),
            last_list_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_init_repo_button_area: Rect::default(),
            scroll: 0,
            selected_change: None,
            commit_feedback: None,
            commit_feedback_is_error: false,
        }
    }

    pub fn set_status(&mut self, status: GitStatus, entries: Vec<ChangeEntry>) {
        self.status = status;
        self.entries = entries;
        // A change set refresh can re-order entries; clear the selection
        // rather than risk pointing it at the wrong file.
        if let Some(idx) = self.selected_change {
            if idx >= self.entries.len() {
                self.selected_change = None;
            }
        }
    }

    /// Click-to-select: returns the entry index now selected, if the
    /// click landed on an entry row.
    pub fn select_change_at(&mut self, y: u16) -> Option<usize> {
        let idx = self.entry_at_y(y)?;
        self.selected_change = Some(idx);
        Some(idx)
    }

    pub fn changes_count(&self) -> usize {
        self.entries.len()
    }

    pub fn insert_char(&mut self, c: char) {
        if c == '\n' || c == '\r' {
            return;
        }
        let idx = self.byte_index_at_cursor();
        self.message.insert(idx, c);
        self.message_cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' || c == '\r' {
                continue;
            }
            self.insert_char(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.message_cursor == 0 {
            return;
        }
        let target = self.message_cursor - 1;
        let mut iter = self.message.char_indices();
        let mut prev_byte = 0;
        for (n, (b, _)) in iter.by_ref().enumerate() {
            if n == target {
                prev_byte = b;
                break;
            }
        }
        let next_byte = iter.next().map(|(b, _)| b).unwrap_or_else(|| self.message.len());
        self.message.replace_range(prev_byte..next_byte, "");
        self.message_cursor = target;
    }

    pub fn move_cursor_left(&mut self) {
        if self.message_cursor > 0 {
            self.message_cursor -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.message_cursor < self.message.chars().count() {
            self.message_cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.message_cursor = 0;
    }

    pub fn end(&mut self) {
        self.message_cursor = self.message.chars().count();
    }

    pub fn clear_message(&mut self) {
        self.message.clear();
        self.message_cursor = 0;
    }

    pub fn entry_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_list_area.y || y >= self.last_list_area.y + self.last_list_area.height {
            return None;
        }
        let row = (y - self.last_list_area.y) as usize;
        // The list interleaves section headers with rows; tag each visual
        // line with whether it points to an entry, then look up by offset.
        let lines = self.list_layout();
        let line_idx = self.scroll + row;
        match lines.get(line_idx)? {
            ListLine::Entry(idx) => Some(*idx),
            ListLine::Header(_) => None,
        }
    }

    pub fn click_button(&self, x: u16, y: u16) -> bool {
        let rect = self.last_button_area;
        rect.width > 0
            && rect.height > 0
            && x >= rect.x
            && x < rect.x + rect.width
            && y >= rect.y
            && y < rect.y + rect.height
    }

    pub fn click_input(&self, x: u16, y: u16) -> bool {
        let rect = self.last_input_area;
        rect.width > 0
            && rect.height > 0
            && x >= rect.x
            && x < rect.x + rect.width
            && y >= rect.y
            && y < rect.y + rect.height
    }

    pub fn click_init_repo_button(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_init_repo_button_area, x, y)
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        let max = self
            .list_layout()
            .len()
            .saturating_sub(self.last_list_area.height as usize);
        self.scroll = (self.scroll + rows).min(max);
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let total = self.list_layout().len();
        let viewport = self.last_list_area.height as usize;
        let Some(metrics) =
            scrollbar::vertical_metrics(self.last_scrollbar, total, viewport, self.scroll)
        else {
            return false;
        };
        self.scroll = scrollbar::scroll_for_y(metrics, y);
        true
    }

    fn byte_index_at_cursor(&self) -> usize {
        self.message
            .char_indices()
            .nth(self.message_cursor)
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.message.len())
    }

    /// Compute the visual layout of the change list as a flat sequence of
    /// rows: section headers and entry rows. Used for hit-testing and
    /// scrollbar metrics.
    fn list_layout(&self) -> Vec<ListLine> {
        let mut out = Vec::new();
        for section in [
            ChangeSection::Conflicts,
            ChangeSection::Staged,
            ChangeSection::Changes,
            ChangeSection::Untracked,
        ] {
            let mut any = false;
            for (i, entry) in self.entries.iter().enumerate() {
                if entry.kind.section() == section {
                    if !any {
                        out.push(ListLine::Header(section));
                        any = true;
                    }
                    out.push(ListLine::Entry(i));
                }
            }
        }
        out
    }

    /// Paint the "No repository detected" empty-state card. `inner` is the
    /// panel's inner rect (border already drawn); the SOURCE CONTROL header
    /// sits on `inner.y` so the card starts two rows below.
    fn render_no_repo_empty_state(&mut self, inner: Rect, buf: &mut Buffer) {
        if inner.height < 4 || inner.width < 8 {
            return;
        }
        let card_top = inner.y + 2;
        let card_bottom = inner.y + inner.height - 1;
        let card_h = card_bottom.saturating_sub(card_top);
        if card_h < 3 {
            return;
        }
        let side_pad: u16 = if inner.width >= 24 { 2 } else { 1 };
        let card_x = inner.x + side_pad;
        let card_w = inner.width.saturating_sub(side_pad * 2);
        let card_area = Rect {
            x: card_x,
            y: card_top,
            width: card_w,
            height: card_h,
        };
        let card_border = Style::default().fg(Color::Rgb(0x60, 0x68, 0x78));
        let card_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(card_border);
        let card_inner = card_block.inner(card_area);
        card_block.render(card_area, buf);
        if card_inner.width < 4 || card_inner.height < 3 {
            return;
        }

        let blue = Color::Rgb(0x60, 0x9a, 0xfe);
        let dim_blue = Color::Rgb(0x4b, 0x50, 0x5a);
        let frame_grey = Color::Rgb(0x36, 0x3a, 0x45);
        let text_white = Color::Rgb(0xff, 0xff, 0xff);
        let text_dim = Color::Rgb(0x9d, 0xa5, 0xb4);
        let blue_bg = Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2);

        let cx = card_inner.x + card_inner.width / 2;
        let mut y = card_inner.y + 2;
        let bottom = card_inner.y + card_inner.height;

        // Illustration: file silhouette with a corner fold and a 3-node
        // git tree inside, surrounded by dashed dots and decorative `+`
        // sigils echoing the SVG mockup. Two width tiers: the rich 7-row
        // version when the card is at least 17 wide; otherwise a 3-row
        // fallback so very narrow sidebars still render something.
        if card_inner.width >= 17 && y + 7 <= bottom {
            self.paint_illustration_rich(buf, card_inner, y, frame_grey, dim_blue, blue);
            y += 7 + 2;
        } else if card_inner.width >= 7 && y + 3 <= bottom {
            self.paint_illustration_compact(buf, card_inner, y, dim_blue, blue);
            y += 3 + 2;
        }

        // Title: "No repository detected", centered, bold white.
        let title = "No repository detected";
        if y + 1 <= bottom {
            let lw = (title.chars().count() as u16).min(card_inner.width);
            let visible: String = title.chars().take(lw as usize).collect();
            let tx = card_inner.x + (card_inner.width.saturating_sub(lw)) / 2;
            buf.set_string(
                tx,
                y,
                &visible,
                Style::default()
                    .fg(text_white)
                    .add_modifier(Modifier::BOLD),
            );
            y += 2;
        }

        // Description: dim, centered, two lines (truncated per-line when
        // narrower than the source string).
        for line in [
            "Open a folder under Git or create a new",
            "repository to start tracking changes.",
        ] {
            if y >= bottom {
                break;
            }
            let lw = (line.chars().count() as u16).min(card_inner.width);
            let visible: String = line.chars().take(lw as usize).collect();
            let lx = card_inner.x + (card_inner.width.saturating_sub(lw)) / 2;
            buf.set_string(lx, y, &visible, Style::default().fg(text_dim));
            y += 1;
        }
        if y < bottom {
            y += 2;
        }

        // Single primary button, full card width minus 2-cell side gutter.
        let _ = cx;
        let max_btn_w: u16 = 40;
        let btn_w = card_inner.width.saturating_sub(2).min(max_btn_w);
        let btn_x = card_inner.x + (card_inner.width.saturating_sub(btn_w)) / 2;
        let init_label: &str = if btn_w >= 23 {
            "Initialize Repository"
        } else if btn_w >= 12 {
            "Initialize"
        } else {
            "Init"
        };
        if y + 3 <= bottom && btn_w >= 6 {
            let init_area = Rect { x: btn_x, y, width: btn_w, height: 3 };
            self.last_init_repo_button_area = init_area;
            render_rounded_button(buf, init_area, init_label, blue_bg, text_white);
        }
    }

    /// 7-row "rich" illustration: file silhouette with corner fold, a
    /// 3-node git tree inside, surrounded by dashed dots and `+` sigils.
    /// Mirrors the SVG layout one TUI cell at a time.
    fn paint_illustration_rich(
        &self,
        buf: &mut Buffer,
        card_inner: Rect,
        top_y: u16,
        frame_grey: Color,
        dim_blue: Color,
        blue: Color,
    ) {
        let dot = Style::default().fg(dim_blue);
        let frame = Style::default().fg(frame_grey);
        let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
        // The file silhouette is 9 cells wide (`╭` + 7 inner + `╮`), 5
        // rows tall. Centre it on the card. Decorations on either side.
        let file_w: u16 = 9;
        let cx = card_inner.x + card_inner.width / 2;
        let fx = cx.saturating_sub(file_w / 2);
        let fy = top_y + 1;

        // Top decorative `+` glyphs flanking the file's top edge.
        if fx >= card_inner.x + 2 {
            buf.set_string(fx.saturating_sub(2), top_y, "+", dot);
        }
        if fx + file_w + 1 < card_inner.x + card_inner.width {
            buf.set_string(fx + file_w + 1, top_y, "+", dot);
        }

        // File silhouette with corner fold at the top-left. Row layout:
        //   ╭─────╮     <- top edge with fold "╮" two cells to the right
        //   │     │
        //   │     │
        //   │     │
        //   ╰─────╯
        // We draw the fold with a small `╮` two cells in, and the top edge
        // joins back to the right at the same row.
        buf.set_string(fx, fy, "╭─\u{2533}─────╮", frame); // top with fold marker
        buf.set_string(fx, fy + 1, "│ ╰╮    │", frame);
        buf.set_string(fx, fy + 2, "│  │    │", frame);
        buf.set_string(fx, fy + 3, "│       │", frame);
        buf.set_string(fx, fy + 4, "╰───────╯", frame);

        // 3-node git tree inside the file: trunk on column fx+3 spanning
        // rows fy+1..fy+3, branch out to fx+5 on row fy+2.
        let trunk_x = fx + 3;
        let branch_x = fx + 6;
        // Trunk verticals.
        buf.set_string(trunk_x, fy + 2, "│", node);
        // Branch line: a corner glyph then a horizontal segment.
        buf.set_string(trunk_x + 1, fy + 2, "─", node);
        buf.set_string(trunk_x + 2, fy + 2, "─", node);
        // Three nodes (top of trunk, end of branch, bottom of trunk).
        buf.set_string(trunk_x, fy + 1, "●", node);
        buf.set_string(branch_x, fy + 2, "●", node);
        buf.set_string(trunk_x, fy + 3, "●", node);

        // Bottom decorative dots and a small ring on the right of the file.
        let bottom_y = fy + 5;
        if bottom_y < card_inner.y + card_inner.height {
            if fx >= card_inner.x + 2 {
                buf.set_string(fx.saturating_sub(2), bottom_y - 1, "·", dot);
            }
            if fx + file_w + 2 <= card_inner.x + card_inner.width {
                buf.set_string(fx + file_w + 1, bottom_y - 1, "·", dot);
            }
            if fx + file_w + 3 <= card_inner.x + card_inner.width {
                buf.set_string(fx + file_w + 2, fy + 2, "○", dot);
            }
            if fx >= card_inner.x + 1 {
                buf.set_string(fx.saturating_sub(1), bottom_y, "+", dot);
            }
        }
    }

    /// 3-row fallback illustration for very narrow side panels: top arc,
    /// branch glyph flanked by side dots, bottom arc. Width: 7 cells.
    fn paint_illustration_compact(
        &self,
        buf: &mut Buffer,
        card_inner: Rect,
        top_y: u16,
        dim_blue: Color,
        blue: Color,
    ) {
        let dot = Style::default().fg(dim_blue);
        let glyph = Style::default().fg(blue).add_modifier(Modifier::BOLD);
        let arc = "· · ·";
        let arc_w = arc.chars().count() as u16;
        let arc_x = card_inner.x + (card_inner.width - arc_w) / 2;
        buf.set_string(arc_x, top_y, arc, dot);
        let middle_w: u16 = 7;
        let mid_x = card_inner.x + (card_inner.width - middle_w) / 2;
        buf.set_string(mid_x, top_y + 1, "·", dot);
        buf.set_string(mid_x + 3, top_y + 1, "\u{ea68}", glyph);
        buf.set_string(mid_x + 6, top_y + 1, "·", dot);
        buf.set_string(arc_x, top_y + 2, arc, dot);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListLine {
    Header(ChangeSection),
    Entry(usize),
}

impl Default for SourceControlPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn section_label(section: ChangeSection) -> &'static str {
    match section {
        ChangeSection::Conflicts => "MERGE CONFLICTS",
        ChangeSection::Staged => "STAGED CHANGES",
        ChangeSection::Changes => "CHANGES",
        ChangeSection::Untracked => "UNTRACKED",
    }
}

/// Paint a chunky 3-row solid-bg button at `area`, with rounded corner
/// glyphs that let the panel bg show through the four corner notches so
/// the button reads as a rounded rectangle (matching the VS Code mockup).
fn rect_hit(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x + rect.width
        && y >= rect.y
        && y < rect.y + rect.height
}

/// Shared between the Source Control commit button and the Run-and-Debug
/// button — same style, same corner treatment.
pub fn render_rounded_button(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    bg: Color,
    fg: Color,
) {
    if area.width < 2 || area.height < 1 {
        return;
    }
    // Fill every cell with the solid bg. The label and the corner glyphs
    // overwrite specific cells below.
    let solid = Style::default().bg(bg);
    for ry in 0..area.height {
        for rx in 0..area.width {
            buf[(area.x + rx, area.y + ry)]
                .set_symbol(" ")
                .set_style(solid);
        }
    }
    // Replace the four corner cells with rounded-border glyphs whose bg is
    // *unset* so the panel bg shows through the outer angle of each curve;
    // the glyph's stroke is drawn in the button bg colour so the curve
    // visually continues the button outline.
    let corner_style = Style::default().fg(bg);
    if area.height >= 2 {
        buf[(area.x, area.y)]
            .set_symbol("╭")
            .set_style(corner_style);
        buf[(area.x + area.width - 1, area.y)]
            .set_symbol("╮")
            .set_style(corner_style);
        buf[(area.x, area.y + area.height - 1)]
            .set_symbol("╰")
            .set_style(corner_style);
        buf[(area.x + area.width - 1, area.y + area.height - 1)]
            .set_symbol("╯")
            .set_style(corner_style);
    }
    // Truncate the label rather than dropping it entirely when the button
    // is narrower than `label.chars().count()`. The earlier silent-drop
    // path produced a blank blue rectangle in narrow side panels — the
    // user-visible "WTF is this" bug from the no-repo empty state.
    let max_w = area.width as usize;
    let visible_label: String = label.chars().take(max_w).collect();
    if visible_label.is_empty() {
        return;
    }
    let label_w = visible_label.chars().count() as u16;
    let label_x = area.x + (area.width - label_w) / 2;
    let label_y = area.y + area.height / 2;
    buf.set_string(
        label_x,
        label_y,
        &visible_label,
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
}

fn badge_color(kind: ChangeKind) -> Color {
    match kind {
        ChangeKind::StagedAdded => Color::Rgb(0x81, 0xb8, 0x8c),
        ChangeKind::StagedModified | ChangeKind::Modified => Color::Rgb(0xeb, 0xcb, 0x8b),
        ChangeKind::StagedDeleted | ChangeKind::Deleted => Color::Rgb(0xe7, 0x70, 0x70),
        ChangeKind::StagedRenamed => Color::Rgb(0xb4, 0x8e, 0xad),
        ChangeKind::Untracked => Color::Rgb(0x88, 0xc0, 0xd0),
        ChangeKind::Conflicted => Color::Rgb(0xe7, 0x4c, 0x3c),
    }
}

impl Widget for &mut SourceControlPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let focus_blue = Color::Rgb(0x4e, 0x9a, 0xff);
        let outer_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = Block::default().borders(Borders::ALL).border_style(outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;
        self.last_input_area = Rect::default();
        self.last_button_area = Rect::default();
        self.last_list_area = Rect::default();
        self.last_scrollbar = Rect::default();

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Row 0: SOURCE CONTROL header (light grey bold), inside the panel.
        buf.set_string(
            inner.x,
            inner.y,
            "SOURCE CONTROL",
            Style::default()
                .fg(Color::Rgb(0xb0, 0xb8, 0xc8))
                .add_modifier(Modifier::BOLD),
        );

        // Non-repo workspace: render the "No repository detected" card -
        // bordered card with illustration, title, description, primary
        // (Initialize Repository) and secondary (Open Folder) buttons,
        // plus a "Learn more about Git" help link. Mirrors the reference
        // design the user supplied.
        if !self.status.in_repo {
            self.last_init_repo_button_area = Rect::default();
            self.render_no_repo_empty_state(inner, buf);
            return;
        }

        // Row 2: branch row — a green branch glyph plus the branch name.
        let mut y = inner.y + 2;
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
        spans.push(Span::styled(
            "\u{ea68} ",
            Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0)),
        ));
        let label = match (&self.status.branch, &self.status.detached_hash) {
            (Some(b), _) => b.clone(),
            (None, Some(h)) => h.clone(),
            (None, None) => "(no head)".to_string(),
        };
        spans.push(Span::styled(
            label,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
        if self.status.ahead > 0 {
            spans.push(Span::styled(
                format!("  \u{2191}{}", self.status.ahead),
                Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c)),
            ));
        }
        if self.status.behind > 0 {
            spans.push(Span::styled(
                format!("  \u{2193}{}", self.status.behind),
                Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
            ));
        }
        buf.set_line(inner.x, y, &Line::from(spans), inner.width);
        y += 2; // blank gap below branch row

        // Rows y..y+3: commit-message input (3-row rounded box).
        if y + 3 > inner.y + inner.height {
            return;
        }
        let input_box = Rect { x: inner.x, y, width: inner.width, height: 3 };
        self.last_input_area = input_box;
        let input_border_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::Rgb(0x60, 0x68, 0x78))
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(input_border_style);
        let input_inner = input_block.inner(input_box);
        input_block.render(input_box, buf);
        if input_inner.width > 0 && input_inner.height > 0 {
            let content_y = input_inner.y;
            if self.message.is_empty() {
                buf.set_string(
                    input_inner.x + 1,
                    content_y,
                    "Message (\u{2318}Enter to commit)",
                    Style::default()
                        .fg(Color::Rgb(
                            INPUT_PROMPT_RGB.0,
                            INPUT_PROMPT_RGB.1,
                            INPUT_PROMPT_RGB.2,
                        ))
                        .add_modifier(Modifier::ITALIC),
                );
            } else {
                buf.set_string(
                    input_inner.x + 1,
                    content_y,
                    self.message.as_str(),
                    Style::default().fg(Color::White),
                );
            }
        }
        y += 3 + 1; // input box + 1-row gap

        // Rows y..y+3: chunky commit button with rounded corners.
        if y + 3 > inner.y + inner.height {
            return;
        }
        let button_area = Rect { x: inner.x, y, width: inner.width, height: 3 };
        self.last_button_area = button_area;
        let blue = Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2);
        let white = Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2);
        render_rounded_button(buf, button_area, "Commit", blue, white);
        y += 3 + 1; // button + 1-row gap

        // Optional feedback line.
        if let Some(msg) = self.commit_feedback.as_ref() {
            if y < inner.y + inner.height {
                let style = if self.commit_feedback_is_error {
                    Style::default().fg(Color::Rgb(0xe7, 0x70, 0x70))
                } else {
                    Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))
                };
                buf.set_string(inner.x, y, msg.as_str(), style);
                y += 2;
            }
        }

        // Thin separator line.
        if y >= inner.y + inner.height {
            return;
        }
        let sep_style = Style::default().fg(Color::Rgb(0x40, 0x48, 0x58));
        for x in inner.x..inner.x + inner.width {
            buf.set_string(x, y, "─", sep_style);
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
            buf.set_string(
                list_area.x,
                list_area.y,
                "No changes",
                Style::default().fg(Color::DarkGray),
            );
            return;
        }

        let selected_bg = Color::Rgb(0x26, 0x4f, 0x78);
        let end = (self.scroll + viewport).min(total);
        for (row, idx) in (self.scroll..end).enumerate() {
            let row_y = list_area.y + row as u16;
            match &lines[idx] {
                ListLine::Header(section) => {
                    let count = self
                        .entries
                        .iter()
                        .filter(|e| e.kind.section() == *section)
                        .count();
                    let header_spans = vec![
                        Span::styled(
                            "\u{25be} ",
                            Style::default().fg(Color::Rgb(0xb0, 0xb8, 0xc8)),
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
                                .fg(Color::White)
                                .bg(Color::Rgb(0x2a, 0x33, 0x42))
                                .add_modifier(Modifier::BOLD),
                        ),
                    ];
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
                    let is_selected = self.selected_change == Some(*entry_idx);
                    // Selected rows carry the VS Code list-active-blue bg
                    // so the user can see which entry the editor's diff
                    // is currently bound to. Unselected rows inherit the
                    // panel bg so the list reads cleanly.
                    let row_bg = if is_selected {
                        Some(selected_bg)
                    } else {
                        None
                    };
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
                        .fg(badge_color(entry.kind))
                        .add_modifier(Modifier::BOLD);
                    let mut icon_style = Style::default().fg(icon.color);
                    let mut path_style = Style::default().fg(Color::White);
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
                    let text_w = badge_x.saturating_sub(text_x).saturating_sub(1);
                    if text_w > 0 {
                        let path_para = Paragraph::new(path_str).style(path_style);
                        path_para.render(
                            Rect { x: text_x, y: row_y, width: text_w, height: 1 },
                            buf,
                        );
                    }
                }
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_char_appends_and_moves_cursor() {
        let mut p = SourceControlPanel::new();
        p.insert_char('h');
        p.insert_char('i');
        assert_eq!(p.message, "hi");
        assert_eq!(p.message_cursor, 2);
    }

    #[test]
    fn backspace_deletes_previous_char() {
        let mut p = SourceControlPanel::new();
        p.insert_str("abc");
        p.backspace();
        assert_eq!(p.message, "ab");
        assert_eq!(p.message_cursor, 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut p = SourceControlPanel::new();
        p.backspace();
        assert_eq!(p.message, "");
        assert_eq!(p.message_cursor, 0);
    }

    #[test]
    fn list_layout_orders_conflicts_staged_changes_untracked() {
        let mut p = SourceControlPanel::new();
        p.entries = vec![
            ChangeEntry { path: "u.txt".into(), kind: ChangeKind::Untracked },
            ChangeEntry { path: "m.txt".into(), kind: ChangeKind::Modified },
            ChangeEntry { path: "s.txt".into(), kind: ChangeKind::StagedAdded },
            ChangeEntry { path: "c.txt".into(), kind: ChangeKind::Conflicted },
        ];
        let lines = p.list_layout();
        assert!(matches!(lines[0], ListLine::Header(ChangeSection::Conflicts)));
        assert!(matches!(lines[2], ListLine::Header(ChangeSection::Staged)));
        assert!(matches!(lines[4], ListLine::Header(ChangeSection::Changes)));
        assert!(matches!(lines[6], ListLine::Header(ChangeSection::Untracked)));
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in buf.area.y..buf.area.y + buf.area.height {
            for x in buf.area.x..buf.area.x + buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn dummy_status_with_branch(name: &str) -> GitStatus {
        GitStatus {
            in_repo: true,
            branch: Some(name.to_string()),
            detached_hash: None,
            ahead: 0,
            behind: 0,
            dirty: false,
        }
    }

    #[test]
    fn header_row_says_source_control_inside_the_panel_not_on_the_outer_border() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let dump = buffer_to_string(&buf);
        // Header lives on the first inner row in light-grey-bold.
        let inner_top = dump.lines().nth(1).expect("inner row 0");
        assert!(
            inner_top.contains("SOURCE CONTROL"),
            "first inner row must carry SOURCE CONTROL: {inner_top:?}"
        );
        // Outer border row carries no chip / title.
        let outer = dump.lines().next().expect("outer top border");
        assert!(
            !outer.contains("SOURCE CONTROL"),
            "outer border must not carry the title chip: {outer:?}"
        );
    }

    #[test]
    fn input_box_is_three_rows_tall_with_a_focus_aware_border() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        p.focused = true;
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        assert_eq!(
            p.last_input_area.height, 3,
            "commit-message input must be 3 rows tall to match the chunky aesthetic"
        );
        assert!(p.last_input_area.width >= 20);
    }

    #[test]
    fn commit_button_is_chunky_three_row_full_width_block() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        assert_eq!(
            p.last_button_area.height, 3,
            "Commit button must be 3 rows tall (chunky) like the mockup"
        );
        // Button must be wider than the old single-glyph label — close to
        // the inner width.
        assert!(
            p.last_button_area.width >= 20,
            "Commit button must be near-full-width; got {}",
            p.last_button_area.width
        );
    }

    #[test]
    fn branch_row_uses_the_source_control_codicon_in_cyan() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let area = Rect { x: 0, y: 0, width: 60, height: 20 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let inner = p.last_inner;
        let row_y = inner.y + 2;
        // The branch glyph is cod-source-control (U+EA68) in cyan.
        // Verified against the upstream codicon mapping.json and the
        // Nerd Fonts CSS on 2026-05-09. Two prior mis-pins (U+EB14 =
        // link-external, U+EA84 = github) put the wrong shape on the
        // user's branch row; this test guards against either coming
        // back.
        let mut hit: Option<u16> = None;
        for x in inner.x..inner.x + inner.width {
            if buf[(x, row_y)].symbol() == "\u{ea68}" {
                hit = Some(x);
                break;
            }
        }
        let x = hit.expect("branch row must carry the cod-source-control (U+EA68) Y-fork glyph");
        let style = buf[(x, row_y)].style();
        let expected = ratatui::style::Color::Rgb(0x88, 0xc0, 0xd0);
        assert_eq!(
            style.fg,
            Some(expected),
            "branch glyph must render in cyan, matching the VS Code mockup"
        );
        // Regression guards against the two wrong codepoints that
        // shipped before:
        //   U+EB14 → cod-link-external (the share-arrow the user spotted)
        //   U+EA84 → cod-github (the octocat the user spotted before that)
        for x in inner.x..inner.x + inner.width {
            let s = buf[(x, row_y)].symbol();
            assert_ne!(
                s, "\u{eb14}",
                "branch row must not carry cod-link-external (U+EB14)"
            );
            assert_ne!(
                s, "\u{ea84}",
                "branch row must not carry cod-github (U+EA84)"
            );
        }
    }

    #[test]
    fn commit_button_uses_rounded_border_corners() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let area = Rect { x: 0, y: 0, width: 40, height: 20 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let b = p.last_button_area;
        assert!(b.height >= 3 && b.width >= 4, "button must be laid out");
        let tl = buf[(b.x, b.y)].symbol().to_string();
        let tr = buf[(b.x + b.width - 1, b.y)].symbol().to_string();
        let bl = buf[(b.x, b.y + b.height - 1)].symbol().to_string();
        let br = buf[(b.x + b.width - 1, b.y + b.height - 1)].symbol().to_string();
        assert_eq!(tl, "╭", "top-left commit-button corner must be rounded; got {tl:?}");
        assert_eq!(tr, "╮", "top-right commit-button corner must be rounded; got {tr:?}");
        assert_eq!(bl, "╰", "bottom-left commit-button corner must be rounded; got {bl:?}");
        assert_eq!(br, "╯", "bottom-right commit-button corner must be rounded; got {br:?}");
    }

    #[test]
    fn change_section_header_carries_a_count_pill() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry { path: "a.py".into(), kind: ChangeKind::Modified },
                ChangeEntry { path: "b.py".into(), kind: ChangeKind::Modified },
                ChangeEntry { path: "c.py".into(), kind: ChangeKind::Modified },
            ],
        );
        let area = Rect { x: 0, y: 0, width: 60, height: 40 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("CHANGES"),
            "section header must say CHANGES:\n{dump}"
        );
        assert!(
            dump.contains('3'),
            "section header must include a count badge of 3:\n{dump}"
        );
        assert!(
            dump.contains('▾') || dump.contains('▿'),
            "section header must carry a down chevron:\n{dump}"
        );
    }

    #[test]
    fn render_in_non_repo_hides_commit_input_and_button_and_list() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default(); // in_repo = false
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(p.last_input_area, Rect::default(), "input area must stay empty in non-repo");
        assert_eq!(p.last_button_area, Rect::default(), "commit button area must stay empty in non-repo");
        assert_eq!(p.last_list_area, Rect::default(), "list area must stay empty in non-repo");
    }

    #[test]
    fn empty_state_renders_no_repository_detected_title_and_description() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("No repository detected"),
            "empty-state title missing:\n{dump}"
        );
        assert!(
            dump.contains("Initialize Repository"),
            "primary button label missing:\n{dump}"
        );
        assert!(
            dump.contains("Open a folder under Git"),
            "first description line missing:\n{dump}"
        );
        assert!(
            dump.contains("repository to start tracking"),
            "second description line missing:\n{dump}"
        );
        assert!(
            !dump.contains("Open Folder"),
            "Open Folder button must be gone:\n{dump}"
        );
        assert!(
            !dump.contains("Learn more about Git"),
            "Learn more link must be gone:\n{dump}"
        );
    }

    #[test]
    fn empty_state_records_button_rect_for_hit_testing() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(
            p.last_init_repo_button_area.width > 0
                && p.last_init_repo_button_area.height > 0,
            "Initialize Repository button rect must be tracked"
        );
    }

    #[test]
    fn empty_state_init_button_paints_a_visible_label_at_narrow_widths() {
        // Regression for "WTF is this": a narrow side panel produced a
        // blank blue rectangle because render_rounded_button used to
        // silently drop the label when label.chars().count() > area.width.
        // The button must always carry SOME visible label.
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        let area = Rect { x: 0, y: 0, width: 30, height: 30 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let btn = p.last_init_repo_button_area;
        assert!(btn.width > 0 && btn.height > 0, "init-repo button must be tracked");
        let label_y = btn.y + btn.height / 2;
        let mut row = String::new();
        for x in btn.x..btn.x + btn.width {
            row.push_str(buf[(x, label_y)].symbol());
        }
        let trimmed = row.trim();
        assert!(
            !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphabetic()),
            "init-repo button row must carry a visible label, got {row:?}"
        );
    }

    #[test]
    fn empty_state_illustration_does_not_use_arrow_up_codicon() {
        // Regression: the earlier illustration used U+EAA1 (cod-arrow-up)
        // which rendered a literal up-arrow glyph. The new rich
        // illustration uses ● bullets for git nodes; the compact fallback
        // uses U+EA68 (cod-source-control). Whichever variant renders,
        // U+EAA1 must NEVER appear.
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            !dump.contains('\u{eaa1}'),
            "empty-state must NOT carry U+EAA1 (cod-arrow-up); that was the broken glyph"
        );
        // The rich illustration must surface git nodes (●).
        assert!(
            dump.contains('\u{25cf}'),
            "rich illustration must paint ● git nodes"
        );
    }

    #[test]
    fn click_init_repo_button_returns_true_only_inside_the_rect() {
        let mut p = SourceControlPanel::new();
        p.last_init_repo_button_area = Rect { x: 5, y: 10, width: 30, height: 3 };
        assert!(p.click_init_repo_button(20, 11));
        assert!(!p.click_init_repo_button(40, 11));
        assert!(!p.click_init_repo_button(20, 20));
    }


    #[test]
    fn change_rows_use_a_subtle_row_tint_not_darker_than_panel_bg() {
        // User report: the rows were darker than the panel bg, the
        // opposite of what the mockup shows. Drop the heavy dark tint —
        // either no row bg at all, or a tint LIGHTER than the editor bg
        // (Color::Reset / 0x1e222e). Rgb(0x16,0x1b,0x25) is darker and
        // must not appear on entry rows.
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![ChangeEntry { path: "a.py".into(), kind: ChangeKind::Modified }],
        );
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let bad = ratatui::style::Color::Rgb(0x16, 0x1b, 0x25);
        for y in 0..area.height {
            for x in 0..area.width {
                assert_ne!(
                    buf[(x, y)].style().bg,
                    Some(bad),
                    "no cell may carry the darker-than-panel row bg ({bad:?}) — that's the colour the user complained about; cell at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn clicking_a_change_entry_highlights_it_with_a_visible_row_bg() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry { path: "a.py".into(), kind: ChangeKind::Modified },
                ChangeEntry { path: "b.py".into(), kind: ChangeKind::Modified },
            ],
        );
        // Render once to learn the row coordinates.
        let area = Rect { x: 0, y: 0, width: 60, height: 30 };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        // Pretend the user clicked the second change row.
        let target_y = (0..area.height)
            .find(|y| {
                let mut row = String::new();
                for x in 0..area.width {
                    row.push_str(buf[(x, *y)].symbol());
                }
                row.contains("b.py")
            })
            .expect("b.py row must render");
        p.select_change_at(target_y);
        // Re-render with the new selection.
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        // The selected row must carry a non-Reset bg distinct from the
        // unselected rows so the user can tell which entry is active.
        let mut selected_has_bg = false;
        for x in 0..area.width {
            if let Some(bg) = buf[(x, target_y)].style().bg {
                if bg != ratatui::style::Color::Reset {
                    selected_has_bg = true;
                    break;
                }
            }
        }
        assert!(
            selected_has_bg,
            "the row of the clicked entry must carry a visible bg highlight"
        );
    }

    #[test]
    fn rendering_at_narrow_widths_does_not_panic() {
        use ratatui::buffer::Buffer;
        for width in 0u16..40 {
            for height in 0u16..30 {
                let mut p = SourceControlPanel::new();
                p.set_status(
                    dummy_status_with_branch("main"),
                    vec![
                        ChangeEntry { path: "a.py".into(), kind: ChangeKind::Modified },
                        ChangeEntry { path: ".idea/".into(), kind: ChangeKind::Untracked },
                    ],
                );
                let area = Rect { x: 0, y: 0, width, height };
                if area.width == 0 || area.height == 0 {
                    continue;
                }
                let mut buf = Buffer::empty(area);
                ratatui::widgets::Widget::render(&mut p, area, &mut buf);
            }
        }
    }

    #[test]
    fn render_in_repo_paints_input_and_button() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus { in_repo: true, branch: Some("main".into()), ..Default::default() };
        // Need at least: header + blank + branch + blank + 3-row input +
        // blank + 3-row button = 11 rows of inner area, so 13 rows of
        // outer area to clear the borders.
        let area = Rect { x: 0, y: 0, width: 40, height: 16 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(p.last_input_area.height > 0, "input area must paint when in_repo");
        assert!(p.last_button_area.height > 0, "button area must paint when in_repo");
    }

    #[test]
    fn changes_count_returns_entry_total() {
        let mut p = SourceControlPanel::new();
        assert_eq!(p.changes_count(), 0);
        p.entries.push(ChangeEntry { path: "x".into(), kind: ChangeKind::Modified });
        p.entries.push(ChangeEntry { path: "y".into(), kind: ChangeKind::Untracked });
        assert_eq!(p.changes_count(), 2);
    }
}
