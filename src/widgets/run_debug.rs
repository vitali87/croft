use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use std::path::PathBuf;

const BUTTON_BG_RGB: (u8, u8, u8) = (0x09, 0x67, 0xb8);
const BUTTON_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);
const BODY_FG_RGB: (u8, u8, u8) = (0xb0, 0xb8, 0xc8);
const TITLE_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);
const FOCUS_BORDER_RGB: (u8, u8, u8) = (0x4e, 0x9a, 0xff);

/// Cells reserved above the headline for the OSC-1337 debug-alt icon
/// overlay. Six cells wide × three cells tall lands a roughly 60×60-pixel
/// codicon at typical iTerm2 cell sizes (10×20 px), matching the VS Code
/// empty-state proportions the user supplied. The image is painted post-
/// frame by `App::flush_run_debug_icon_overlay`; on view change away from
/// Run-Debug the App's `terminal.clear()` evicts the cached image cells
/// (the same pipeline the welcome wordmark and editor preview use).
pub const RUN_DEBUG_ICON_CELLS_W: u16 = 6;
pub const RUN_DEBUG_ICON_CELLS_H: u16 = 3;

pub struct RunDebugPanel {
    pub focused: bool,
    pub active_file: Option<PathBuf>,
    pub last_area: Rect,
    pub last_button_area: Rect,
    /// Top-left cell of the OSC-1337 icon overlay block. The post-draw
    /// flush in `App` reads this to emit the rasterised debug-alt PNG
    /// above the headline. `None` when the panel hasn't been laid out,
    /// or when the panel is too short for the icon to fit alongside
    /// the rest of the cluster.
    pub last_icon_cell: Option<(u16, u16)>,
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
            last_icon_cell: None,
            feedback: None,
            feedback_is_error: false,
        }
    }

    pub fn set_active_file(&mut self, path: Option<PathBuf>) {
        self.active_file = path;
    }

    pub fn click_button(&self, x: u16, y: u16) -> bool {
        let r = self.last_button_area;
        r.width > 0
            && r.height > 0
            && x >= r.x
            && x < r.x + r.width
            && y >= r.y
            && y < r.y + r.height
    }

    pub fn button_label(&self) -> String {
        match self.active_file.as_ref().and_then(|p| p.file_name()) {
            Some(name) => format!("Run {}", name.to_string_lossy()),
            None => String::from("Run and Debug"),
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
        let border_style = if self.focused {
            Style::default().fg(Color::Rgb(
                FOCUS_BORDER_RGB.0,
                FOCUS_BORDER_RGB.1,
                FOCUS_BORDER_RGB.2,
            ))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_button_area = Rect::default();
        self.last_icon_cell = None;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Vertical cluster, top-to-bottom: icon, gap, headline, gap, body, gap, button.
        // Centre the cluster vertically so the panel feels balanced like the mockup.
        const GAP_AFTER_ICON: u16 = 2;
        const TITLE_H: u16 = 1;
        const GAP_AFTER_TITLE: u16 = 1;
        const BODY_MAX_H: u16 = 3;
        const GAP_AFTER_BODY: u16 = 2;
        const BUTTON_H: u16 = 3;

        let icon_w = RUN_DEBUG_ICON_CELLS_W.min(inner.width);
        let icon_h_full = RUN_DEBUG_ICON_CELLS_H;
        let cluster_full = icon_h_full
            + GAP_AFTER_ICON
            + TITLE_H
            + GAP_AFTER_TITLE
            + BODY_MAX_H
            + GAP_AFTER_BODY
            + BUTTON_H;

        // If the panel is too short for the full cluster, drop the icon
        // first (it's decorative) before sacrificing the body or button.
        let (icon_h, gap_after_icon, cluster) = if inner.height >= cluster_full {
            (icon_h_full, GAP_AFTER_ICON, cluster_full)
        } else {
            (
                0,
                0,
                TITLE_H + GAP_AFTER_TITLE + BODY_MAX_H + GAP_AFTER_BODY + BUTTON_H,
            )
        };

        let top_pad = if inner.height > cluster {
            (inner.height - cluster) / 2
        } else {
            0
        };
        let mut y = inner.y + top_pad;

        if icon_h > 0 && y + icon_h <= inner.y + inner.height {
            let icon_x = inner.x + (inner.width.saturating_sub(icon_w)) / 2;
            self.last_icon_cell = Some((icon_x, y));
            // Glyph fallback: paint the cod-debug-alt codicon centred
            // inside the reserved icon block so terminals without
            // OSC-1337 image support still see a recognisable shape.
            // On iTerm2 the post-draw `flush_run_debug_icon_overlay`
            // overwrites these cells with the proper rasterised PNG.
            let glyph_style = Style::default()
                .fg(Color::Rgb(BODY_FG_RGB.0, BODY_FG_RGB.1, BODY_FG_RGB.2))
                .add_modifier(Modifier::BOLD);
            let glyph_x = icon_x + icon_w / 2;
            let glyph_y = y + icon_h / 2;
            buf.set_span(
                glyph_x,
                glyph_y,
                &Span::styled(crate::icons::ACTIVITY_RUN_DEBUG.to_string(), glyph_style),
                1,
            );
            y += icon_h + gap_after_icon;
        }

        if y + TITLE_H > inner.y + inner.height {
            return;
        }
        let title = Paragraph::new(Line::from("RUN AND DEBUG"))
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Rgb(TITLE_FG_RGB.0, TITLE_FG_RGB.1, TITLE_FG_RGB.2))
                    .add_modifier(Modifier::BOLD),
            );
        title.render(
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: TITLE_H,
            },
            buf,
        );
        y += TITLE_H + GAP_AFTER_TITLE;

        if y >= inner.y + inner.height {
            return;
        }
        let body_text = match self.active_file.as_ref() {
            Some(_) => "Press the button below to run the active file in a new terminal.",
            None => "Open a file that can be run or debugged, then press the button below.",
        };
        let body_h = BODY_MAX_H.min(inner.y + inner.height - y);
        let body_x_pad = (inner.width / 12).clamp(1, 4);
        let body_area = Rect {
            x: inner.x + body_x_pad,
            y,
            width: inner.width.saturating_sub(body_x_pad * 2).max(1),
            height: body_h,
        };
        let body = Paragraph::new(body_text)
            .style(Style::default().fg(Color::Rgb(BODY_FG_RGB.0, BODY_FG_RGB.1, BODY_FG_RGB.2)))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        body.render(body_area, buf);
        y += BODY_MAX_H + GAP_AFTER_BODY;

        if y + BUTTON_H > inner.y + inner.height {
            return;
        }
        let label = self.button_label();
        let label_chars = label.chars().count() as u16;
        // Pin the button to the panel's interior so it can never be wider
        // than the available space; without this clamp `inner.width -
        // button_w` underflowed at narrow sidebar widths and ratatui
        // panicked indexing the buffer at a wrapped-u16 column.
        let button_w = (label_chars + 8).min(inner.width.saturating_sub(2));
        if button_w < 4 {
            return;
        }
        let button_x = inner.x + inner.width.saturating_sub(button_w) / 2;
        let button_area = Rect {
            x: button_x,
            y,
            width: button_w,
            height: BUTTON_H,
        };
        self.last_button_area = button_area;
        crate::widgets::source_control::render_rounded_button(
            buf,
            button_area,
            label.as_str(),
            Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2),
            Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2),
        );

        let mut next_y = button_area.y + button_area.height + 1;
        if let Some(msg) = self.feedback.as_ref()
            && next_y < inner.y + inner.height
        {
            let style = if self.feedback_is_error {
                Style::default().fg(Color::Rgb(0xe7, 0x70, 0x70))
            } else {
                Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c))
            };
            buf.set_string(inner.x + 1, next_y, msg.as_str(), style);
            next_y = next_y.saturating_add(1);
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
        assert_eq!(panel.button_label(), "Run and Debug");
    }

    #[test]
    fn button_label_includes_filename_when_a_file_is_active() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/script.py")));
        assert_eq!(panel.button_label(), "Run script.py");
    }

    #[test]
    fn click_button_is_inside_recorded_button_area() {
        let mut panel = RunDebugPanel::new();
        panel.last_button_area = Rect {
            x: 10,
            y: 5,
            width: 12,
            height: 3,
        };
        assert!(panel.click_button(10, 5));
        assert!(panel.click_button(21, 7));
        assert!(!panel.click_button(22, 5));
        assert!(!panel.click_button(15, 8));
    }

    #[test]
    fn rendering_lays_out_button_area_inside_panel() {
        let mut panel = RunDebugPanel::new();
        panel.set_active_file(Some(PathBuf::from("/work/run_me.rs")));
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let b = panel.last_button_area;
        assert!(b.width > 0 && b.height > 0, "button area must be laid out");
        assert!(b.x >= area.x && b.x + b.width <= area.x + area.width);
        assert!(b.y >= area.y && b.y < area.y + area.height);
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(buf.area.x + x, buf.area.y + y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn empty_state_renders_centred_title_and_description_and_chunky_button() {
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 36,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("RUN AND DEBUG"),
            "headline must be present: \n{dump}"
        );
        assert!(
            dump.contains("Open a file"),
            "body copy must be present: \n{dump}"
        );
        assert!(
            !dump.lines().next().unwrap().contains("RUN AND DEBUG"),
            "title bar must NOT sit on the top border row (mockup has no title bar): \n{dump}"
        );
        let button = panel.last_button_area;
        assert!(
            button.height >= 3,
            "button must be at least 3 rows tall to feel chunky like the mockup; got height={}",
            button.height
        );
        // The OSC-1337 icon block must be reserved ABOVE the button.
        let icon = panel.last_icon_cell.expect("icon block must be reserved");
        assert!(
            button.y > icon.1,
            "button must sit below the icon (icon top y={}, button.y={})",
            icon.1,
            button.y
        );
    }

    #[test]
    fn icon_block_reserves_an_osc_1337_overlay_cell_with_a_glyph_fallback() {
        // The headline icon is a 6x3-cell OSC-1337 image overlay (the
        // codicon `debug-alt` PNG, same one the activity-bar slot uses),
        // emitted post-frame by App::flush_run_debug_icon_overlay. The
        // widget's job is to (1) reserve the icon block via
        // last_icon_cell and (2) paint the codicon glyph as a text
        // fallback for terminals without OSC-1337 support; iTerm2
        // overwrites those cells with the rasterised image.
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 36,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let (ix, iy) = panel
            .last_icon_cell
            .expect("icon overlay cell must be reserved on a tall enough panel");
        let icon_w = RUN_DEBUG_ICON_CELLS_W;
        let icon_h = RUN_DEBUG_ICON_CELLS_H;
        let glyph_x = ix + icon_w / 2;
        let glyph_y = iy + icon_h / 2;
        let cell = buf[(glyph_x, glyph_y)].symbol().to_string();
        assert_eq!(
            cell,
            crate::icons::ACTIVITY_RUN_DEBUG.to_string(),
            "glyph fallback must be the cod-debug-alt codicon at the centre of the icon block"
        );
    }

    #[test]
    fn run_and_debug_button_uses_rounded_border_corners() {
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 36,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        let b = panel.last_button_area;
        assert!(b.height >= 3 && b.width >= 4, "button must be laid out");
        let tl = buf[(b.x, b.y)].symbol().to_string();
        let tr = buf[(b.x + b.width - 1, b.y)].symbol().to_string();
        let bl = buf[(b.x, b.y + b.height - 1)].symbol().to_string();
        let br = buf[(b.x + b.width - 1, b.y + b.height - 1)]
            .symbol()
            .to_string();
        assert_eq!(
            tl, "╭",
            "top-left button corner must be rounded; got {tl:?}"
        );
        assert_eq!(
            tr, "╮",
            "top-right button corner must be rounded; got {tr:?}"
        );
        assert_eq!(
            bl, "╰",
            "bottom-left button corner must be rounded; got {bl:?}"
        );
        assert_eq!(
            br, "╯",
            "bottom-right button corner must be rounded; got {br:?}"
        );
    }

    #[test]
    fn rendering_at_narrow_widths_does_not_panic() {
        // Repro for the user-reported crash: dragging the sidebar
        // splitter all the way to the left squeezed Run-and-Debug to a
        // few cells wide and ratatui panicked with `index outside of
        // buffer`. The cause was `inner.width - button_w` wrapping to a
        // huge u16 when the "Run and Debug" label (13 chars) was wider
        // than `inner.width`. Render the panel at every width from 0 up
        // to a reasonable maximum and assert nothing crashes.
        for width in 0u16..40 {
            for height in 0u16..30 {
                let mut panel = RunDebugPanel::new();
                panel.set_active_file(Some(PathBuf::from("/work/run_me.rs")));
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                if area.width == 0 || area.height == 0 {
                    continue;
                }
                let mut buf = Buffer::empty(area);
                Widget::render(&mut panel, area, &mut buf);
            }
        }
    }

    #[test]
    fn small_panel_skips_icon_block_to_keep_button_visible() {
        let mut panel = RunDebugPanel::new();
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        assert!(
            panel.last_button_area.height >= 3,
            "button must remain laid out when the panel collapses the icon block"
        );
    }
}
