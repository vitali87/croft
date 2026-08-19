use crate::remote::{RemoteTarget, SshConfigState, discover_ssh_targets, ssh_config_state};
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
    /// True under the Black theme: overpaint the focused outer border with the
    /// orange→green brand gradient instead of the legacy solid blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    pub collapsed: bool,
    pub filter: String,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_list_area: Rect,
    pub last_scrollbar: Rect,
    pub header_chevron_btn: Rect,
    pub header_add_btn: Rect,
    pub header_refresh_btn: Rect,
    pub empty_primary_btn: Rect,
    pub empty_secondary_btn: Rect,
    pub empty_learn_link: Rect,
    pub last_image_cell: Option<(u16, u16)>,
    /// Full cell rect the SSH empty-state illustration occupied on the most
    /// recent render. Lets the post-draw flush detect when a chrome tooltip
    /// lands on the image (whose OSC-1337 layer would hide the tooltip text).
    pub last_image_area: Rect,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// hovered host row and the header action pills (+ / refresh) can lift.
    pub hover_pointer: Option<(u16, u16)>,
    visible: Vec<usize>,
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
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            collapsed: false,
            filter: String::new(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_list_area: Rect::default(),
            last_scrollbar: Rect::default(),
            header_chevron_btn: Rect::default(),
            header_add_btn: Rect::default(),
            header_refresh_btn: Rect::default(),
            empty_primary_btn: Rect::default(),
            empty_secondary_btn: Rect::default(),
            empty_learn_link: Rect::default(),
            last_image_cell: None,
            last_image_area: Rect::default(),
            hover_pointer: None,
            visible: Vec::new(),
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
        self.recompute_visible();
        self.selected = selected_alias
            .and_then(|alias| self.targets.iter().position(|t| t.alias == alias))
            .unwrap_or(0)
            .min(self.targets.len().saturating_sub(1));
        self.scroll_to(self.scroll);
    }

    fn recompute_visible(&mut self) {
        let needle: String = self.filter.to_lowercase();
        self.visible.clear();
        for (i, t) in self.targets.iter().enumerate() {
            if needle.is_empty() {
                self.visible.push(i);
                continue;
            }
            let alias_l = t.alias.to_lowercase();
            let detail_l = t.detail().to_lowercase();
            if alias_l.contains(&needle) || detail_l.contains(&needle) {
                self.visible.push(i);
            }
        }
    }

    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.recompute_visible();
        self.selected = self.visible.first().copied().unwrap_or(0);
        self.scroll = 0;
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.recompute_visible();
        if !self.visible.contains(&self.selected) {
            self.selected = self.visible.first().copied().unwrap_or(0);
        }
        self.scroll = 0;
    }

    pub fn clear_filter(&mut self) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        self.filter.clear();
        self.recompute_visible();
        true
    }

    pub fn visible_indices(&self) -> &[usize] {
        &self.visible
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
        self.visible.get(idx).copied()
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.targets.len() && self.visible.contains(&idx) {
            self.selected = idx;
        }
    }

    pub fn move_up(&mut self) {
        let Some(pos) = self.visible.iter().position(|&i| i == self.selected) else {
            self.selected = self.visible.first().copied().unwrap_or(0);
            return;
        };
        if pos > 0 {
            self.selected = self.visible[pos - 1];
        }
    }

    pub fn move_down(&mut self) {
        let Some(pos) = self.visible.iter().position(|&i| i == self.selected) else {
            self.selected = self.visible.first().copied().unwrap_or(0);
            return;
        };
        if pos + 1 < self.visible.len() {
            self.selected = self.visible[pos + 1];
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
            self.visible.len(),
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
        if viewport == 0 || self.visible.is_empty() {
            self.scroll = 0;
            self.selected = self.visible.first().copied().unwrap_or(0);
            return;
        }
        self.scroll = top.min(self.visible.len().saturating_sub(viewport));
        let last_visible = (self.scroll + viewport - 1).min(self.visible.len().saturating_sub(1));
        let cur_pos = self.visible.iter().position(|&i| i == self.selected);
        match cur_pos {
            Some(pos) if pos < self.scroll => {
                self.selected = self.visible[self.scroll];
            }
            Some(pos) if pos > last_visible => {
                self.selected = self.visible[last_visible];
            }
            None => {
                self.selected = self.visible[self.scroll];
            }
            _ => {}
        }
    }
}

