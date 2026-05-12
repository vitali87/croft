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
    /// Header-row `+` (add host) hit rect. Empty when not rendered.
    pub header_add_btn: Rect,
    /// Header-row gear (settings / refresh) hit rect.
    pub header_gear_btn: Rect,
    /// Empty-state primary `+ Add New Host` button hit rect.
    pub empty_primary_btn: Rect,
    /// Empty-state secondary `Open SSH Config` button hit rect.
    pub empty_secondary_btn: Rect,
    /// Empty-state `? Learn more about SSH` link hit rect.
    pub empty_learn_link: Rect,
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
            header_add_btn: Rect::default(),
            header_gear_btn: Rect::default(),
            empty_primary_btn: Rect::default(),
            empty_secondary_btn: Rect::default(),
            empty_learn_link: Rect::default(),
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

const CARD_BORDER: Color = Color::Rgb(0x33, 0xb0, 0xc8);
const CARD_BG: Color = Color::Rgb(0x12, 0x1a, 0x24);
const PRIMARY_BG: Color = Color::Rgb(0x16, 0x9b, 0xba);
const LINK_FG: Color = Color::Rgb(0x4e, 0xc6, 0xff);

fn render_section_header(panel: &mut RemotePanel, buf: &mut Buffer, inner: Rect) {
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
    if inner.width < 6 {
        return;
    }
    let icon_fg = Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4));
    let gear_x = inner.x + inner.width.saturating_sub(2);
    let plus_x = gear_x.saturating_sub(3);
    if plus_x > inner.x + 4 {
        buf.set_string(plus_x, inner.y, "+", icon_fg);
        panel.header_add_btn = Rect { x: plus_x, y: inner.y, width: 1, height: 1 };
    }
    if gear_x > inner.x + 4 {
        buf.set_string(gear_x, inner.y, "⚙", icon_fg);
        panel.header_gear_btn = Rect { x: gear_x, y: inner.y, width: 1, height: 1 };
    }
}

