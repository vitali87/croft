//! The Explorer's AGENT LANE section (#345): per coding agent, the files it
//! changed since you last reviewed them. The ledger lives in
//! [`crate::agent_lane`]; the app turns it into [`LaneRow`]s with
//! [`AgentLanePanel::set_rows`], and a click on a file row opens its diff
//! against the reviewed snapshot. An unreviewed file carries a dot; a reviewed
//! one stays listed, dimmed, until the agent touches it again.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::theme::Theme;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_FILE: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_AGENT: Color = Color::Rgb(0x8f, 0xd9, 0xcf);
const COLOR_DOT: Color = Color::Rgb(0xe5, 0xc0, 0x7b);

/// Left indent matching the tree's `Borders::ALL` inset.
const CONTENT_INDENT: u16 = 1;

/// One row of the section: an agent, or a file under the agent above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneRow {
    Agent {
        name: String,
        unreviewed: usize,
    },
    File {
        agent: String,
        path: PathBuf,
        /// What the row shows: the path relative to its workspace root.
        label: String,
        unreviewed: bool,
    },
}

pub struct AgentLanePanel {
    pub collapsed: bool,
    rows: Vec<LaneRow>,
    scroll: usize,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    last_header_row: u16,
    last_header_x: u16,
    last_header_w: u16,
    first_row_y: u16,
    visible_rows: u16,
}

impl AgentLanePanel {
    pub fn new() -> Self {
        Self {
            collapsed: true,
            rows: Vec::new(),
            scroll: 0,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_header_row: 0,
            last_header_x: 0,
            last_header_w: 0,
            first_row_y: 0,
            visible_rows: 0,
        }
    }

    /// Replace the rows (the ledger changed), keeping the scroll in range.
    pub fn set_rows(&mut self, rows: Vec<LaneRow>) {
        self.rows = rows;
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    pub fn rows(&self) -> &[LaneRow] {
        &self.rows
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.rows.len().saturating_sub(1));
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Header plus bottom separator when collapsed; one row per item (or a
    /// single empty-state row) when open, capped at half the shared region.
    pub fn desired_height(&self, available: u16) -> u16 {
        const BORDER: u16 = 1;
        if available == 0 {
            return 0;
        }
        let floor = 1 + BORDER;
        if self.collapsed {
            return floor.min(available);
        }
        let content = (self.rows.len() as u16).max(1);
        let half = (available / 2).max(floor);
        (1 + content + BORDER).min(half)
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }

    /// The row a click at `y` lands on.
    pub fn row_at(&self, y: u16) -> Option<&LaneRow> {
        if self.collapsed || self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = (y - self.first_row_y) as usize;
        if offset >= self.visible_rows as usize {
            return None;
        }
        self.rows.get(self.scroll + offset)
    }
}

impl Default for AgentLanePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut AgentLanePanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(self.theme.ui(COLOR_DIM)));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
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
        let total: usize = self
            .rows
            .iter()
            .map(|r| match r {
                LaneRow::Agent { unreviewed, .. } => *unreviewed,
                LaneRow::File { .. } => 0,
            })
            .sum();
        let mut header = vec![
            Span::styled(
                format!("{chevron} "),
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            ),
            Span::styled(
                "AGENT LANE",
                Style::default()
                    .fg(self.theme.ui(COLOR_HEADER))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if total > 0 {
            header.push(Span::styled(
                format!("  {total}"),
                Style::default().fg(self.theme.ui(COLOR_DOT)),
            ));
        }
        Paragraph::new(Line::from(header)).render(
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
        self.last_header_row = inner.y;
        self.last_header_x = inner.x;
        self.last_header_w = inner.width;
        if self.collapsed || inner.height < 2 {
            return;
        }
        let body_y = inner.y + 1;
        let body_h = inner.height - 1;
        self.first_row_y = body_y;
        if self.rows.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No agent has changed a file",
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
        self.scroll = self
            .scroll
            .min(self.rows.len().saturating_sub(body_h as usize));
        let shown = (body_h as usize).min(self.rows.len() - self.scroll);
        self.visible_rows = shown as u16;
        for i in 0..shown {
            let y = body_y + i as u16;
            let rect = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            };
            if let Some(bg) =
                crate::widgets::hover::row_hover_bg(rect, self.hover_pointer, self.theme)
            {
                buf.set_style(rect, Style::default().bg(bg));
            }
            let line = match &self.rows[self.scroll + i] {
                LaneRow::Agent { name, unreviewed } => {
                    let mut spans = vec![Span::styled(
                        format!("\u{25c6} {name}"),
                        Style::default()
                            .fg(self.theme.ui(COLOR_AGENT))
                            .add_modifier(Modifier::BOLD),
                    )];
                    if *unreviewed > 0 {
                        spans.push(Span::styled(
                            format!("  {unreviewed} to review"),
                            Style::default().fg(self.theme.ui(COLOR_DIM)),
                        ));
                    }
                    Line::from(spans)
                }
                LaneRow::File {
                    label, unreviewed, ..
                } => {
                    let (dot, fg) = if *unreviewed {
                        ("\u{25cf} ", COLOR_FILE)
                    } else {
                        ("  ", COLOR_DIM)
                    };
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(dot, Style::default().fg(self.theme.ui(COLOR_DOT))),
                        Span::styled(label.clone(), Style::default().fg(self.theme.ui(fg))),
                    ])
                }
            };
            Paragraph::new(line).render(rect, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<LaneRow> {
        vec![
            LaneRow::Agent {
                name: "claude".into(),
                unreviewed: 1,
            },
            LaneRow::File {
                agent: "claude".into(),
                path: PathBuf::from("/w/src/a.rs"),
                label: "src/a.rs".into(),
                unreviewed: true,
            },
            LaneRow::File {
                agent: "claude".into(),
                path: PathBuf::from("/w/src/b.rs"),
                label: "src/b.rs".into(),
                unreviewed: false,
            },
        ]
    }

    fn dump(buf: &Buffer) -> String {
        let a = buf.area;
        (a.y..a.y + a.height)
            .map(|y| {
                (a.x..a.x + a.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn open_section_lists_agents_and_files_and_maps_clicks_to_rows() {
        let mut p = AgentLanePanel::new();
        p.collapsed = false;
        p.set_rows(rows());
        let area = Rect::new(0, 0, 40, 8);
        let mut buf = Buffer::empty(area);
        Widget::render(&mut p, area, &mut buf);
        let text = dump(&buf);
        assert!(text.contains("AGENT LANE  1"), "{text}");
        assert!(text.contains("claude  1 to review"), "{text}");
        assert!(
            text.contains("\u{25cf} src/a.rs"),
            "the unreviewed file is marked: {text}"
        );
        assert!(
            text.contains("src/b.rs") && !text.contains("\u{25cf} src/b.rs"),
            "{text}"
        );
        assert!(p.hit_header(3, 0));
        assert!(matches!(p.row_at(2), Some(LaneRow::File { label, .. }) if label == "src/a.rs"));
        assert!(matches!(p.row_at(1), Some(LaneRow::Agent { .. })));
        assert_eq!(p.row_at(6), None, "below the rows");
    }

    #[test]
    fn a_collapsed_section_is_one_row_and_takes_no_row_clicks() {
        let mut p = AgentLanePanel::new();
        p.set_rows(rows());
        assert_eq!(p.desired_height(40), 2);
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        Widget::render(&mut p, area, &mut buf);
        assert_eq!(p.row_at(1), None);
        p.toggle_collapse();
        assert_eq!(p.desired_height(40), 5, "header, three rows, border");
        assert_eq!(p.desired_height(6), 3, "capped at half the region");
    }
}
