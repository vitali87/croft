use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget, Wrap, Paragraph},
};
use std::path::PathBuf;

const BUTTON_BG_RGB: (u8, u8, u8) = (0x09, 0x67, 0xb8);
const BUTTON_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);

pub struct RunDebugPanel {
    pub focused: bool,
    pub active_file: Option<PathBuf>,
    pub last_area: Rect,
    pub last_button_area: Rect,
    pub feedback: Option<String>,
    pub feedback_is_error: bool,
}

impl RunDebugPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            active_file: None,
            last_area: Rect::default(),
            last_button_area: Rect::default(),
            feedback: None,
            feedback_is_error: false,
        }
    }

    pub fn set_active_file(&mut self, path: Option<PathBuf>) {
        self.active_file = path;
    }

    pub fn click_button(&self, x: u16, y: u16) -> bool {
        let r = self.last_button_area;
        r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
    }

    pub fn button_label(&self) -> String {
        match self.active_file.as_ref().and_then(|p| p.file_name()) {
            Some(name) => format!("  Run {}  ", name.to_string_lossy()),
            None => String::from("  Run and Debug  "),
        }
    }
}

impl Default for RunDebugPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut RunDebugPanel {
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
                " RUN AND DEBUG ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_button_area = Rect::default();
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let dim = Style::default().fg(Color::Rgb(0xa0, 0xa8, 0xb8));
        let body = match self.active_file.as_ref() {
            Some(_) => "Press the button below to run the active file in a new terminal.",
            None => "Open a file which can be run, then press the button below.",
        };
        let para = Paragraph::new(Line::from(Span::styled(body, dim))).wrap(Wrap { trim: true });
        let body_height = inner.height.min(3);
        let body_area = Rect { x: inner.x, y: inner.y, width: inner.width, height: body_height };
        para.render(body_area, buf);

        let button_y = body_area.y + body_area.height + 1;
        if button_y >= inner.y + inner.height {
            return;
        }
        let label = self.button_label();
        let label_w = label.chars().count() as u16;
        let button_w = label_w.min(inner.width);
        let button_x = inner.x + (inner.width - button_w) / 2;
        let button_area = Rect { x: button_x, y: button_y, width: button_w, height: 1 };
        self.last_button_area = button_area;
        buf.set_style(
            button_area,
            Style::default().bg(Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)),
        );
        buf.set_string(
            button_area.x,
            button_area.y,
            label.as_str(),
            Style::default()
                .fg(Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2))
                .bg(Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2))
                .add_modifier(Modifier::BOLD),
        );

        let mut next_y = button_area.y + 1;
        if let Some(msg) = self.feedback.as_ref() {
            if next_y < inner.y + inner.height {
                let style = if self.feedback_is_error {
                    Style::default().fg(Color::Rgb(0xe7, 0x70, 0x70))
                } else {
                    Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))
                };
                buf.set_string(inner.x, next_y, msg.as_str(), style);
                next_y += 1;
            }
        }
        let _ = next_y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_label_says_run_and_debug_when_no_file_is_active() {
        let panel = RunDebugPanel::new();
        assert_eq!(panel.button_label(), "  Run and Debug  ");
    }

    #[test]
    fn button_label_includes_filename_when_a_file_is_active() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/script.py")));
        assert_eq!(panel.button_label(), "  Run script.py  ");
    }

    #[test]
    fn click_button_is_inside_recorded_button_area() {
        let mut panel = RunDebugPanel::new();
        panel.last_button_area = Rect { x: 10, y: 5, width: 12, height: 1 };
        assert!(panel.click_button(10, 5));
        assert!(panel.click_button(21, 5));
        assert!(!panel.click_button(22, 5));
        assert!(!panel.click_button(15, 6));
    }

    #[test]
    fn rendering_lays_out_button_area_inside_panel() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/run_me.rs")));
        let area = Rect { x: 0, y: 0, width: 40, height: 12 };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let b = panel.last_button_area;
        assert!(b.width > 0 && b.height > 0, "button area must be laid out");
        assert!(b.x >= area.x && b.x + b.width <= area.x + area.width);
        assert!(b.y >= area.y && b.y < area.y + area.height);
    }
}
