//! The orange→green corner gradient shared by the welcome "Recent Activity"
//! box and, under the Black theme, the focused-pane border. Defined once here
//! so both surfaces sweep through identical colours.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

pub const GRAD_TL: (u8, u8, u8) = (0x5c, 0xd6, 0xc8);
pub const GRAD_TR: (u8, u8, u8) = (0xec, 0x8c, 0x5a);
pub const GRAD_BR: (u8, u8, u8) = (0x4f, 0xb1, 0xa6);
pub const GRAD_BL: (u8, u8, u8) = (0x35, 0x80, 0x78);

/// Muted dark-teal fill for the selected row of popups/menus under the Black
/// theme, replacing the legacy bright-blue (`#4e9aff`) highlight inherited from
/// the pre-brand VS Code accent. Quiet enough that the gradient border carries
/// the brand identity; white text reads cleanly on top.
pub const POPUP_SEL_BG: (u8, u8, u8) = (0x26, 0x4f, 0x4a);

/// VS Code's `panelTitle.activeForeground` (#E7E7E7): the chipless panel
/// header text under the Black theme (e.g. the TERMINAL title), replacing the
/// legacy white-on-navy chip so the gradient border alone carries the brand.
pub const PANEL_TITLE_FG: (u8, u8, u8) = (0xe7, 0xe7, 0xe7);

/// Muted brand teal (the gradient's bottom-right corner) used as the inner
/// stroke accent under the Black theme — input-box focus rings, text cursors,
/// chevrons, and magnifier glyphs — replacing the legacy bright-blue
/// (`#4e9aff`). Quieter than the top-left teal so inner chrome doesn't shout
/// over the gradient border that carries the brand.
pub const INNER_ACCENT: (u8, u8, u8) = (0x4f, 0xb1, 0xa6);

pub fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| ((1.0 - t) * x as f32 + t * y as f32).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

pub fn rgb_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Paint a rounded-rectangle border whose stroke colour interpolates
/// linearly between the four corner colours along each edge. The interior
/// is left untouched so the caller can fill it with content.
///
/// The rect is clipped against the buffer's area, so callers don't have to
/// do the bounds math themselves — passing a rect that runs off the edge
/// (e.g., a 80x25 default startup buffer with a tall recents list) draws
/// nothing instead of panicking inside `set_string`.
pub fn paint_gradient_box(buf: &mut Buffer, rect: Rect) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let buf_area = buf.area;
    if rect.x < buf_area.x
        || rect.y < buf_area.y
        || rect.x + rect.width > buf_area.x + buf_area.width
        || rect.y + rect.height > buf_area.y + buf_area.height
    {
        return;
    }
    let max_x = rect.width - 1;
    let max_y = rect.height - 1;
    for x in 0..rect.width {
        let u = if max_x > 0 {
            x as f32 / max_x as f32
        } else {
            0.0
        };
        let top = lerp_rgb(GRAD_TL, GRAD_TR, u);
        let bot = lerp_rgb(GRAD_BL, GRAD_BR, u);
        let top_ch = if x == 0 {
            "\u{256d}"
        } else if x == max_x {
            "\u{256e}"
        } else {
            "\u{2500}"
        };
        let bot_ch = if x == 0 {
            "\u{2570}"
        } else if x == max_x {
            "\u{256f}"
        } else {
            "\u{2500}"
        };
        buf.set_string(
            rect.x + x,
            rect.y,
            top_ch,
            Style::default().fg(rgb_color(top)),
        );
        buf.set_string(
            rect.x + x,
            rect.y + max_y,
            bot_ch,
            Style::default().fg(rgb_color(bot)),
        );
    }
    for y in 1..max_y {
        let v = if max_y > 0 {
            y as f32 / max_y as f32
        } else {
            0.0
        };
        let left = lerp_rgb(GRAD_TL, GRAD_BL, v);
        let right = lerp_rgb(GRAD_TR, GRAD_BR, v);
        buf.set_string(
            rect.x,
            rect.y + y,
            "\u{2502}",
            Style::default().fg(rgb_color(left)),
        );
        buf.set_string(
            rect.x + max_x,
            rect.y + y,
            "\u{2502}",
            Style::default().fg(rgb_color(right)),
        );
    }
}
