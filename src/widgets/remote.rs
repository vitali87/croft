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
    /// Cell where the SSH-empty-state PNG should be emitted via OSC-1337,
    /// sized to (SSH_EMPTY_STATE_CELLS_W, SSH_EMPTY_STATE_CELLS_H). None
    /// when the panel isn't in empty-state or doesn't fit the image.
    pub last_image_cell: Option<(u16, u16)>,
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
            last_image_cell: None,
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

/// Teal/cyan accent color sampled from the `v0.1.122` version chip in the
/// croft mockup — the same color is used for the empty-state card border,
/// the secondary button outline, the illustration line art, and the
/// learn-more link. NOT the same as the blue used in `REMOTE EXPLORER`'s
/// title chip; the two palettes coexist by design.
const CARD_BORDER: Color = Color::Rgb(0x3e, 0xd8, 0xc4);
const PRIMARY_BG: Color = Color::Rgb(0x0e, 0x7e, 0x76);
const LINK_FG: Color = Color::Rgb(0x3e, 0xd8, 0xc4);
const BODY_FG: Color = Color::Rgb(0xb4, 0xbe, 0xc8);

/// Cell dimensions used to bake the SSH empty-state PNG via OSC-1337.
/// Public so `app.rs` can size the canvas at startup to match the cells
/// the renderer reserves.
pub const SSH_EMPTY_STATE_CELLS_W: u16 = 18;
pub const SSH_EMPTY_STATE_CELLS_H: u16 = 8;

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
    panel.last_image_cell = None;
    if area.height < 14 || area.width < 22 {
        return;
    }
    let card_width = area.width;
    let card_x = area.x;
    let card_y = area.y;
    let card_height = area.height;
    let card_rect = Rect {
        x: card_x,
        y: card_y,
        width: card_width,
        height: card_height,
    };
    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(CARD_BORDER));
    let card_inner = card_block.inner(card_rect);
    card_block.render(card_rect, buf);

    let content_x = card_inner.x;
    let content_w = card_inner.width;
    let content_end_y = card_inner.y + card_inner.height;
    let mut y = card_inner.y.saturating_add(1);

    // Reserve cells for the OSC-1337 illustration. Cells are blanked
    // with default style (no explicit bg) so the host terminal's session
    // bg shows through the illustration's transparent letterbox — the
    // user explicitly does NOT want a dark rectangle behind the PNG.
    let img_w = SSH_EMPTY_STATE_CELLS_W.min(content_w);
    let img_h = SSH_EMPTY_STATE_CELLS_H.min(content_end_y.saturating_sub(y));
    if img_w > 0 && img_h > 0 {
        let img_x = content_x + (content_w.saturating_sub(img_w)) / 2;
        for dy in 0..img_h {
            for dx in 0..img_w {
                let cell = &mut buf[(img_x + dx, y + dy)];
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }
        panel.last_image_cell = Some((img_x, y));
        y += img_h;
    }
    y = y.saturating_add(1);

    let title = "No SSH hosts yet";
    let title_w = title.chars().count() as u16;
    if y < content_end_y {
        let tx = content_x + (content_w.saturating_sub(title_w)) / 2;
        buf.set_string(
            tx,
            y,
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
        y += 2;
    }

    let body_lines = wrap_body(content_w as usize);
    let body_fg = Style::default().fg(BODY_FG);
    for line in body_lines.iter() {
        if y >= content_end_y {
            break;
        }
        let w = line.chars().count() as u16;
        let lx = content_x + (content_w.saturating_sub(w)) / 2;
        buf.set_string(lx, y, line, body_fg);
        y += 1;
    }
    y = y.saturating_add(1);

    let btn_width = content_w.saturating_sub(2).min(28);
    let btn_x = content_x + (content_w.saturating_sub(btn_width)) / 2;

    if y + 3 <= content_end_y {
        panel.empty_primary_btn =
            render_filled_button(buf, btn_x, y, btn_width, "+  Add New Host", PRIMARY_BG);
        y += 4;
    }

    if y + 3 <= content_end_y {
        panel.empty_secondary_btn =
            render_outlined_button(buf, btn_x, y, btn_width, "Open SSH Config");
        y += 4;
    }

    if y < content_end_y {
        let full_link = "?  Learn more about SSH  ↗";
        let short_link = "? Learn more ↗";
        let link = if content_w >= full_link.chars().count() as u16 {
            full_link
        } else if content_w >= short_link.chars().count() as u16 {
            short_link
        } else {
            ""
        };
        if !link.is_empty() {
            let w = link.chars().count() as u16;
            let lx = content_x + (content_w.saturating_sub(w)) / 2;
            buf.set_string(
                lx,
                y,
                link,
                Style::default().fg(LINK_FG).add_modifier(Modifier::UNDERLINED),
            );
            panel.empty_learn_link = Rect { x: lx, y, width: w, height: 1 };
        }
    }
}

const BODY_TEXT: &str =
    "Add SSH host entries to securely connect to your remote servers and start exploring.";

fn wrap_body(max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in BODY_TEXT.split_whitespace() {
        let extra = if current.is_empty() {
            word.chars().count()
        } else {
            word.chars().count() + 1
        };
        if current.chars().count() + extra > max_width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Render a 3-row filled rounded button at (x, y), width cells wide, with
/// `label` centred on the middle row. Returns the clickable rect covering
/// all three rows so the caller can hit-test the full button area.
fn render_filled_button(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    label: &str,
    fill: Color,
) -> Rect {
    let fill_style = Style::default().bg(fill);
    let label_style = Style::default()
        .fg(Color::White)
        .bg(fill)
        .add_modifier(Modifier::BOLD);
    for dx in 0..width {
        for dy in 0..3 {
            let cell = &mut buf[(x + dx, y + dy)];
            cell.set_char(' ');
            cell.set_style(fill_style);
        }
    }
    // Rounded corners painted over the fill so the button reads as a pill.
    let right = x + width.saturating_sub(1);
    buf[(x, y)].set_char('╭');
    buf[(right, y)].set_char('╮');
    buf[(x, y + 2)].set_char('╰');
    buf[(right, y + 2)].set_char('╯');
    let lbl_w = label.chars().count() as u16;
    let lbl_x = x + (width.saturating_sub(lbl_w)) / 2;
    buf.set_string(lbl_x, y + 1, label, label_style);
    Rect { x, y, width, height: 3 }
}

/// Render a 3-row outlined rounded button (transparent fill, teal border)
/// with `label` centred on the middle row. The interior cells are blanked
/// with default style so the host terminal's session bg shows through —
/// no explicit bg color is set anywhere.
fn render_outlined_button(buf: &mut Buffer, x: u16, y: u16, width: u16, label: &str) -> Rect {
    for dx in 0..width {
        for dy in 0..3 {
            let cell = &mut buf[(x + dx, y + dy)];
            cell.set_char(' ');
            cell.set_style(Style::default());
        }
    }
    let right = x + width.saturating_sub(1);
    let border_style = Style::default().fg(CARD_BORDER);
    buf[(x, y)].set_char('╭').set_style(border_style);
    for dx in 1..width.saturating_sub(1) {
        buf[(x + dx, y)].set_char('─').set_style(border_style);
    }
    buf[(right, y)].set_char('╮').set_style(border_style);
    buf[(x, y + 1)].set_char('│').set_style(border_style);
    buf[(right, y + 1)].set_char('│').set_style(border_style);
    buf[(x, y + 2)].set_char('╰').set_style(border_style);
    for dx in 1..width.saturating_sub(1) {
        buf[(x + dx, y + 2)].set_char('─').set_style(border_style);
    }
    buf[(right, y + 2)].set_char('╯').set_style(border_style);
    let label_style = Style::default()
        .fg(CARD_BORDER)
        .add_modifier(Modifier::BOLD);
    let lbl_w = label.chars().count() as u16;
    let lbl_x = x + (width.saturating_sub(lbl_w)) / 2;
    buf.set_string(lbl_x, y + 1, label, label_style);
    Rect { x, y, width, height: 3 }
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
    fn empty_state_body_text_never_overflows_card_width_at_real_sidebar_sizes() {
        for sidebar_w in 24u16..=44 {
            let mut p = empty_panel();
            let area = Rect { x: 0, y: 0, width: sidebar_w, height: 40 };
            let mut buf = Buffer::empty(area);
            (&mut p).render(area, &mut buf);
            for y in area.top()..area.bottom() {
                let mut row = String::new();
                for x in area.left()..area.right() {
                    row.push_str(buf[(x, y)].symbol());
                }
                let chars: Vec<char> = row.chars().collect();
                let cw = sidebar_w as usize;
                if chars.len() < cw {
                    continue;
                }
                let last = chars[cw - 1];
                let last_non_space = chars.iter().rposition(|c| !c.is_whitespace());
                if let Some(pos) = last_non_space {
                    let is_border = matches!(
                        last,
                        '╮' | '╯' | '│' | '─' | '┌' | '┐' | '└' | '┘'
                    );
                    assert!(
                        pos < cw - 1 || is_border,
                        "body text overflows the sidebar at width {sidebar_w}: row='{row}', last non-space '{last}' at col {pos} >= card right edge {}",
                        cw - 1
                    );
                }
            }
        }
    }

    #[test]
    fn empty_state_buttons_are_three_rows_tall_for_a_proper_click_target() {
        let mut p = empty_panel();
        let area = Rect { x: 0, y: 0, width: 40, height: 40 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            p.empty_primary_btn.height, 3,
            "primary button must be 3 rows tall so the rounded corners fit and the click target is comfortable"
        );
        assert_eq!(p.empty_secondary_btn.height, 3);
    }

    #[test]
    fn empty_state_reserves_a_cell_for_the_osc_1337_illustration() {
        let mut p = empty_panel();
        let area = Rect { x: 0, y: 0, width: 40, height: 40 };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(
            p.last_image_cell.is_some(),
            "render_empty_state must publish the cell where the SSH PNG should be emitted via OSC-1337; without this the post-draw flush has nowhere to land the image and the empty state shows a blank box"
        );
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
