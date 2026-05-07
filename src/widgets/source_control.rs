use crate::git::{ChangeEntry, ChangeKind, ChangeSection, GitStatus};
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};

const BUTTON_LABEL: &str = "  Commit  ";
const INPUT_PROMPT_RGB: (u8, u8, u8) = (0x6c, 0x7d, 0x9c);
const INPUT_BG_RGB: (u8, u8, u8) = (0x2a, 0x2f, 0x3b);
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
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                " SOURCE CONTROL ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;
        self.last_input_area = Rect::default();
        self.last_button_area = Rect::default();
        self.last_list_area = Rect::default();
        self.last_scrollbar = Rect::default();

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Non-repo workspace: nothing to commit, no branch to show. Render
        // a clear empty state and bail before painting the input/button —
        // both would imply functionality this folder doesn't support.
        if !self.status.in_repo {
            let dim = Style::default().fg(Color::DarkGray);
            buf.set_string(inner.x + 1, inner.y, "Not a git repository", dim);
            if inner.height > 1 {
                buf.set_string(
                    inner.x + 1,
                    inner.y + 1,
                    "Open a folder under git to commit",
                    dim,
                );
            }
            return;
        }

        let header_style = Style::default()
            .fg(Color::Rgb(SECTION_HEADER_RGB.0, SECTION_HEADER_RGB.1, SECTION_HEADER_RGB.2))
            .add_modifier(Modifier::BOLD);
        // Branch / dirty summary row.
        let mut spans: Vec<Span> = Vec::with_capacity(4);
        spans.push(Span::raw(" "));
        spans.push(Span::styled("\u{eb14} ", Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))));
        let label = match (&self.status.branch, &self.status.detached_hash) {
            (Some(b), _) => b.clone(),
            (None, Some(h)) => h.clone(),
            (None, None) => "(no head)".to_string(),
        };
        spans.push(Span::styled(label, header_style));
        if self.status.ahead > 0 {
            spans.push(Span::styled(
                format!(" \u{2191}{}", self.status.ahead),
                Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c)),
            ));
        }
        if self.status.behind > 0 {
            spans.push(Span::styled(
                format!(" \u{2193}{}", self.status.behind),
                Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
            ));
        }
        buf.set_line(inner.x, inner.y, &Line::from(spans), inner.width);

        // Message input (single-line).
        let input_y = inner.y + 1;
        if input_y >= inner.y + inner.height {
            return;
        }
        let input_area = Rect { x: inner.x, y: input_y, width: inner.width, height: 1 };
        self.last_input_area = input_area;
        let input_bg = Style::default().bg(Color::Rgb(INPUT_BG_RGB.0, INPUT_BG_RGB.1, INPUT_BG_RGB.2));
        buf.set_style(input_area, input_bg);
        if self.message.is_empty() {
            buf.set_string(
                input_area.x + 1,
                input_y,
                "Message (\u{2318}Enter to commit)",
                Style::default()
                    .fg(Color::Rgb(INPUT_PROMPT_RGB.0, INPUT_PROMPT_RGB.1, INPUT_PROMPT_RGB.2))
                    .bg(Color::Rgb(INPUT_BG_RGB.0, INPUT_BG_RGB.1, INPUT_BG_RGB.2)),
            );
        } else {
            buf.set_string(
                input_area.x + 1,
                input_y,
                self.message.as_str(),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(INPUT_BG_RGB.0, INPUT_BG_RGB.1, INPUT_BG_RGB.2)),
            );
        }

        // Commit button.
        let button_y = input_y + 1;
        if button_y >= inner.y + inner.height {
            return;
        }
        let button_w = (BUTTON_LABEL.chars().count() as u16).min(inner.width);
        let button_x = inner.x + (inner.width - button_w) / 2;
        let button_area = Rect { x: button_x, y: button_y, width: button_w, height: 1 };
        self.last_button_area = button_area;
        buf.set_style(
            button_area,
            Style::default()
                .bg(Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)),
        );
        buf.set_string(
            button_area.x,
            button_area.y,
            BUTTON_LABEL,
            Style::default()
                .fg(Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2))
                .bg(Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2))
                .add_modifier(Modifier::BOLD),
        );

        // Optional feedback line below the button.
        let mut next_y = button_y + 1;
        if let Some(msg) = self.commit_feedback.as_ref() {
            if next_y < inner.y + inner.height {
                let style = if self.commit_feedback_is_error {
                    Style::default().fg(Color::Rgb(0xe7, 0x70, 0x70))
                } else {
                    Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))
                };
                buf.set_string(inner.x, next_y, msg.as_str(), style);
                next_y += 1;
            }
        }

        // List of changes.
        if next_y >= inner.y + inner.height {
            return;
        }
        let list_area = Rect {
            x: inner.x,
            y: next_y,
            width: inner.width,
            height: inner.y + inner.height - next_y,
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
                list_area.x + 1,
                list_area.y,
                "No changes",
                Style::default().fg(Color::DarkGray),
            );
            return;
        }

        let end = (self.scroll + viewport).min(total);
        for (row, idx) in (self.scroll..end).enumerate() {
            let y = list_area.y + row as u16;
            match &lines[idx] {
                ListLine::Header(section) => {
                    let count = self
                        .entries
                        .iter()
                        .filter(|e| e.kind.section() == *section)
                        .count();
                    let line = Line::from(vec![
                        Span::styled("\u{25be} ", Style::default().fg(Color::Gray)),
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
                            format!("{count}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    buf.set_line(list_area.x, y, &line, row_width);
                }
                ListLine::Entry(entry_idx) => {
                    let entry = &self.entries[*entry_idx];
                    let badge = entry.kind.badge();
                    let line = Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            entry.path.as_str(),
                            Style::default().fg(Color::White),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            badge.to_string(),
                            Style::default()
                                .fg(badge_color(entry.kind))
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]);
                    buf.set_line(list_area.x, y, &line, row_width);
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

    #[test]
    fn render_in_non_repo_hides_input_and_button() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus::default(); // in_repo = false
        let area = Rect { x: 0, y: 0, width: 32, height: 10 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(p.last_input_area, Rect::default(), "input area must stay empty in non-repo");
        assert_eq!(p.last_button_area, Rect::default(), "button area must stay empty in non-repo");
        assert_eq!(p.last_list_area, Rect::default(), "list area must stay empty in non-repo");
        let mut row0 = String::new();
        for x in 0..area.width {
            row0.push_str(buf[(x, 1)].symbol());
        }
        assert!(row0.contains("Not a git repository"), "row was: {row0:?}");
    }

    #[test]
    fn render_in_repo_paints_input_and_button() {
        use crate::git::GitStatus;
        let mut p = SourceControlPanel::new();
        p.status = GitStatus { in_repo: true, branch: Some("main".into()), ..Default::default() };
        let area = Rect { x: 0, y: 0, width: 32, height: 10 };
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