fn render_empty_state(panel: &mut RemotePanel, buf: &mut Buffer, area: Rect) {
    if area.height < 12 || area.width < 22 {
        return;
    }
    let card_width = area.width.min(34);
    let card_x = area.x + (area.width.saturating_sub(card_width)) / 2;
    let card_y = area.y;
    let card_height = area.height.min(24);
    let card_rect = Rect {
        x: card_x,
        y: card_y,
        width: card_width,
        height: card_height,
    };
    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(CARD_BORDER))
        .style(Style::default().bg(CARD_BG));
    let card_inner = card_block.inner(card_rect);
    card_block.render(card_rect, buf);

    let mut y = card_inner.y;
    let illustration = [
        "  +                    ",
        "  ╭───────╮            ",
        "  │── ── ●│            ",
        "  ╰───────╯  ╭────╮    ",
        "  ╭───────╮──┤ >_ │    ",
        "  │── ── ●│  ╰────╯    ",
        "  ╰───────╯       +    ",
    ];
    let illus_w = illustration[0].chars().count() as u16;
    let illus_x = card_inner.x + (card_inner.width.saturating_sub(illus_w)) / 2;
    let illus_fg = Style::default().fg(CARD_BORDER).bg(CARD_BG);
    for line in illustration.iter() {
        if y >= card_inner.y + card_inner.height {
            break;
        }
        buf.set_string(illus_x, y, *line, illus_fg);
        y += 1;
    }
    y = y.saturating_add(1);

    let title = "No SSH hosts yet";
    let title_w = title.chars().count() as u16;
    if y < card_inner.y + card_inner.height {
        let tx = card_inner.x + (card_inner.width.saturating_sub(title_w)) / 2;
        buf.set_string(
            tx,
            y,
            title,
            Style::default()
                .fg(Color::White)
                .bg(CARD_BG)
                .add_modifier(Modifier::BOLD),
        );
        y += 2;
    }

    let body = [
        "Add SSH host entries to securely",
        "connect to your remote servers",
        "and start exploring.",
    ];
    let body_fg = Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4)).bg(CARD_BG);
    for line in body.iter() {
        if y >= card_inner.y + card_inner.height {
            break;
        }
        let w = line.chars().count() as u16;
        let lx = card_inner.x + (card_inner.width.saturating_sub(w)) / 2;
        buf.set_string(lx, y, *line, body_fg);
        y += 1;
    }
    y = y.saturating_add(1);

    let btn_width = card_inner.width.saturating_sub(4).min(28);
    let btn_x = card_inner.x + (card_inner.width.saturating_sub(btn_width)) / 2;
    if y + 1 < card_inner.y + card_inner.height {
        let label = "  +  Add New Host";
        let pad = (btn_width as usize).saturating_sub(label.chars().count());
        let padded = format!("{label}{}", " ".repeat(pad));
        buf.set_string(
            btn_x,
            y,
            &padded,
            Style::default()
                .fg(Color::White)
                .bg(PRIMARY_BG)
                .add_modifier(Modifier::BOLD),
        );
        panel.empty_primary_btn = Rect { x: btn_x, y, width: btn_width, height: 1 };
        y += 2;
    }

    if y < card_inner.y + card_inner.height {
        let label = "   Open SSH Config";
        let pad = (btn_width as usize).saturating_sub(label.chars().count());
        let padded = format!("{label}{}", " ".repeat(pad));
        buf.set_string(
            btn_x,
            y,
            &padded,
            Style::default()
                .fg(CARD_BORDER)
                .bg(CARD_BG)
                .add_modifier(Modifier::BOLD),
        );
        for dx in 0..btn_width {
            let bx = btn_x + dx;
            if dx == 0 || dx + 1 == btn_width {
                buf[(bx, y)].set_char('│').set_style(Style::default().fg(CARD_BORDER).bg(CARD_BG));
            }
        }
        panel.empty_secondary_btn = Rect { x: btn_x, y, width: btn_width, height: 1 };
        y += 2;
    }

    if y < card_inner.y + card_inner.height {
        let link = "?  Learn more about SSH  ↗";
        let w = link.chars().count() as u16;
        let lx = card_inner.x + (card_inner.width.saturating_sub(w)) / 2;
        buf.set_string(
            lx,
            y,
            link,
            Style::default().fg(LINK_FG).bg(CARD_BG).add_modifier(Modifier::UNDERLINED),
        );
        panel.empty_learn_link = Rect { x: lx, y, width: w, height: 1 };
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
        self.header_add_btn = Rect::default();
        self.header_gear_btn = Rect::default();
        self.empty_primary_btn = Rect::default();
        self.empty_secondary_btn = Rect::default();
        self.empty_learn_link = Rect::default();

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        render_section_header(self, buf, inner);

        if self.targets.is_empty() {
            render_empty_state(self, buf, self.last_list_area);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn empty_panel() -> RemotePanel {
        let mut p = RemotePanel::new();
        p.targets.clear();
        p
    }

    #[test]
    fn empty_state_registers_hit_rects_for_every_actionable_affordance() {
        let mut p = empty_panel();
        let area = Rect { x: 0, y: 0, width: 40, height: 40 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(
            p.header_add_btn.width > 0,
            "header + button must register a hit rect so clicking it opens the ssh config"
        );
        assert!(
            p.header_gear_btn.width > 0,
            "header gear must register a hit rect"
        );
        assert!(
            p.empty_primary_btn.width > 0,
            "primary 'Add New Host' button must register a hit rect — the whole point of the empty state is that this is clickable"
        );
        assert!(
            p.empty_secondary_btn.width > 0,
            "secondary 'Open SSH Config' button must register a hit rect"
        );
        assert!(
            p.empty_learn_link.width > 0,
            "learn-more link must register a hit rect"
        );
    }

    #[test]
    fn populated_state_does_not_register_empty_state_hit_rects() {
        let mut p = RemotePanel::new();
        p.targets = vec![RemoteTarget {
            alias: "dev".into(),
            host_name: Some("example.com".into()),
            user: None,
        }];
        let area = Rect { x: 0, y: 0, width: 40, height: 40 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            p.empty_primary_btn,
            Rect::default(),
            "populated state must zero out the empty-state hit rects so a click on the host list does not accidentally fire 'Add New Host'"
        );
        assert_eq!(p.empty_secondary_btn, Rect::default());
        assert_eq!(p.empty_learn_link, Rect::default());
    }

    #[test]
    fn empty_state_renders_the_no_hosts_heading_so_the_user_knows_what_state_they_are_in() {
        let mut p = empty_panel();
        let area = Rect { x: 0, y: 0, width: 40, height: 40 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let mut joined = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                joined.push_str(buf[(x, y)].symbol());
            }
            joined.push('\n');
        }
        assert!(
            joined.contains("No SSH hosts yet"),
            "rendered buffer must contain the empty-state heading; got:\n{joined}"
        );
        assert!(
            joined.contains("Add New Host"),
            "rendered buffer must contain the primary button label"
        );
        assert!(
            joined.contains("Open SSH Config"),
            "rendered buffer must contain the secondary button label"
        );
        assert!(
            joined.contains("Learn more about SSH"),
            "rendered buffer must contain the learn-more link"
        );
    }
}
