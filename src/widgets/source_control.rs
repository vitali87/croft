use crate::git::{ChangeEntry, ChangeKind, ChangeSection, GitStatus};
use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap},
};
use std::collections::BTreeSet;

const INPUT_PROMPT_RGB: (u8, u8, u8) = (0x6c, 0x7d, 0x9c);
const BUTTON_BG_RGB: (u8, u8, u8) = (0x09, 0x67, 0xb8);
const BUTTON_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);
const SECTION_HEADER_RGB: (u8, u8, u8) = (0xcc, 0xcc, 0xcc);
const ADDITIONS_RGB: (u8, u8, u8) = (0xa3, 0xbe, 0x8c);
const DELETIONS_RGB: (u8, u8, u8) = (0xe7, 0x70, 0x70);

pub const DISCARD_GLYPH: char = '\u{21b6}';
pub const STAGE_GLYPH: char = '+';
const ROW_ACTIONS_WIDTH: u16 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowActionAreas {
    pub entry_idx: usize,
    pub discard: Rect,
    pub stage: Rect,
}

/// Items in the chevron-opened dropdown next to the Commit button.
/// Ordered top-to-bottom in render order; the click handler in `App`
/// dispatches on the variant returned by `click_commit_menu_item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitMenuItem {
    CommitAndPush,
    Push,
    ViewStagedDiff,
    ViewDefaultBranchDiff,
}

impl CommitMenuItem {
    pub const ALL: [CommitMenuItem; 4] = [
        CommitMenuItem::CommitAndPush,
        CommitMenuItem::Push,
        CommitMenuItem::ViewStagedDiff,
        CommitMenuItem::ViewDefaultBranchDiff,
    ];

    /// Display label rendered in the dropdown. `default_branch` is the
    /// repo's default branch, inlined into the "View Changes vs <name>"
    /// label so the user sees exactly which branch is being compared.
    pub fn label(&self, default_branch: &str) -> String {
        match self {
            CommitMenuItem::CommitAndPush => "Commit & Push".to_string(),
            CommitMenuItem::Push => "Push".to_string(),
            CommitMenuItem::ViewStagedDiff => "View Staged Changes".to_string(),
            CommitMenuItem::ViewDefaultBranchDiff => {
                format!("View Changes vs {default_branch}")
            }
        }
    }
}

pub struct SourceControlPanel {
    pub focused: bool,
    pub message: String,
    pub message_cursor: usize,
    /// Leading characters scrolled out of view on the left of the
    /// commit-message input so the caret stays visible when the message is
    /// wider than the box. Recomputed every render from the cursor and the
    /// box width.
    pub message_scroll: usize,
    pub status: GitStatus,
    pub entries: Vec<ChangeEntry>,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_input_area: Rect,
    pub last_button_area: Rect,
    /// Hit-test rect for the chevron split-button that opens the Commit
    /// dropdown (Commit & Push, Push, View …). Empty when the panel is in
    /// the no-repo state or the button row was clipped.
    pub last_commit_caret_area: Rect,
    /// Hit-test rects for the dropdown items (Commit & Push, Push, …)
    /// that pop below the caret. Empty when the menu is closed. The App
    /// owns the open/closed flag (`commit_menu_open`) so menu state
    /// survives re-renders.
    pub commit_menu_item_areas: Vec<(Rect, CommitMenuItem)>,
    pub last_list_area: Rect,
    pub last_scrollbar: Rect,
    /// Hit-test rect for the empty-state "Initialize Repository" button.
    /// Empty when the panel is in a git repo or when the panel is too
    /// small to draw the empty-state card.
    pub last_init_repo_button_area: Rect,
    /// Cell rect reserved for the no-repo empty-state hero illustration.
    /// `App` paints an OSC-1337 inline PNG here on iTerm2-class terminals;
    /// the widget paints an ASCII Y-fork as fallback so the same rect
    /// reads as a logo on plain terminals too. Empty when the panel is
    /// in a git repo or the empty-state card is too small to allocate.
    pub last_hero_area: Rect,
    /// Set by `App` after probing the host terminal for OSC-1337 inline
    /// image support. When true, the empty-state render leaves the hero
    /// rect blank so the PNG owns those cells without competing with an
    /// ASCII fallback that would briefly flash through during resize.
    pub inline_hero_image_active: bool,
    pub scroll: usize,
    /// Index into `entries` of the currently-selected change, if any.
    /// Drives the row-highlight bg so the user can tell at a glance
    /// which entry the editor is showing the diff for.
    pub selected_change: Option<usize>,
    /// Indices of all rows currently selected (multi-select). Always
    /// includes `selected_change` when that is `Some`. Cmd+A populates this
    /// with every entry; a plain click resets it to the single clicked row.
    /// The Stage shortcut (Cmd+S) acts on this set; click on an action icon
    /// still acts on just that row.
    pub multi_selection: BTreeSet<usize>,
    /// Status / error line painted below the button after a commit attempt.
    /// Cleared on the next refresh.
    pub commit_feedback: Option<String>,
    pub commit_feedback_is_error: bool,
    /// Hit-test rects for the inline action icons (Discard, Stage) painted
    /// on the selected unstaged row. `None` when no row is selected or the
    /// selected row is already staged. Cleared at the top of each render
    /// and re-populated for the visible selected row.
    pub last_row_actions: Option<RowActionAreas>,
}

