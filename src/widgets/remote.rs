use crate::remote::{discover_ssh_targets, ssh_config_state, RemoteTarget, SshConfigState};
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};

pub struct RemotePanel {
    pub targets: Vec<RemoteTarget>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_list_area: Rect,
    pub last_scrollbar: Rect,
    ssh_config_state: SshConfigState,
}

impl RemotePanel {
    pub fn new() -> Self {
        let ssh_config_state = ssh_config_state();
        Self {
            targets: discover_ssh_targets(),
            selected: 0,
            scroll: 0,
            focused: false,
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_list_area: Rect::default(),
            last_scrollbar: Rect::default(),
            ssh_config_state,
        }
    }

    pub fn refresh(&mut self) {
        self.ssh_config_state = ssh_config_state();
        self.reload_targets();
    }

    pub fn refresh_if_config_changed(&mut self) -> bool {
        let state = ssh_config_state();
        if state == self.ssh_config_state {
            return false;
        }
        self.ssh_config_state = state;
        self.reload_targets();
        true
    }

    fn reload_targets(&mut self) {
        let selected_alias = self.selected_target().map(|t| t.alias.clone());
        self.targets = discover_ssh_targets();
        self.selected = selected_alias
            .and_then(|alias| self.targets.iter().position(|t| t.alias == alias))
            .unwrap_or(0)
            .min(self.targets.len().saturating_sub(1));
        self.scroll_to(self.scroll);
    }

    pub fn selected_target(&self) -> Option<&RemoteTarget> {
        self.targets.get(self.selected)
    }

    pub fn target_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_list_area.y || y >= self.last_list_area.y + self.last_list_area.height {
            return None;
        }
        let row = (y - self.last_list_area.y) as usize;
        let idx = self.scroll + row;
        (idx < self.targets.len()).then_some(idx)
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.targets.len() {
            self.selected = idx;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.targets.len() {
            self.selected += 1;
        }
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll_to(self.scroll.saturating_sub(rows));
    }

    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll_to(self.scroll.saturating_add(rows));
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.targets.len(),
            self.last_list_area.height as usize,
            self.scroll,
        ) else {
            return false;
        };
        self.scroll_to(scrollbar::scroll_for_y(metrics, y));
        true
    }

    fn scroll_to(&mut self, top: usize) {
        let viewport = self.last_list_area.height as usize;
        if viewport == 0 || self.targets.is_empty() {
            self.scroll = 0;
            self.selected = 0;
            return;
        }
        self.scroll = top.min(self.targets.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.targets.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.selected = self.scroll;
        } else if self.selected > last_visible {
            self.selected = last_visible;
        }
    }
}

impl Default for RemotePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut RemotePanel {
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
                " REMOTE EXPLORER ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;
        self.last_scrollbar = Rect::default();
        self.last_list_area = Rect {
            x: inner.x,
            y: inner.y.saturating_add(1),
            width: inner.width,
            height: inner.height.saturating_sub(1),
        };

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let header_style = Style::default()
            .fg(Color::Rgb(0xcc, 0xcc, 0xcc))
            .add_modifier(Modifier::BOLD);
        buf.set_line(
            inner.x,
            inner.y,
            &Line::from(vec![
                Span::styled("▾ ", Style::default().fg(Color::Gray)),
                Span::styled("SSH", header_style),
            ]),
            inner.width,
        );

        if self.targets.is_empty() {
            let dim = Style::default().fg(Color::DarkGray);
            if self.last_list_area.height > 0 {
                buf.set_string(
                    self.last_list_area.x,
                    self.last_list_area.y,
                    "No SSH hosts found",
                    dim,
                );
            }
            if self.last_list_area.height > 1 {
                buf.set_string(
                    self.last_list_area.x,
                    self.last_list_area.y + 1,
                    "Add Host entries to ~/.ssh/config",
                    dim,
                );
            }
            return;
        }

        let viewport = self.last_list_area.height as usize;
        if viewport == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + viewport {
            self.scroll = self.selected + 1 - viewport;
        }

        let scrollbar_area = Rect {
            x: self.last_list_area.x + self.last_list_area.width.saturating_sub(1),
            y: self.last_list_area.y,
            width: u16::from(self.last_list_area.width > 0),
            height: self.last_list_area.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, self.targets.len(), viewport, self.scroll);
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = self
            .last_list_area
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        let end = (self.scroll + viewport).min(self.targets.len());
        for (row, idx) in (self.scroll..end).enumerate() {
            let target = &self.targets[idx];
            let y = self.last_list_area.y + row as u16;
            let selected = idx == self.selected;
            let style = if selected {
                Style::default().bg(Color::Rgb(0x09, 0x4d, 0x77))
            } else {
                Style::default()
            };
            buf.set_style(
                Rect {
                    x: self.last_list_area.x,
                    y,
                    width: row_width,
                    height: 1,
                },
                style,
            );
            let detail = target.detail();
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    "\u{f108} ",
                    Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4)),
                ),
                Span::styled(
                    target.alias.as_str(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            if !detail.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(detail, Style::default().fg(Color::DarkGray)));
            }
            buf.set_line(self.last_list_area.x, y, &Line::from(spans), row_width);
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical(buf, metrics, self.focused);
        }
    }
}
