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
    pub scroll: usize,
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
            scroll: 0,
            commit_feedback: None,
            commit_feedback_is_error: false,
        }
    }

    pub fn set_status(&mut self, status: GitStatus, entries: Vec<ChangeEntry>) {
        self.status = status;
        self.entries = entries;
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
    let label_w = label.chars().count() as u16;
    if label_w > area.width {
        return;
    }
    let label_x = area.x + (area.width - label_w) / 2;
    let label_y = area.y + area.height / 2;
    buf.set_string(
        label_x,
        label_y,
        label,
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

        // Non-repo workspace: clear empty state below the header.
        if !self.status.in_repo {
            let dim = Style::default().fg(Color::DarkGray);
            if inner.height > 2 {
                buf.set_string(inner.x, inner.y + 2, "Not a git repository", dim);
            }
            if inner.height > 3 {
                buf.set_string(
                    inner.x,
                    inner.y + 3,
                    "Open a folder under git to commit",
                    dim,
                );
            }
            return;
        }

        // Row 2: branch row — a green branch glyph plus the branch name.
        let mut y = inner.y + 2;
        if y >= inner.y + inner.height {
            return;
        }
        let mut spans: Vec<Span> = Vec::with_capacity(5);
        // Codicon `cod-source-control` (U+EB14) — the same Y-fork glyph
        // VS Code shows in the activity bar AND on the branch row, in
        // cyan. The previous green tint and the U+EA84 cod-github swap
        // were both wrong for this slot.
        spans.push(Span::styled(
            "\u{eb14} ",
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

        let row_bg_style = Style::default().bg(Color::Rgb(0x16, 0x1b, 0x25));
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
                    // Subtle row background so each entry reads as a chip.
                    for rx in 0..row_rect.width {
                        buf[(row_rect.x + rx, row_rect.y)]
                            .set_symbol(" ")
                            .set_style(row_bg_style);
                    }
                    // Entry icon: pick the file/folder icon by name +
                    // extension; folders carry a trailing '/' in
                    // git-porcelain output.
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
                    // Right-align the status badge inside the row, leaving
                    // a one-cell gap from the row's right edge.
                    let badge_x = row_rect
                        .x
                        .saturating_add(row_rect.width.saturating_sub(badge_w + row_padding));
                    if row_rect.width > badge_w + row_padding + 4 {
                        buf.set_string(
                            badge_x,
                            row_y,
                            badge_str.as_str(),
                            Style::default()
                                .fg(badge_color(entry.kind))
                                .bg(Color::Rgb(0x16, 0x1b, 0x25))
                                .add_modifier(Modifier::BOLD),
                        );
                    }
                    // Icon on the left.
                    let icon_x = row_rect.x + 1;
                    buf.set_string(
                        icon_x,
                        row_y,
                        icon.glyph.to_string(),
                        Style::default().fg(icon.color).bg(Color::Rgb(0x16, 0x1b, 0x25)),
                    );
                    // Path text between the icon and the badge column.
                    let text_x = icon_x + 2;
                    let text_w = badge_x.saturating_sub(text_x).saturating_sub(1);
                    if text_w > 0 {
                        let path_para = Paragraph::new(path_str).style(
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::Rgb(0x16, 0x1b, 0x25)),
                        );
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
        // The branch glyph is the cod-source-control Y-fork (U+EB14) —
        // SAME glyph as the activity-bar slot, in cyan. NOT cod-github
        // (U+EA84): that's the GitHub octocat and the user explicitly
        // pointed out the wrong glyph in the previous build.
        let mut hit: Option<u16> = None;
        for x in inner.x..inner.x + inner.width {
            if buf[(x, row_y)].symbol() == "\u{eb14}" {
                hit = Some(x);
                break;
            }
        }
        let x = hit.expect("branch row must carry the cod-source-control (U+EB14) Y-fork glyph");
        let style = buf[(x, row_y)].style();
        let expected = ratatui::style::Color::Rgb(0x88, 0xc0, 0xd0);
        assert_eq!(
            style.fg,
            Some(expected),
            "branch glyph must render in cyan, matching the VS Code mockup"
        );
        // Defensive: the GitHub octocat must NOT appear here (regression
        // guard against the previous wrong choice).
        for x in inner.x..inner.x + inner.width {
            assert_ne!(
                buf[(x, row_y)].symbol(),
                "\u{ea84}",
                "branch row must not carry the cod-github octocat (U+EA84) — that was the wrong glyph"
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
    fn render_in_non_repo_hides_input_and_button() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default(); // in_repo = false
        let area = Rect { x: 0, y: 0, width: 40, height: 10 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(p.last_input_area, Rect::default(), "input area must stay empty in non-repo");
        assert_eq!(p.last_button_area, Rect::default(), "button area must stay empty in non-repo");
        assert_eq!(p.last_list_area, Rect::default(), "list area must stay empty in non-repo");
        // The empty-state message lives below the SOURCE CONTROL header
        // (row 0 of inner = row 1 of buffer), at inner.y + 2 = buffer
        // row 3.
        let mut row = String::new();
        for x in 0..area.width {
            row.push_str(buf[(x, 3)].symbol());
        }
        assert!(row.contains("Not a git repository"), "row was: {row:?}");
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