impl SourceControlPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            message: String::new(),
            message_cursor: 0,
            message_scroll: 0,
            status: GitStatus::default(),
            entries: Vec::new(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_input_area: Rect::default(),
            last_button_area: Rect::default(),
            last_commit_caret_area: Rect::default(),
            commit_menu_item_areas: Vec::new(),
            last_list_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_init_repo_button_area: Rect::default(),
            last_hero_area: Rect::default(),
            inline_hero_image_active: false,
            scroll: 0,
            selected_change: None,
            multi_selection: BTreeSet::new(),
            commit_feedback: None,
            commit_feedback_is_error: false,
            last_row_actions: None,
        }
    }

    /// Returns the entry index when (x, y) lands inside the Discard icon
    /// of the currently-selected row. The widget only paints this icon on
    /// the selected unstaged row, so a hit implies that row is the action
    /// target.
    pub fn click_discard_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.discard, x, y).then_some(a.entry_idx)
    }

    /// Returns the entry index when (x, y) lands inside the Stage icon of
    /// the currently-selected row. Mirrors `click_discard_action`.
    pub fn click_stage_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.stage, x, y).then_some(a.entry_idx)
    }

    pub fn set_status(&mut self, status: GitStatus, entries: Vec<ChangeEntry>) {
        self.status = status;
        self.entries = entries;
        // A change set refresh can re-order entries; clear stale selection
        // state rather than risk pointing it at the wrong file.
        if let Some(idx) = self.selected_change {
            if idx >= self.entries.len() {
                self.selected_change = None;
            }
        }
        self.multi_selection.retain(|i| *i < self.entries.len());
    }

    /// Click-to-select: returns the entry index now selected, if the
    /// click landed on an entry row. Resets the multi-selection to just
    /// this row — Cmd+A is the way to opt into multi.
    pub fn select_change_at(&mut self, y: u16) -> Option<usize> {
        let idx = self.entry_at_y(y)?;
        self.selected_change = Some(idx);
        self.multi_selection.clear();
        self.multi_selection.insert(idx);
        Some(idx)
    }

    /// Cmd+A in the SC pane: every entry row is now in `multi_selection`,
    /// so the Stage shortcut acts on all of them at once. Leaves
    /// `selected_change` alone so the diff editor keeps showing the same
    /// file. Returns the new selection count.
    pub fn select_all_changes(&mut self) -> usize {
        self.multi_selection = (0..self.entries.len()).collect();
        if self.selected_change.is_none() && !self.entries.is_empty() {
            self.selected_change = Some(0);
        }
        self.multi_selection.len()
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
        let next_byte = iter
            .next()
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.message.len());
        self.message.replace_range(prev_byte..next_byte, "");
        self.message_cursor = target;
    }

    pub fn delete_char(&mut self) {
        if self.message_cursor >= self.message.chars().count() {
            return;
        }
        let at = self.byte_index_at_cursor();
        let next = self.message[at..]
            .char_indices()
            .nth(1)
            .map(|(b, _)| at + b)
            .unwrap_or_else(|| self.message.len());
        self.message.replace_range(at..next, "");
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
        self.message_scroll = 0;
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
        rect_hit(self.last_button_area, x, y)
    }

    pub fn click_commit_caret(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_commit_caret_area, x, y)
    }

    pub fn click_commit_menu_item(&self, x: u16, y: u16) -> Option<CommitMenuItem> {
        self.commit_menu_item_areas
            .iter()
            .find(|(rect, _)| rect_hit(*rect, x, y))
            .map(|(_, item)| *item)
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

    /// Absolute (col, row) of the commit-message caret in screen
    /// coordinates, matching the pattern `Editor::cursor_screen_pos` uses
    /// to drive the host terminal's hardware caret. Returns `None` when
    /// the panel isn't focused, isn't in a repo (no input box painted),
    /// hasn't been laid out yet, or the cursor would land outside the
    /// visible inner width of the input box. The render path paints the
    /// message at `input_inner.x + 1` on `input_inner.y`, so the caret
    /// sits at column `input_box.x + 2 + message_cursor`, row
    /// `input_box.y + 1` (rounded border occupies one cell on each side).
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if !self.focused {
            return None;
        }
        let r = self.last_input_area;
        if r.width < 3 || r.height < 3 {
            return None;
        }
        let cursor_col = self.message_cursor.saturating_sub(self.message_scroll) as u16;
        let max_col = r.width.saturating_sub(3);
        if cursor_col > max_col {
            return None;
        }
        Some((r.x + 2 + cursor_col, r.y + 1))
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
        let text_white = Color::Rgb(0xff, 0xff, 0xff);
        let text_dim = Color::Rgb(0x9d, 0xa5, 0xb4);
        let blue_bg = Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2);

        let mut y = card_inner.y + 2;
        let bottom = card_inner.y + card_inner.height;

        // Illustration: reserve a centred block for the OSC-1337 hero
        // image (App paints the PNG over these cells on iTerm2-class
        // terminals). Paint the ASCII Y-fork as a fallback inside the
        // same rect so non-iTerm2 terminals still see a logo.
        let hero_h: u16 = if card_inner.height >= 18 {
            8
        } else if card_inner.height >= 14 {
            7
        } else {
            5
        };
        let hero_w: u16 = (hero_h * 2).min(card_inner.width).max(7);
        let hero_x = card_inner.x + (card_inner.width.saturating_sub(hero_w)) / 2;
        if y + hero_h <= bottom {
            self.last_hero_area = Rect {
                x: hero_x,
                y,
                width: hero_w,
                height: hero_h,
            };
            // Only paint the ASCII Y-fork when there's NO inline-image
            // hero owning these cells. With OSC-1337 active, leave the
            // rect blank: ratatui's text would briefly flash through the
            // image during a resize, which the user (correctly) called
            // out as old-logo glimpses peeking through the new one.
            if !self.inline_hero_image_active {
                if card_inner.width >= 13 && hero_h >= 7 {
                    paint_y_fork_illustration(buf, card_inner, y, dim_blue, blue);
                } else {
                    paint_y_fork_compact(buf, card_inner, y, blue);
                }
            }
            y += hero_h + 1;
        }

        // Title and description go through ratatui's `Paragraph` with
        // word wrapping so a resized panel reflows the text instead of
        // truncating mid-word - that's the "text inside is not even
        // adjusted" bug from the user's screenshot.
        let title_text = "No repository detected";
        let title_h = paragraph_line_count(title_text, card_inner.width).min(3) as u16;
        if y + title_h <= bottom && title_h > 0 {
            let title_rect = Rect {
                x: card_inner.x,
                y,
                width: card_inner.width,
                height: title_h,
            };
            let title = Paragraph::new(title_text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(text_white).add_modifier(Modifier::BOLD));
            title.render(title_rect, buf);
            y += title_h + 1;
        }

        let desc_text =
            "Open a folder under Git or create a new repository to start tracking changes.";
        let desc_h = paragraph_line_count(desc_text, card_inner.width).min(6) as u16;
        if y + desc_h <= bottom && desc_h > 0 {
            let desc_rect = Rect {
                x: card_inner.x,
                y,
                width: card_inner.width,
                height: desc_h,
            };
            let desc = Paragraph::new(desc_text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(text_dim));
            desc.render(desc_rect, buf);
            y += desc_h + 2;
        }

        // Primary button, centered, width capped so it doesn't sprawl on
        // a wide sidebar.
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
            let init_area = Rect {
                x: btn_x,
                y,
                width: btn_w,
                height: 3,
            };
            self.last_init_repo_button_area = init_area;
            render_rounded_button(buf, init_area, init_label, blue_bg, text_white);
        }
    }
}

