//! The Explorer's TIMELINE section: a collapsible list of the commits that
//! touched the active file, mirroring VS Code's Timeline view (the git-history
//! source). The rows are fetched off-thread by the app via
//! [`crate::git::file_history`] and handed here with [`set_history`]; clicking a
//! row opens that commit's diff for the file. Each row shows the commit summary
//! with the author and humanized age beneath, matching VS Code's two-line item.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::git::FileHistoryEntry;
use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_SUMMARY: Color = Color::Rgb(0xCC, 0xCC, 0xCC);

/// Left indent matching the tree's `Borders::ALL` inset so rows line up under
/// the file rows above (this panel draws only a bottom separator).
const CONTENT_INDENT: u16 = 1;

pub struct TimelinePanel {
    pub collapsed: bool,
    /// The file the current history describes; `None` until the first fetch.
    pub path: Option<PathBuf>,
    entries: Vec<FileHistoryEntry>,
    /// `true` once a fetch (even an empty one) has landed for `path`, so the
    /// panel can show "No history" instead of spinning on "Loading".
    loaded: bool,
    scroll: usize,
    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,
    last_header_row: u16,
    last_header_x: u16,
    last_header_w: u16,
    first_row_y: u16,
    visible_rows: u16,
    viewport_rows: u16,
}

impl TimelinePanel {
    pub fn new() -> Self {
        Self {
            // Start collapsed (a 1-row chevron strip), like OUTLINE.
            collapsed: true,
            path: None,
            entries: Vec::new(),
            loaded: false,
            scroll: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_header_row: 0,
            last_header_x: 0,
            last_header_w: 0,
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// Apply a fetched history for `path`. Resets scroll when the file changed
    /// so the view starts at the newest commit.
    pub fn set_history(&mut self, path: PathBuf, entries: Vec<FileHistoryEntry>) {
        let same_file = self.path.as_ref() == Some(&path);
        self.path = Some(path);
        self.entries = entries;
        self.loaded = true;
        if !same_file {
            self.scroll = 0;
        }
    }

    /// Mark the panel as loading history for `path` (the active file just
    /// changed). Shows "Loading" until the background fetch lands. No-op when
    /// already tracking `path`, so it never resets a fetched view.
    pub fn begin_loading(&mut self, path: PathBuf) {
        if self.path.as_ref() != Some(&path) {
            self.path = Some(path);
            self.entries.clear();
            self.loaded = false;
            self.scroll = 0;
        }
    }

    /// Forget the history (the editor returned to the welcome screen).
    pub fn clear(&mut self) {
        self.path = None;
        self.entries.clear();
        self.loaded = false;
        self.scroll = 0;
    }

    /// The file the panel currently tracks, for the app's fetch bookkeeping.
    pub fn tracked_path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        // One commit occupies two rows (summary + author/age), so the viewport
        // holds `viewport_rows / 2` commits.
        self.entries
            .len()
            .saturating_sub((self.viewport_rows / 2) as usize)
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.entries.len(),
            (self.viewport_rows / 2) as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    /// Collapsed is the header + bottom separator; expanded adds two rows per
    /// commit (summary + author/age) or a single empty/loading row, capped at
    /// half the shared region like the other Explorer sub-views.
    pub fn desired_height(&self, available: u16) -> u16 {
        const BORDER: u16 = 1;
        if available == 0 {
            return 0;
        }
        let header = 1u16;
        let floor = header + BORDER;
        if self.collapsed {
            return floor.min(available);
        }
        let content = if self.entries.is_empty() {
            1
        } else {
            (self.entries.len() as u16).saturating_mul(2)
        };
        let half = (available / 2).max(floor);
        (header + content + BORDER).min(half)
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }

    /// Map a click row to the commit index it lands on. Either row of a
    /// commit's two-row item resolves to that commit.
    pub fn row_at(&self, y: u16) -> Option<usize> {
        if self.collapsed || self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = (y - self.first_row_y) as usize;
        if offset >= self.visible_rows as usize {
            return None;
        }
        let idx = self.scroll + offset / 2;
        (idx < self.entries.len()).then_some(idx)
    }

    /// The `(file, commit hash)` for the entry at `idx`, for opening its diff.
    pub fn diff_target(&self, idx: usize) -> Option<(PathBuf, String)> {
        let entry = self.entries.get(idx)?;
        let path = self.path.clone()?;
        Some((path, entry.short_hash.clone()))
    }
}

impl Default for TimelinePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut TimelinePanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(self.theme.ui(COLOR_DIM)));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.visible_rows = 0;
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let inner = Rect {
            x: inner.x + CONTENT_INDENT.min(inner.width),
            width: inner.width.saturating_sub(CONTENT_INDENT),
            ..inner
        };