impl Default for RemotePanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Teal/cyan accent color sampled from the `v0.1.122` version chip in the
/// croft mockup. Used by the empty-state card, the secondary button outline,
/// the illustration line art, and the learn-more link.
const CARD_BORDER: Color = Color::Rgb(
    crate::gradient::CARD_ACCENT.0,
    crate::gradient::CARD_ACCENT.1,
    crate::gradient::CARD_ACCENT.2,
);
const PRIMARY_BG: Color = Color::Rgb(
    crate::gradient::PRIMARY_BTN_BG.0,
    crate::gradient::PRIMARY_BTN_BG.1,
    crate::gradient::PRIMARY_BTN_BG.2,
);
const LINK_FG: Color = CARD_BORDER;
const BODY_FG: Color = Color::Rgb(0xb4, 0xbe, 0xc8);

pub const SSH_EMPTY_STATE_CELLS_W: u16 = 18;
pub const SSH_EMPTY_STATE_CELLS_H: u16 = 8;

// The header pill renderer, chip colours and codicon glyphs are shared with the
// Source Control header; re-export the glyph so existing call sites and tests
// keep using `crate::widgets::remote::REFRESH_GLYPH`.
pub use crate::widgets::header_pill::{ADD_GLYPH, REFRESH_GLYPH};

fn render_section_header(panel: &mut RemotePanel, buf: &mut Buffer, inner: Rect) {
    let header_style = Style::default()
        .fg(panel.theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc)))
        .add_modifier(Modifier::BOLD);
    let chevron = if panel.collapsed { '▸' } else { '▾' };
    buf.set_line(
        inner.x,
        inner.y,
        &Line::from(vec![
            Span::styled(format!("{chevron} "), Style::default().fg(Color::Gray)),
            Span::styled("SSH", header_style),
        ]),
        inner.width,
    );
    panel.header_chevron_btn = Rect {
        x: inner.x,
        y: inner.y,
        width: 5.min(inner.width),
        height: 1,
    };
    if inner.width < 10 {
        return;
    }
    let pill_w: u16 = 2;
    let right = inner.x + inner.width;
    let refresh_x = right.saturating_sub(pill_w + 1);
    let add_x = refresh_x.saturating_sub(pill_w + 1);
    let pointer = panel.hover_pointer;
    if add_x > inner.x + 5 {
        let rect = Rect {
            x: add_x,
            y: inner.y,
            width: pill_w,
            height: 1,
        };
        panel.header_add_btn = rect;
        let hovered = crate::widgets::hover::contains(rect, pointer);
        crate::widgets::header_pill::render(buf, add_x, inner.y, ADD_GLYPH, panel.theme, hovered);
    }
    if refresh_x > inner.x + 5 {
        let rect = Rect {
            x: refresh_x,
            y: inner.y,
            width: pill_w,
            height: 1,
        };
        panel.header_refresh_btn = rect;
        let hovered = crate::widgets::hover::contains(rect, pointer);
        crate::widgets::header_pill::render(
            buf,
            refresh_x,
            inner.y,
            REFRESH_GLYPH,
            panel.theme,
            hovered,
        );
    }
}

fn render_filter_row(buf: &mut Buffer, area: Rect, filter: &str, theme: crate::theme::Theme) {
    if area.width < 4 {
        return;
    }
    for dx in 0..area.width {
        let cell = &mut buf[(area.x + dx, area.y)];
        cell.set_char(' ');
        cell.set_style(Style::default());
    }
    let label_style = Style::default()
        .fg(theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)))
        .add_modifier(Modifier::ITALIC);
    let val_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let prefix = "filter: ";
    buf.set_string(area.x, area.y, prefix, label_style);
    let avail = area.width as usize - prefix.chars().count();
    let shown: String = filter.chars().take(avail).collect();
    buf.set_string(
        area.x + prefix.chars().count() as u16,
        area.y,
        &shown,
        val_style,
    );
}