/// How many rows ratatui needs to render `text` as a centered, wrapped
/// `Paragraph` at `width`. Uses the same word-wrap rules as ratatui itself
/// (whitespace-separated, no mid-word breaks unless a word exceeds the
/// width). Returns at least 1 unless `width == 0`.
fn paragraph_line_count(text: &str, width: u16) -> usize {
    if width == 0 || text.is_empty() {
        return 0;
    }
    let max = width as usize;
    let mut lines: usize = 1;
    let mut current: usize = 0;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if current == 0 {
            current = wlen.min(max);
            if wlen > max {
                lines += wlen / max;
                current = wlen % max;
            }
        } else if current + 1 + wlen <= max {
            current += 1 + wlen;
        } else {
            lines += 1;
            current = wlen.min(max);
            if wlen > max {
                lines += wlen / max;
                current = wlen % max;
            }
        }
    }
    lines
}

/// 7-row "elegant" illustration: a Git Y-fork (top node, trunk vertical,
/// branch row with side node, trunk vertical, bottom node) framed by a
/// dashed ring of dots. The classic source-control silhouette - no file
/// outline competing for attention.
fn paint_y_fork_illustration(
    buf: &mut Buffer,
    card_inner: Rect,
    top_y: u16,
    dim_blue: Color,
    blue: Color,
) {
    let dot = Style::default().fg(dim_blue);
    let trunk = Style::default().fg(blue);
    let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
    let cx = card_inner.x + card_inner.width / 2;

    // Top + bottom dashed arcs framing the tree.
    let arc = "· · · · ·";
    let arc_w = arc.chars().count() as u16;
    let arc_x = card_inner.x + (card_inner.width - arc_w) / 2;
    buf.set_string(arc_x, top_y, arc, dot);
    buf.set_string(arc_x, top_y + 6, arc, dot);

    // Side dots flanking the tree on the 5 inner rows, completing the
    // dashed-ring silhouette of the SVG mockup.
    let side_off: u16 = 5;
    if cx >= card_inner.x + side_off + 1 {
        buf.set_string(cx - side_off, top_y + 1, "·", dot);
        buf.set_string(cx - side_off, top_y + 3, "·", dot);
        buf.set_string(cx - side_off, top_y + 5, "·", dot);
    }
    if cx + side_off < card_inner.x + card_inner.width {
        buf.set_string(cx + side_off, top_y + 1, "·", dot);
        buf.set_string(cx + side_off, top_y + 3, "·", dot);
        buf.set_string(cx + side_off, top_y + 5, "·", dot);
    }

    // Y-fork tree - three nodes (●) on the trunk column, branch out on
    // the middle row to a side node, trunk verticals between rows.
    //     ●
    //     │
    //     ●─●
    //     │
    //     ●
    buf.set_string(cx, top_y + 1, "●", node);
    buf.set_string(cx, top_y + 2, "│", trunk);
    buf.set_string(cx, top_y + 3, "●", node);
    buf.set_string(cx + 1, top_y + 3, "─", trunk);
    if cx + 2 < card_inner.x + card_inner.width {
        buf.set_string(cx + 2, top_y + 3, "●", node);
    }
    buf.set_string(cx, top_y + 4, "│", trunk);
    buf.set_string(cx, top_y + 5, "●", node);
}