        let chevron = if self.collapsed {
            crate::icons::CHEVRON_CLOSED
        } else {
            crate::icons::CHEVRON_OPEN
        };
        let header_y = inner.y;
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{chevron} "),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ),
            Span::styled(
                "TIMELINE",
                Style::default()
                    .fg(self.theme.ui(COLOR_HEADER))
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(
            Rect {
                x: inner.x,
                y: header_y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
        self.last_header_row = header_y;
        self.last_header_x = inner.x;
        self.last_header_w = inner.width;

        if self.collapsed || inner.height < 2 {
            return;
        }

        let body_y = header_y + 1;
        let body_h = inner.height - 1;
        self.first_row_y = body_y;
        self.viewport_rows = body_h;

        if self.entries.is_empty() {
            let msg = if self.path.is_some() && !self.loaded {
                "Loading"
            } else if self.path.is_some() {
                "No history"
            } else {
                "No file open"
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            )))
            .render(
                Rect {
                    x: inner.x,
                    y: body_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
            );
            return;
        }

        // Each commit is two rows, so the viewport holds `body_h / 2` commits.
        let commits_per_view = (body_h / 2) as usize;
        self.scroll = self
            .scroll
            .min(self.entries.len().saturating_sub(commits_per_view.max(1)));
        let bar = scrollbar::vertical_metrics(
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: body_y,
                width: 1,
                height: body_h,
            },
            self.entries.len(),
            commits_per_view,
            self.scroll,
        );
        let content_w = inner.width.saturating_sub(u16::from(bar.is_some()));

        let visible_commits = commits_per_view.min(self.entries.len().saturating_sub(self.scroll));
        self.visible_rows = (visible_commits as u16).saturating_mul(2);

        for row in 0..visible_commits {
            let idx = self.scroll + row;
            let entry = &self.entries[idx];
            let summary_y = body_y + (row as u16) * 2;
            let meta_y = summary_y + 1;

            // Summary line.
            let summary_rect = Rect {
                x: inner.x,
                y: summary_y,
                width: content_w,
                height: 1,
            };
            if let Some(bg) =
                crate::widgets::hover::row_hover_bg(summary_rect, self.hover_pointer, self.theme)
            {
                buf.set_style(summary_rect, Style::default().bg(bg));
            }
            Paragraph::new(Line::from(Span::styled(
                entry.summary.clone(),
                Style::default().fg(self.theme.ui(COLOR_SUMMARY)),
            )))
            .render(summary_rect, buf);

            // Author · age, dimmed, indented under the summary.
            if meta_y < body_y + body_h {
                let meta = format!(
                    "  {} · {}",
                    entry.author,
                    crate::git::humanize_age(entry.age_secs)
                );
                Paragraph::new(Line::from(Span::styled(
                    meta,
                    Style::default().fg(self.theme.ui(COLOR_DIM)),
                )))
                .render(
                    Rect {
                        x: inner.x,
                        y: meta_y,
                        width: content_w,
                        height: 1,
                    },
                    buf,
                );
            }
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, summary: &str) -> FileHistoryEntry {
        FileHistoryEntry {
            short_hash: hash.to_string(),
            summary: summary.to_string(),
            author: "vitali87".to_string(),
            age_secs: 7200,
        }
    }

    #[test]
    fn collapsed_is_header_plus_separator_expanded_grows_by_two_rows_per_commit() {
        let mut p = TimelinePanel::new();
        p.set_history("a.rs".into(), vec![entry("abc123", "fix: thing")]);
        p.collapsed = true;
        assert_eq!(p.desired_height(40), 2, "collapsed: header + separator");
        p.toggle_collapse();
        assert_eq!(
            p.desired_height(40),
            4,
            "header + two rows (summary + author/age) + separator"
        );
    }

    #[test]
    fn loading_then_no_history_messages() {
        let mut p = TimelinePanel::new();
        p.collapsed = false;
        // path set but no fetch landed yet → Loading.
        p.begin_loading("a.rs".into());
        let text = rendered_text(&mut p, 30, 6);
        assert!(text.contains("Loading"), "got:\n{text}");
        // empty fetch landed → No history.
        p.set_history("a.rs".into(), vec![]);
        let text = rendered_text(&mut p, 30, 6);
        assert!(text.contains("No history"), "got:\n{text}");
    }

    #[test]
    fn row_at_resolves_both_rows_of_a_commit_to_one_index() {
        let mut p = TimelinePanel::new();
        p.collapsed = false;
        p.set_history(
            "a.rs".into(),
            vec![entry("aaa", "first"), entry("bbb", "second")],
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 8,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        // header at y=0; commit 0 at rows 1-2, commit 1 at rows 3-4.
        assert_eq!(p.row_at(0), None, "header is not a commit");
        assert_eq!(p.row_at(1), Some(0));
        assert_eq!(
            p.row_at(2),
            Some(0),
            "author/age row maps to the same commit"
        );
        assert_eq!(p.row_at(3), Some(1));
        assert_eq!(p.diff_target(1), Some(("a.rs".into(), "bbb".to_string())));
    }

    fn rendered_text(p: &mut TimelinePanel, width: u16, height: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_summary_author_and_age() {
        let mut p = TimelinePanel::new();
        p.collapsed = false;
        p.set_history("a.rs".into(), vec![entry("abc", "feat: add views menu")]);
        let text = rendered_text(&mut p, 40, 6);
        assert!(text.contains("feat: add views menu"), "got:\n{text}");
        assert!(text.contains("vitali87"), "got:\n{text}");
        assert!(text.contains("hours ago"), "got:\n{text}");
    }
}