fn render_empty_state(panel: &mut RemotePanel, buf: &mut Buffer, area: Rect) {
    panel.last_image_cell = None;
    panel.last_image_area = Rect::default();
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
        .border_style(Style::default().fg(panel.theme.ui(CARD_BORDER)));
    let card_inner = card_block.inner(card_rect);
    card_block.render(card_rect, buf);

    let content_x = card_inner.x;
    let content_w = card_inner.width;
    let content_end_y = card_inner.y + card_inner.height;
    let mut y = card_inner.y.saturating_add(1);

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
        panel.last_image_area = Rect {
            x: img_x,
            y,
            width: img_w,
            height: img_h,
        };
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
    let body_fg = Style::default().fg(panel.theme.ui(BODY_FG));
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
        panel.empty_primary_btn = render_filled_button(
            buf,
            btn_x,
            y,
            btn_width,
            "+  Add New Host",
            panel.theme.ui(PRIMARY_BG),
        );
        y += 4;
    }

    if y + 3 <= content_end_y {
        panel.empty_secondary_btn =
            render_outlined_button(buf, btn_x, y, btn_width, "Open SSH Config", panel.theme);
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
                Style::default()
                    .fg(panel.theme.ui(LINK_FG))
                    .add_modifier(Modifier::UNDERLINED),
            );
            panel.empty_learn_link = Rect {
                x: lx,
                y,
                width: w,
                height: 1,
            };
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
    let right = x + width.saturating_sub(1);
    buf[(x, y)].set_char('╭');
    buf[(right, y)].set_char('╮');
    buf[(x, y + 2)].set_char('╰');
    buf[(right, y + 2)].set_char('╯');
    let lbl_w = label.chars().count() as u16;
    let lbl_x = x + (width.saturating_sub(lbl_w)) / 2;
    buf.set_string(lbl_x, y + 1, label, label_style);
    Rect {
        x,
        y,
        width,
        height: 3,
    }
}

fn render_outlined_button(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    label: &str,
    theme: crate::theme::Theme,
) -> Rect {
    for dx in 0..width {
        for dy in 0..3 {
            let cell = &mut buf[(x + dx, y + dy)];
            cell.set_char(' ');
            cell.set_style(Style::default());
        }
    }
    let right = x + width.saturating_sub(1);
    let border_style = Style::default().fg(theme.ui(CARD_BORDER));
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
        .fg(theme.ui(CARD_BORDER))
        .add_modifier(Modifier::BOLD);
    let lbl_w = label.chars().count() as u16;
    let lbl_x = x + (width.saturating_sub(lbl_w)) / 2;
    buf.set_string(lbl_x, y + 1, label, label_style);
    Rect {
        x,
        y,
        width,
        height: 3,
    }
}