/// 5-row compact fallback for narrow panels: just the Y-fork tree, no
/// surrounding ring. Width: 5 cells.
fn paint_y_fork_compact(buf: &mut Buffer, card_inner: Rect, top_y: u16, blue: Color) {
    let trunk = Style::default().fg(blue);
    let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
    let cx = card_inner.x + card_inner.width / 2;
    buf.set_string(cx, top_y, "●", node);
    buf.set_string(cx, top_y + 1, "│", trunk);
    buf.set_string(cx, top_y + 2, "●", node);
    buf.set_string(cx + 1, top_y + 2, "─", trunk);
    if cx + 2 < card_inner.x + card_inner.width {
        buf.set_string(cx + 2, top_y + 2, "●", node);
    }
    buf.set_string(cx, top_y + 3, "│", trunk);
    buf.set_string(cx, top_y + 4, "●", node);
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
pub fn render_rounded_button(buf: &mut Buffer, area: Rect, label: &str, bg: Color, fg: Color) {
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
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;
        self.last_input_area = Rect::default();
        self.last_button_area = Rect::default();
        self.last_commit_caret_area = Rect::default();
        self.commit_menu_item_areas.clear();
        self.last_list_area = Rect::default();
        self.last_scrollbar = Rect::default();
        self.last_row_actions = None;

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
            self.last_hero_area = Rect::default();
            self.render_no_repo_empty_state(inner, buf);
            return;
        }
        // Repo path: hero is only for the empty state.
        self.last_hero_area = Rect::default();

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
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
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
        let input_box = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 3,
        };
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
                buf.set_stringn(
                    text_x,
                    content_y,
                    "Message (Enter = commit, \u{2303}Enter = push)",
                    text_max,
                    Style::default()
                        .fg(Color::Rgb(
                            INPUT_PROMPT_RGB.0,
                            INPUT_PROMPT_RGB.1,
                            INPUT_PROMPT_RGB.2,
                        ))
                        .add_modifier(Modifier::ITALIC),
                );
            } else {
                // Horizontal scroll: keep the caret inside the box so a
                // message wider than the input (e.g. a pasted path) stays
                // editable. Without this the box always shows byte 0, the
                // caret falls off the right edge, and edits at the tail are
                // invisible — the field reads as frozen.
                let len = self.message.chars().count();
                let cursor = self.message_cursor.min(len);
                if cursor < self.message_scroll {
                    self.message_scroll = cursor;
                } else if text_max > 0 && cursor > self.message_scroll + text_max {
                    self.message_scroll = cursor - text_max;
                }
                let visible: String = self
                    .message
                    .chars()
                    .skip(self.message_scroll)
                    .take(text_max)
                    .collect();
                buf.set_stringn(
                    text_x,
                    content_y,
                    &visible,
                    text_max,
                    Style::default().fg(Color::White),
                );
            }
        }
        y += 3 + 1; // input box + 1-row gap

        // Rows y..y+3: chunky split-button — wide "Commit" main + narrow
        // chevron caret. The caret reveals a "Commit & Push" dropdown so
        // users have the same affordance VS Code surfaces. Narrow panels
        // skip the split and render a plain Commit button so the caret
        // never collides with the label.
        if y + 3 > inner.y + inner.height {
            return;
        }
        let blue = Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2);
        let white = Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2);
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
            render_rounded_button(buf, main_area, "✓ Commit", blue, white);
            render_rounded_button(buf, caret_area, "▾", blue, white);
        } else {
            let button_area = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 3,
            };
            self.last_button_area = button_area;
            render_rounded_button(buf, button_area, "Commit", blue, white);
        }
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
                    let (count, sec_add, sec_del) = self
                        .entries
                        .iter()
                        .filter(|e| e.kind.section() == *section)
                        .fold((0usize, 0usize, 0usize), |(c, a, d), e| {
                            (c + 1, a + e.additions, d + e.deletions)
                        });
                    let mut header_spans = vec![
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
                    let row_bg = if is_primary {
                        Some(selected_bg)
                    } else if is_in_multi {
                        Some(Color::Rgb(0x1f, 0x3a, 0x5c))
                    } else {
                        None
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
                    // Reserve a 4-cell strip before the badge for inline
                    // action icons on the selected unstaged row:
                    //   [discard][gap][stage][gap-before-badge]
                    // Already-staged entries get no icons (stage would be a
                    // no-op; we deliberately don't show unstage to keep the
                    // surface to the two actions the user asked for).
                    let text_w_unreserved = badge_x.saturating_sub(text_x).saturating_sub(1);
                    let show_actions = is_selected
                        && entry.kind.section() != ChangeSection::Staged
                        && text_w_unreserved > ROW_ACTIONS_WIDTH;
                    let text_w_reserved = if show_actions { ROW_ACTIONS_WIDTH } else { 0 };
                    let text_w = text_w_unreserved.saturating_sub(text_w_reserved);
                    if text_w > 0 {
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
                        let discard_x = badge_x - 4;
                        let stage_x = badge_x - 2;
                        let action_fg = Color::Rgb(0xd4, 0xd9, 0xe2);
                        let mut action_style = Style::default().fg(action_fg);
                        if let Some(bg) = row_bg {
                            action_style = action_style.bg(bg);
                        }
                        buf.set_string(discard_x, row_y, DISCARD_GLYPH.to_string(), action_style);
                        buf.set_string(stage_x, row_y, STAGE_GLYPH.to_string(), action_style);
                        self.last_row_actions = Some(RowActionAreas {
                            entry_idx: *entry_idx,
                            discard: Rect {
                                x: discard_x,
                                y: row_y,
                                width: 1,
                                height: 1,
                            },
                            stage: Rect {
                                x: stage_x,
                                y: row_y,
                                width: 1,
                                height: 1,
                            },
                        });
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
            ChangeEntry {
                path: "u.txt".into(),
                kind: ChangeKind::Untracked,
                ..Default::default()
            },
            ChangeEntry {
                path: "m.txt".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "s.txt".into(),
                kind: ChangeKind::StagedAdded,
                ..Default::default()
            },
            ChangeEntry {
                path: "c.txt".into(),
                kind: ChangeKind::Conflicted,
                ..Default::default()
            },
        ];
        let lines = p.list_layout();
        assert!(matches!(
            lines[0],
            ListLine::Header(ChangeSection::Conflicts)
        ));
        assert!(matches!(lines[2], ListLine::Header(ChangeSection::Staged)));
        assert!(matches!(lines[4], ListLine::Header(ChangeSection::Changes)));
        assert!(matches!(
            lines[6],
            ListLine::Header(ChangeSection::Untracked)
        ));
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
    fn placeholder_text_never_overflows_past_the_input_box_right_border() {
        use ratatui::buffer::Buffer;
        // Render the SC panel in a region that is *narrower* than the
        // buffer, leaving an empty band to the right where leaks would
        // show up. The placeholder is 41 chars; the input-box inner is
        // only ~22 cells wide at a 28-col panel, so a missing width cap
        // would write 'r = push)' into the empty band — exactly the
        // 'leak past the right border into the editor pane' the user
        // reported.
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let panel_area = Rect {
            x: 0,
            y: 0,
            width: 28,
            height: 30,
        };
        let buf_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 30,
        };
        let mut buf = Buffer::empty(buf_area);
        ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
        let placeholder_row = p.last_input_area.y + 1;
        // Every cell past the panel's right edge must be blank — the
        // panel area is 0..28, so columns 28..80 are the editor band.
        for x in panel_area.right()..buf_area.right() {
            let sym = buf[(x, placeholder_row)].symbol().to_string();
            assert!(
                sym == " " || sym.is_empty(),
                "column {x} (past the SC panel's right edge at {}) carries content: {sym:?} — the placeholder string overflowed the input box and bled into the editor band, exactly the bug the user reported. Use set_stringn with a width cap.",
                panel_area.right(),
            );
        }
    }

    #[test]
    fn typed_message_text_never_overflows_past_the_input_box_right_border() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        p.message =
            "this is a very long commit message that should not overflow the input box at all"
                .to_string();
        let panel_area = Rect {
            x: 0,
            y: 0,
            width: 28,
            height: 30,
        };
        let buf_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 30,
        };
        let mut buf = Buffer::empty(buf_area);
        ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
        let row = p.last_input_area.y + 1;
        for x in panel_area.right()..buf_area.right() {
            let sym = buf[(x, row)].symbol().to_string();
            assert!(
                sym == " " || sym.is_empty(),
                "typed message overflowed past the SC panel's right edge at column {x}: {sym:?} — same root cause as the placeholder leak"
            );
        }
    }

    #[test]
    fn long_commit_message_keeps_the_caret_visible_and_scrolls_to_show_the_tail() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        p.focused = true;
        // A workspace temp path is far longer than the ~22-cell input box.
        // Typing it leaves the cursor at the end.
        let long = "/var/folders/8r/rz52gwd56wz0yw3sqc7xprgm0000gn/T/croft";
        p.insert_str(long);
        let panel_area = Rect {
            x: 0,
            y: 0,
            width: 28,
            height: 30,
        };
        let mut buf = Buffer::empty(panel_area);
        ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
        assert!(
            p.cursor_screen_pos().is_some(),
            "the caret must stay visible when the message is longer than the input box; otherwise the user can't see what they're editing and the box feels frozen"
        );
        let row = p.last_input_area.y + 1;
        let rendered: String = (p.last_input_area.x..p.last_input_area.right())
            .map(|x| buf[(x, row)].symbol().to_string())
            .collect();
        assert!(
            rendered.contains("croft"),
            "the box must scroll horizontally to reveal the tail near the cursor; rendered window was {rendered:?}"
        );
    }

    #[test]
    fn delete_key_forward_deletes_in_the_commit_message() {
        let mut p = SourceControlPanel::new();
        p.insert_str("abc");
        p.home();
        p.delete_char();
        assert_eq!((p.message.as_str(), p.message_cursor), ("bc", 0));
    }

    #[test]
    fn input_box_is_three_rows_tall_with_a_focus_aware_border() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        p.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let b = p.last_button_area;
        assert!(b.height >= 3 && b.width >= 4, "button must be laid out");
        let tl = buf[(b.x, b.y)].symbol().to_string();
        let tr = buf[(b.x + b.width - 1, b.y)].symbol().to_string();
        let bl = buf[(b.x, b.y + b.height - 1)].symbol().to_string();
        let br = buf[(b.x + b.width - 1, b.y + b.height - 1)]
            .symbol()
            .to_string();
        assert_eq!(
            tl, "╭",
            "top-left commit-button corner must be rounded; got {tl:?}"
        );
        assert_eq!(
            tr, "╮",
            "top-right commit-button corner must be rounded; got {tr:?}"
        );
        assert_eq!(
            bl, "╰",
            "bottom-left commit-button corner must be rounded; got {bl:?}"
        );
        assert_eq!(
            br, "╯",
            "bottom-right commit-button corner must be rounded; got {br:?}"
        );
    }

    #[test]
    fn change_section_header_carries_a_count_pill() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry {
                    path: "a.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "b.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "c.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
            ],
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 40,
        };
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
    fn section_header_shows_green_plus_additions_and_red_minus_deletions() {
        use ratatui::buffer::Buffer;
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry {
                    path: "a.py".into(),
                    kind: ChangeKind::Modified,
                    additions: 12,
                    deletions: 3,
                },
                ChangeEntry {
                    path: "b.py".into(),
                    kind: ChangeKind::Modified,
                    additions: 5,
                    deletions: 7,
                },
            ],
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("+17"),
            "section header must show total additions +17:\n{dump}"
        );
        assert!(
            dump.contains("-10"),
            "section header must show total deletions -10:\n{dump}"
        );

        let mut plus_cell_is_green = false;
        let mut minus_cell_is_red = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                let sym = cell.symbol();
                let Some(fg) = cell.style().fg else { continue };
                let Color::Rgb(r, g, b) = fg else { continue };
                if sym == "+" && g > r && g > b {
                    plus_cell_is_green = true;
                }
                if sym == "-" && r > g && r > b {
                    minus_cell_is_red = true;
                }
            }
        }
        assert!(
            plus_cell_is_green,
            "the '+' glyph in the section header must be painted green"
        );
        assert!(
            minus_cell_is_red,
            "the '-' glyph in the section header must be painted red"
        );
    }

    #[test]
    fn render_in_non_repo_hides_commit_input_and_button_and_list() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default(); // in_repo = false
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            p.last_input_area,
            Rect::default(),
            "input area must stay empty in non-repo"
        );
        assert_eq!(
            p.last_button_area,
            Rect::default(),
            "commit button area must stay empty in non-repo"
        );
        assert_eq!(
            p.last_list_area,
            Rect::default(),
            "list area must stay empty in non-repo"
        );
    }

    #[test]
    fn empty_state_renders_no_repository_detected_title_and_description() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
            "description must mention 'Open a folder under Git':\n{dump}"
        );
        assert!(
            dump.contains("tracking changes"),
            "description must mention 'tracking changes':\n{dump}"
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(
            p.last_init_repo_button_area.width > 0 && p.last_init_repo_button_area.height > 0,
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let btn = p.last_init_repo_button_area;
        assert!(
            btn.width > 0 && btn.height > 0,
            "init-repo button must be tracked"
        );
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
    fn empty_state_description_wraps_to_card_width_no_mid_word_truncation() {
        // Regression for "text inside is not even adjusted": the
        // description used to be hard-truncated at card_inner.width
        // chars, producing fragments like "Open a folder under G" /
        // "repository to start t" when the card was narrow. With
        // ratatui's Paragraph + Wrap, words must stay whole.
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        // Width 30 puts the card_inner at ~24 - too narrow for
        // "Open a folder under Git or create a new" but plenty for
        // word-wrapped fragments.
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            !dump.contains("Open a folder under G\n"),
            "description must NOT be hard-truncated mid-word ('under G'):\n{dump}"
        );
        assert!(
            !dump.contains("repository to start t\n"),
            "description must NOT be hard-truncated mid-word ('start t'):\n{dump}"
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
        p.last_init_repo_button_area = Rect {
            x: 5,
            y: 10,
            width: 30,
            height: 3,
        };
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
            vec![ChangeEntry {
                path: "a.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            }],
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
                ChangeEntry {
                    path: "a.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "b.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
            ],
        );
        // Render once to learn the row coordinates.
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
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
                        ChangeEntry {
                            path: "a.py".into(),
                            kind: ChangeKind::Modified,
                            ..Default::default()
                        },
                        ChangeEntry {
                            path: ".idea/".into(),
                            kind: ChangeKind::Untracked,
                            ..Default::default()
                        },
                    ],
                );
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
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
        p.status = GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            ..Default::default()
        };
        // Need at least: header + blank + branch + blank + 3-row input +
        // blank + 3-row button = 11 rows of inner area, so 13 rows of
        // outer area to clear the borders.
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 16,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(
            p.last_input_area.height > 0,
            "input area must paint when in_repo"
        );
        assert!(
            p.last_button_area.height > 0,
            "button area must paint when in_repo"
        );
    }

    #[test]
    fn cursor_screen_pos_is_none_when_not_focused() {
        let mut p = SourceControlPanel::new();
        p.status = GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            ..Default::default()
        };
        p.focused = false;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 16,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            p.cursor_screen_pos(),
            None,
            "caret must hide when the panel is not focused"
        );
    }

    #[test]
    fn cursor_screen_pos_is_none_when_no_repo() {
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default();
        p.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            p.cursor_screen_pos(),
            None,
            "caret must hide when the panel is in the no-repo empty state (no input box painted)"
        );
    }

    #[test]
    fn cursor_screen_pos_lands_at_first_text_cell_when_message_is_empty() {
        let mut p = SourceControlPanel::new();
        p.status = GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            ..Default::default()
        };
        p.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 16,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let r = p.last_input_area;
        assert!(r.width >= 3 && r.height == 3, "input must be laid out");
        let (cx, cy) = p
            .cursor_screen_pos()
            .expect("focused, in repo → caret must report a cell");
        assert_eq!(
            (cx, cy),
            (r.x + 2, r.y + 1),
            "empty message caret sits at the input's first inner text cell (one pad cell after the rounded left border)"
        );
    }

    #[test]
    fn cursor_screen_pos_advances_one_cell_per_inserted_char() {
        let mut p = SourceControlPanel::new();
        p.status = GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            ..Default::default()
        };
        p.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 16,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        p.insert_str("fix");
        let r = p.last_input_area;
        let (cx, cy) = p.cursor_screen_pos().expect("caret must report a cell");
        assert_eq!(
            (cx, cy),
            (r.x + 2 + 3, r.y + 1),
            "after typing 3 chars the caret sits 3 cells past the first text cell"
        );
    }

    #[test]
    fn cursor_screen_pos_is_none_before_first_render() {
        let mut p = SourceControlPanel::new();
        p.status = GitStatus {
            in_repo: true,
            branch: Some("main".into()),
            ..Default::default()
        };
        p.focused = true;
        assert_eq!(
            p.cursor_screen_pos(),
            None,
            "caret must hide before the first render (last_input_area is still default)"
        );
    }

    #[test]
    fn selected_unstaged_row_paints_discard_and_stage_icons_before_the_badge() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![ChangeEntry {
                path: "ch.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            }],
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        // First render learns the row coordinate; click-select it.
        let row_y = (area.y..area.y + area.height)
            .find(|y| {
                let mut row = String::new();
                for x in area.x..area.x + area.width {
                    row.push_str(buf[(x, *y)].symbol());
                }
                row.contains("ch.py")
            })
            .expect("ch.py row must render");
        p.select_change_at(row_y);
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let actions = p
            .last_row_actions
            .expect("selected unstaged row must record action hit rects");
        assert_eq!(actions.entry_idx, 0);
        assert_eq!(
            buf[(actions.discard.x, actions.discard.y)].symbol(),
            DISCARD_GLYPH.to_string().as_str(),
            "discard icon must paint at the recorded hit cell"
        );
        assert_eq!(
            buf[(actions.stage.x, actions.stage.y)].symbol(),
            STAGE_GLYPH.to_string().as_str(),
            "stage icon must paint at the recorded hit cell"
        );
        // Stage sits to the right of discard, both before the badge.
        assert!(actions.discard.x < actions.stage.x);
    }

    #[test]
    fn unselected_rows_paint_no_action_icons() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry {
                    path: "a.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "b.py".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
            ],
        );
        // No selection ⇒ no row should sprout action icons.
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        assert!(
            p.last_row_actions.is_none(),
            "no selection ⇒ no action icons"
        );
        let dump = buffer_to_string(&buf);
        assert!(
            !dump.contains(DISCARD_GLYPH),
            "discard glyph must not paint on any row when nothing is selected: {dump}"
        );
    }

    #[test]
    fn selected_staged_row_paints_no_action_icons() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![ChangeEntry {
                path: "s.py".into(),
                kind: ChangeKind::StagedModified,
                ..Default::default()
            }],
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let row_y = (area.y..area.y + area.height)
            .find(|y| {
                let mut row = String::new();
                for x in area.x..area.x + area.width {
                    row.push_str(buf[(x, *y)].symbol());
                }
                row.contains("s.py")
            })
            .expect("s.py row must render");
        p.select_change_at(row_y);
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        assert!(
            p.last_row_actions.is_none(),
            "Staged rows expose neither Discard nor Stage (no-op); the user asked for these two icons only"
        );
    }

    #[test]
    fn click_discard_action_returns_entry_idx_only_inside_the_rect() {
        let mut p = SourceControlPanel::new();
        p.last_row_actions = Some(RowActionAreas {
            entry_idx: 7,
            discard: Rect {
                x: 10,
                y: 5,
                width: 1,
                height: 1,
            },
            stage: Rect {
                x: 12,
                y: 5,
                width: 1,
                height: 1,
            },
        });
        assert_eq!(p.click_discard_action(10, 5), Some(7));
        assert_eq!(
            p.click_discard_action(12, 5),
            None,
            "stage cell is not discard"
        );
        assert_eq!(
            p.click_discard_action(11, 5),
            None,
            "gap cell is not discard"
        );
        assert_eq!(p.click_discard_action(10, 6), None, "wrong row");
    }

    #[test]
    fn click_stage_action_returns_entry_idx_only_inside_the_rect() {
        let mut p = SourceControlPanel::new();
        p.last_row_actions = Some(RowActionAreas {
            entry_idx: 3,
            discard: Rect {
                x: 10,
                y: 5,
                width: 1,
                height: 1,
            },
            stage: Rect {
                x: 12,
                y: 5,
                width: 1,
                height: 1,
            },
        });
        assert_eq!(p.click_stage_action(12, 5), Some(3));
        assert_eq!(
            p.click_stage_action(10, 5),
            None,
            "discard cell is not stage"
        );
        assert_eq!(p.click_stage_action(13, 5), None, "outside stage rect");
    }

    #[test]
    fn action_icons_are_suppressed_at_narrow_widths_so_the_path_stays_legible() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![ChangeEntry {
                path: "a.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            }],
        );
        // Width 10 leaves only a few cells for the path; reserving 4 cells
        // for icons would clobber it. The widget must skip icons rather
        // than overlap. Force selection directly since the row may sit
        // below the visible viewport at this tiny size.
        p.selected_change = Some(0);
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 60,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        assert!(
            p.last_row_actions.is_none(),
            "narrow rows must skip the action icons rather than overlap the path"
        );
    }

    #[test]
    fn select_all_changes_loads_every_entry_into_multi_selection() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry {
                    path: "a".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "b".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "c".into(),
                    kind: ChangeKind::Untracked,
                    ..Default::default()
                },
            ],
        );
        let n = p.select_all_changes();
        assert_eq!(n, 3);
        assert!(p.multi_selection.contains(&0));
        assert!(p.multi_selection.contains(&1));
        assert!(p.multi_selection.contains(&2));
    }

    #[test]
    fn clicking_a_row_resets_multi_selection_to_just_that_row() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry {
                    path: "a".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "b".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
            ],
        );
        p.select_all_changes();
        assert_eq!(p.multi_selection.len(), 2);
        // Render to populate hit areas, then "click" the second row.
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let row_y = (area.y..area.y + area.height)
            .find(|y| {
                let mut row = String::new();
                for x in area.x..area.x + area.width {
                    row.push_str(buf[(x, *y)].symbol());
                }
                row.contains("b") && !row.contains("a")
            })
            .expect("b row must render");
        p.select_change_at(row_y);
        assert_eq!(
            p.multi_selection.len(),
            1,
            "single click narrows to one row"
        );
    }

    #[test]
    fn multi_selected_rows_paint_a_visible_row_bg_even_when_not_primary() {
        let mut p = SourceControlPanel::new();
        p.set_status(
            dummy_status_with_branch("main"),
            vec![
                ChangeEntry {
                    path: "a".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "b".into(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
            ],
        );
        p.select_all_changes();
        p.selected_change = Some(0); // only row 0 is primary
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        // Both rows must carry SOME bg distinct from panel bg.
        let row_b_y = (area.y..area.y + area.height)
            .find(|y| {
                let mut row = String::new();
                for x in area.x..area.x + area.width {
                    row.push_str(buf[(x, *y)].symbol());
                }
                row.contains('b') && !row.contains('a')
            })
            .expect("b row must render");
        let mut secondary_has_bg = false;
        for x in 0..area.width {
            if let Some(bg) = buf[(x, row_b_y)].style().bg {
                if bg != Color::Reset {
                    secondary_has_bg = true;
                    break;
                }
            }
        }
        assert!(
            secondary_has_bg,
            "secondary multi-selected rows must paint a visible row bg so the Cmd+S scope is obvious"
        );
    }

    #[test]
    fn split_commit_button_records_a_separate_caret_rect_at_default_widths() {
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        assert!(
            p.last_button_area.width > 0,
            "main commit button must paint"
        );
        assert!(
            p.last_commit_caret_area.width > 0,
            "split caret must paint alongside the main button at panel widths >= 20"
        );
        // Caret sits to the RIGHT of the main button with a 1-cell gap.
        assert_eq!(
            p.last_commit_caret_area.x,
            p.last_button_area.x + p.last_button_area.width + 1
        );
        // Hit-test rejects clicks landing on the caret cell.
        let caret_mid = (
            p.last_commit_caret_area.x + p.last_commit_caret_area.width / 2,
            p.last_commit_caret_area.y + 1,
        );
        assert!(!p.click_button(caret_mid.0, caret_mid.1));
        assert!(p.click_commit_caret(caret_mid.0, caret_mid.1));
    }

    #[test]
    fn click_commit_menu_item_dispatches_on_recorded_rects() {
        let mut p = SourceControlPanel::new();
        p.commit_menu_item_areas = vec![
            (
                Rect {
                    x: 10,
                    y: 5,
                    width: 20,
                    height: 1,
                },
                CommitMenuItem::CommitAndPush,
            ),
            (
                Rect {
                    x: 10,
                    y: 6,
                    width: 20,
                    height: 1,
                },
                CommitMenuItem::Push,
            ),
            (
                Rect {
                    x: 10,
                    y: 7,
                    width: 20,
                    height: 1,
                },
                CommitMenuItem::ViewStagedDiff,
            ),
            (
                Rect {
                    x: 10,
                    y: 8,
                    width: 20,
                    height: 1,
                },
                CommitMenuItem::ViewDefaultBranchDiff,
            ),
        ];
        assert_eq!(
            p.click_commit_menu_item(15, 5),
            Some(CommitMenuItem::CommitAndPush)
        );
        assert_eq!(p.click_commit_menu_item(15, 6), Some(CommitMenuItem::Push));
        assert_eq!(
            p.click_commit_menu_item(15, 7),
            Some(CommitMenuItem::ViewStagedDiff)
        );
        assert_eq!(
            p.click_commit_menu_item(15, 8),
            Some(CommitMenuItem::ViewDefaultBranchDiff)
        );
        assert_eq!(p.click_commit_menu_item(5, 5), None, "outside the rects");
        assert_eq!(p.click_commit_menu_item(15, 9), None, "below the menu");
    }

    #[test]
    fn commit_menu_item_label_inlines_default_branch_for_view_diff_item() {
        assert_eq!(
            CommitMenuItem::ViewDefaultBranchDiff.label("trunk"),
            "View Changes vs trunk"
        );
        assert_eq!(
            CommitMenuItem::CommitAndPush.label("anything"),
            "Commit & Push"
        );
        assert_eq!(CommitMenuItem::Push.label("anything"), "Push");
        assert_eq!(
            CommitMenuItem::ViewStagedDiff.label("anything"),
            "View Staged Changes"
        );
    }

    #[test]
    fn changes_count_returns_entry_total() {
        let mut p = SourceControlPanel::new();
        assert_eq!(p.changes_count(), 0);
        p.entries.push(ChangeEntry {
            path: "x".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        });
        p.entries.push(ChangeEntry {
            path: "y".into(),
            kind: ChangeKind::Untracked,
            ..Default::default()
        });
        assert_eq!(p.changes_count(), 2);
    }
}
