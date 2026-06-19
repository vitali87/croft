//! The Explorer's OPEN EDITORS section: a collapsible list of the editor tabs
//! currently open, mirroring VS Code's "Open Editors" view. Unlike OUTLINE /
//! TIMELINE / RUST DEPENDENCIES this needs no background fetch — the rows are a
//! pure projection of the in-memory [`crate::widgets::editor::EditorTabs`],
//! refreshed each frame by the app. A click activates that tab; the active tab
//! is highlighted and dirty tabs carry a filled dot, exactly as in the tab bar.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_NAME: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_DIRTY: Color = Color::Rgb(0xE2, 0xC0, 0x8D);

/// Left indent matching the tree's `Borders::ALL` inset (this panel draws only
/// a bottom separator) so rows line up under the file rows above.
const CONTENT_INDENT: u16 = 1;

/// One open editor, as projected from the active tab set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenEditorItem {
    /// The tab's file path (used for the click-to-activate target).
    pub path: PathBuf,
    /// The basename shown in the row.
    pub name: String,
    /// Dirty (unsaved) tab — drawn with a filled dot, like the tab bar.
    pub dirty: bool,
    /// The currently focused tab — highlighted.
    pub active: bool,
}

pub struct OpenEditorsPanel {
    pub collapsed: bool,
    items: Vec<OpenEditorItem>,
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

impl OpenEditorsPanel {
    pub fn new() -> Self {
        Self {
            collapsed: false,
            items: Vec::new(),
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

    /// Replace the row set with the current tabs, returning whether anything
    /// actually changed (so the caller only forces a redraw when it did, never
    /// once per tick). Keeps scroll where it is so a redraw never jumps.
    pub fn set_items(&mut self, items: Vec<OpenEditorItem>) -> bool {
        if self.items == items {
            return false;
        }
        self.items = items;
        self.scroll = self.scroll.min(self.max_scroll());
        true
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        self.items.len().saturating_sub(self.viewport_rows as usize)
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.items.len(),
            self.viewport_rows as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    /// Same collapsible sizing contract as [`OutlinePanel::desired_height`]:
    /// collapsed is the header plus the bottom separator; expanded adds one row
    /// per editor (or a single empty-state row), capped at half the region.
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
        let content = if self.items.is_empty() {
            1
        } else {
            self.items.len() as u16
        };
        let half = (available / 2).max(floor);
        (header + content + BORDER).min(half)
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }

    pub fn row_at(&self, y: u16) -> Option<usize> {
        if self.collapsed || self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = (y - self.first_row_y) as usize;
        if offset >= self.visible_rows as usize {
            return None;
        }
        let idx = self.scroll + offset;
        (idx < self.items.len()).then_some(idx)
    }

    /// The path of the editor at `idx`, for activating its tab on click.
    pub fn path_at(&self, idx: usize) -> Option<PathBuf> {
        self.items.get(idx).map(|i| i.path.clone())
    }
}

impl Default for OpenEditorsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut OpenEditorsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(COLOR_DIM));
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
            Span::styled(format!("{chevron} "), Style::default().fg(COLOR_DIM)),
            Span::styled(
                "OPEN EDITORS",
                Style::default()
                    .fg(COLOR_HEADER)
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

        if self.items.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No open editors",
                Style::default().fg(COLOR_DIM),
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

        self.scroll = self
            .scroll
            .min(self.items.len().saturating_sub(body_h as usize));
        let bar = scrollbar::vertical_metrics(
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: body_y,
                width: 1,
                height: body_h,
            },
            self.items.len(),
            body_h as usize,
            self.scroll,
        );
        let content_w = inner.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (body_h as usize).min(self.items.len().saturating_sub(self.scroll));
        self.visible_rows = visible as u16;
        let brand = self.focus_gradient;
        let sel_bg = if brand {
            crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)
        } else {
            Color::Rgb(0x09, 0x4d, 0x77)
        };

        for row in 0..visible {
            let idx = self.scroll + row;
            let item = &self.items[idx];
            let y = body_y + row as u16;
            let row_rect = Rect {
                x: inner.x,
                y,
                width: content_w,
                height: 1,
            };

            // Dirty tabs lead with a filled dot in the dirty colour; clean tabs
            // pad the same column so the names stay aligned.
            let (marker, marker_color) = if item.dirty {
                ("● ", COLOR_DIRTY)
            } else {
                ("  ", COLOR_DIM)
            };
            let name_style = if item.active {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLOR_NAME)
            };
            let spans = vec![
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::styled(item.name.clone(), name_style),
            ];

            let style = if item.active {
                Style::default().bg(sel_bg)
            } else if let Some(bg) =
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, brand)
            {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            buf.set_style(row_rect, style);
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
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

    fn item(name: &str, dirty: bool, active: bool) -> OpenEditorItem {
        OpenEditorItem {
            path: PathBuf::from(format!("/repo/{name}")),
            name: name.to_string(),
            dirty,
            active,
        }
    }

    #[test]
    fn collapsed_is_header_plus_separator_expanded_grows() {
        let mut p = OpenEditorsPanel::new();
        p.set_items(vec![item("a.rs", false, true)]);
        p.collapsed = true;
        assert_eq!(p.desired_height(40), 2, "collapsed: header + separator");
        p.toggle_collapse();
        assert_eq!(p.desired_height(40), 3, "header + one editor + separator");
    }

    #[test]
    fn empty_expanded_shows_one_message_row() {
        let p = OpenEditorsPanel::new();
        assert_eq!(
            p.desired_height(40),
            3,
            "header + 'No open editors' + separator"
        );
    }

    #[test]
    fn row_at_maps_clicks_to_items() {
        let mut p = OpenEditorsPanel::new();
        p.set_items(vec![item("a.rs", false, true), item("b.rs", true, false)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(p.row_at(0), None, "header row is not an item");
        assert_eq!(p.row_at(1), Some(0));
        assert_eq!(p.row_at(2), Some(1));
        assert_eq!(p.row_at(3), None, "below the last item");
        assert_eq!(p.path_at(1), Some(PathBuf::from("/repo/b.rs")));
    }

    fn rendered_text(p: &mut OpenEditorsPanel, width: u16, height: u16) -> String {
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
    fn dirty_tab_draws_a_dot() {
        let mut p = OpenEditorsPanel::new();
        p.set_items(vec![item("dirty.rs", true, false)]);
        let text = rendered_text(&mut p, 24, 4);
        assert!(text.contains('●'), "dirty editor must show a dot: {text:?}");
        assert!(text.contains("dirty.rs"));
    }
}