impl Widget for &mut RemotePanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Black theme: chipless brand header (panelTitle.activeForeground);
        // Croft Dark keeps the historical white-on-navy chip.
        let title = if self.focus_gradient {
            Span::styled(
                " REMOTE EXPLORER ",
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                " REMOTE EXPLORER ",
                Style::default()
                    .fg(Color::White)
                    .bg(self.theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient,
        // then re-stamp the title the gradient top edge just overwrote.
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
            buf.set_span(area.x + 1, area.y, &title, title.width() as u16);
        }
        self.last_area = area;
        self.last_inner = inner;
        self.last_scrollbar = Rect::default();
        self.header_chevron_btn = Rect::default();
        self.header_add_btn = Rect::default();
        self.header_refresh_btn = Rect::default();
        self.empty_primary_btn = Rect::default();
        self.empty_secondary_btn = Rect::default();
        self.empty_learn_link = Rect::default();

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        render_section_header(self, buf, inner);

        if self.collapsed {
            self.last_list_area = Rect {
                x: inner.x,
                y: inner.y.saturating_add(1),
                width: inner.width,
                height: 0,
            };
            return;
        }

        self.recompute_visible();

        let mut body_y = inner.y.saturating_add(1);
        let body_end = inner.y + inner.height;
        if !self.filter.is_empty() && body_y < body_end {
            render_filter_row(
                buf,
                Rect {
                    x: inner.x,
                    y: body_y,
                    width: inner.width,
                    height: 1,
                },
                &self.filter,
                self.theme,
            );
            body_y = body_y.saturating_add(1);
        }
        self.last_list_area = Rect {
            x: inner.x,
            y: body_y,
            width: inner.width,
            height: body_end.saturating_sub(body_y),
        };

        if self.visible.is_empty() {
            if self.targets.is_empty() {
                render_empty_state(self, buf, self.last_list_area);
            } else if !self.filter.is_empty() && self.last_list_area.height > 0 {
                let msg = format!("No hosts match \"{}\"", self.filter);
                let w = msg.chars().count() as u16;
                let cx = inner.x + inner.width.saturating_sub(w) / 2;
                buf.set_string(
                    cx,
                    self.last_list_area.y,
                    &msg,
                    Style::default()
                        .fg(self.theme.ui(BODY_FG))
                        .add_modifier(Modifier::ITALIC),
                );
            }
            return;
        }

        let viewport = self.last_list_area.height as usize;
        if viewport == 0 {
            return;
        }
        let sel_pos = self
            .visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        if sel_pos < self.scroll {
            self.scroll = sel_pos;
        } else if sel_pos >= self.scroll + viewport {
            self.scroll = sel_pos + 1 - viewport;
        }

        let scrollbar_area = Rect {
            x: self.last_list_area.x + self.last_list_area.width.saturating_sub(1),
            y: self.last_list_area.y,
            width: u16::from(self.last_list_area.width > 0),
            height: self.last_list_area.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, self.visible.len(), viewport, self.scroll);
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = self
            .last_list_area
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        let sel_bg = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)
        } else {
            self.theme.ui(Color::Rgb(0x09, 0x4d, 0x77))
        };
        let pointer = self.hover_pointer;
        let end = (self.scroll + viewport).min(self.visible.len());
        for (row, vis_idx) in (self.scroll..end).enumerate() {
            let idx = self.visible[vis_idx];
            let target = &self.targets[idx];
            let y = self.last_list_area.y + row as u16;
            let selected = idx == self.selected;
            let row_rect = Rect {
                x: self.last_list_area.x,
                y,
                width: row_width,
                height: 1,
            };
            let style = if selected {
                Style::default().bg(sel_bg)
            } else if let Some(bg) =
                crate::widgets::hover::row_hover_bg(row_rect, pointer, self.theme)
            {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            buf.set_style(row_rect, style);
            let detail = target.detail();
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    "\u{f108} ",
                    Style::default().fg(self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4))),
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
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
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
        p.recompute_visible();
        p
    }

    fn populated_panel(aliases: &[&str]) -> RemotePanel {
        let mut p = RemotePanel::new();
        p.targets = aliases
            .iter()
            .map(|a| RemoteTarget {
                alias: (*a).to_string(),
                host_name: None,
                user: None,
            })
            .collect();
        p.recompute_visible();
        p
    }

    #[test]
    fn header_uses_cod_refresh_glyph_not_gear() {
        assert_eq!(
            REFRESH_GLYPH, '\u{eb37}',
            "header refresh button must use cod-refresh (the circular-arrow glyph VS Code uses on the Remote Explorer refresh affordance)"
        );
        assert_ne!(
            REFRESH_GLYPH, '\u{2699}',
            "U+2699 is the gear symbol; this is the bug the user reported — the header was painting a settings gear next to the + icon"
        );
    }

    #[test]
    fn header_paints_refresh_glyph_in_buffer_when_panel_renders() {
        let mut p = populated_panel(&["a", "b"]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let header_y = p.header_refresh_btn.y;
        let mut header_row = String::new();
        for x in area.left()..area.right() {
            header_row.push_str(buf[(x, header_y)].symbol());
        }
        assert!(
            header_row.contains(REFRESH_GLYPH),
            "section-header row must contain the cod-refresh glyph U+EB37 (got: {header_row:?})"
        );
        assert!(
            !header_row.contains('\u{2699}'),
            "header must not paint the gear U+2699 anywhere"
        );
    }

    #[test]
    fn header_add_pill_brightens_under_the_pointer() {
        let mut p = populated_panel(&["a", "b"]);
        p.focus_gradient = false; // Croft Dark navy pill
        p.theme = crate::theme::Theme::DARK_BLUE;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let add = p.header_add_btn;
        let refresh = p.header_refresh_btn;
        assert!(add.width > 0, "the + pill must have rendered");
        // Rest the pointer on the + pill and re-render.
        p.hover_pointer = Some((add.x, add.y));
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            buf[(add.x, add.y)].bg,
            crate::widgets::header_pill::HEADER_BTN_HOVER_BG,
            "the + pill lifts to the hover navy under the pointer"
        );
        assert_eq!(
            buf[(refresh.x, refresh.y)].bg,
            crate::widgets::header_pill::HEADER_BTN_BG,
            "the un-hovered refresh pill keeps the rest navy"
        );
    }

    #[test]
    fn hovering_an_unselected_host_row_lifts_it() {
        let mut p = populated_panel(&["a", "b", "c"]);
        p.focus_gradient = false; // Croft Dark
        p.theme = crate::theme::Theme::DARK_BLUE;
        p.selected = 0; // row 0 is selected
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let row_y = p.last_list_area.y + 1; // second host, unselected
        p.hover_pointer = Some((p.last_list_area.x, row_y));
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            buf[(p.last_list_area.x, row_y)].bg,
            Color::Rgb(0x2b, 0x31, 0x42),
            "the hovered unselected host row wears the Croft Dark hover lift"
        );
    }

    #[test]
    fn black_theme_selected_host_uses_muted_brand_teal_not_legacy_blue() {
        use crate::gradient::{POPUP_SEL_BG, rgb_color};
        let mut p = populated_panel(&["a", "b"]);
        p.focus_gradient = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let sel_y = p.last_list_area.y; // selected defaults to row 0
        assert_eq!(
            buf[(p.last_list_area.x, sel_y)].bg,
            rgb_color(POPUP_SEL_BG),
            "under the Black theme the selected SSH host row must use the muted brand teal that Explorer/Search use, not the legacy bright-blue highlight"
        );
        assert_ne!(
            buf[(p.last_list_area.x, sel_y)].bg,
            Color::Rgb(0x09, 0x4d, 0x77),
            "the old #094d77 blue is the regression the user reported on the Remote Explorer"
        );
    }

    #[test]
    fn croft_dark_selected_host_keeps_solid_blue_highlight() {
        let mut p = populated_panel(&["a", "b"]);
        p.focus_gradient = false;
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let sel_y = p.last_list_area.y;
        assert_eq!(
            buf[(p.last_list_area.x, sel_y)].bg,
            Color::Rgb(0x09, 0x4d, 0x77),
            "Croft Dark retains the historical solid-blue selection, matching the file tree's non-gradient branch"
        );
    }

    #[test]
    fn header_icon_buttons_are_two_cells_wide_so_they_read_as_compact_chips() {
        let mut p = populated_panel(&["a"]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(
            p.header_add_btn.width, 2,
            "header + pill must be 2 cells wide — the 3-cell version felt oversized next to the SSH label per user feedback on 2026-05-18"
        );
        assert_eq!(p.header_refresh_btn.width, 2);
        assert!(
            p.header_add_btn.x + p.header_add_btn.width <= p.header_refresh_btn.x
                || p.header_refresh_btn.x + p.header_refresh_btn.width <= p.header_add_btn.x,
            "header pills must not overlap"
        );
    }

    #[test]
    fn chevron_click_rect_is_registered_at_header_origin() {
        let mut p = populated_panel(&["a"]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(
            p.header_chevron_btn.width > 0,
            "chevron must register a hit rect so clicking the ▾ collapses the SSH section"
        );
        assert_eq!(p.header_chevron_btn.y, 1);
    }

    #[test]
    fn toggle_collapsed_hides_list_rows_but_keeps_header_buttons_clickable() {
        let mut p = populated_panel(&["a", "b", "c"]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        p.toggle_collapsed();
        (&mut p).render(area, &mut buf);
        assert!(p.collapsed);
        assert_eq!(
            p.last_list_area.height, 0,
            "collapsed section must reserve zero rows for the host list"
        );
        assert!(
            p.header_add_btn.width > 0,
            "collapsed header still needs to be able to add a host"
        );
        assert!(p.header_refresh_btn.width > 0);
        assert!(p.header_chevron_btn.width > 0);
    }

    #[test]
    fn type_to_filter_narrows_visible_aliases_case_insensitively() {
        let mut p = populated_panel(&["genesis-cloud", "github.com", "vllm-server"]);
        p.push_filter_char('G');
        p.push_filter_char('e');
        let aliases: Vec<&str> = p
            .visible_indices()
            .iter()
            .map(|&i| p.targets[i].alias.as_str())
            .collect();
        assert_eq!(aliases, vec!["genesis-cloud"]);
    }

    #[test]
    fn type_to_filter_empty_query_shows_every_host() {
        let mut p = populated_panel(&["alpha", "beta"]);
        p.push_filter_char('a');
        p.pop_filter_char();
        let n = p.visible_indices().len();
        assert_eq!(
            n, 2,
            "after backspacing the only filter char the panel must show every host again"
        );
    }

    #[test]
    fn type_to_filter_keeps_selection_within_visible_set() {
        let mut p = populated_panel(&["alpha", "bravo", "charlie"]);
        p.selected = 2;
        p.push_filter_char('a');
        let sel = p.selected;
        assert!(
            p.visible_indices().contains(&sel),
            "after applying a filter the selected index must still belong to the visible set; otherwise Enter would launch a hidden host"
        );
    }

    #[test]
    fn target_at_y_resolves_against_filtered_rows_not_raw_targets() {
        let mut p = populated_panel(&["alpha", "bravo", "charlie"]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        p.push_filter_char('b');
        (&mut p).render(area, &mut buf);
        let first_row_y = p.last_list_area.y;
        let idx = p
            .target_at_y(first_row_y)
            .expect("first row must resolve to a target");
        assert_eq!(
            p.targets[idx].alias, "bravo",
            "clicking the first visible row under a filter must select the matching host, not raw targets[0]"
        );
    }

    #[test]
    fn clear_filter_returns_true_when_filter_had_chars() {
        let mut p = populated_panel(&["alpha"]);
        p.push_filter_char('x');
        assert!(p.clear_filter());
        assert!(!p.clear_filter());
        assert!(p.filter.is_empty());
    }

    #[test]
    fn empty_state_registers_hit_rects_for_every_actionable_affordance() {
        let mut p = empty_panel();
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(p.header_add_btn.width > 0);
        assert!(p.header_refresh_btn.width > 0);
        assert!(p.empty_primary_btn.width > 0);
        assert!(p.empty_secondary_btn.width > 0);
        assert!(p.empty_learn_link.width > 0);
    }

    #[test]
    fn populated_state_does_not_register_empty_state_hit_rects() {
        let mut p = populated_panel(&["dev"]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(p.empty_primary_btn, Rect::default());
        assert_eq!(p.empty_secondary_btn, Rect::default());
        assert_eq!(p.empty_learn_link, Rect::default());
    }

    #[test]
    fn empty_state_body_text_never_overflows_card_width_at_real_sidebar_sizes() {
        for sidebar_w in 24u16..=44 {
            let mut p = empty_panel();
            let area = Rect {
                x: 0,
                y: 0,
                width: sidebar_w,
                height: 40,
            };
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
                    let is_border = matches!(last, '╮' | '╯' | '│' | '─' | '┌' | '┐' | '└' | '┘');
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert_eq!(p.empty_primary_btn.height, 3);
        assert_eq!(p.empty_secondary_btn.height, 3);
    }

    #[test]
    fn empty_state_reserves_a_cell_for_the_osc_1337_illustration() {
        let mut p = empty_panel();
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        assert!(p.last_image_cell.is_some());
    }

    #[test]
    fn empty_state_renders_the_no_hosts_heading_so_the_user_knows_what_state_they_are_in() {
        let mut p = empty_panel();
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        let mut joined = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                joined.push_str(buf[(x, y)].symbol());
            }
            joined.push('\n');
        }
        assert!(joined.contains("No SSH hosts yet"));
        assert!(joined.contains("Add New Host"));
        assert!(joined.contains("Open SSH Config"));
        assert!(joined.contains("Learn more about SSH"));
    }
}
