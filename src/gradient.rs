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

/// Bright teal/cyan card accent (borders, line-art illustrations, links) on
/// the empty-state cards under the Black theme — the same value the Remote
/// Explorer card has always used locally as `CARD_BORDER`/`LINK_FG`.
pub const CARD_ACCENT: (u8, u8, u8) = (0x3e, 0xd8, 0xc4);

/// Brand-teal fill for primary action buttons (`Initialize Repository`,
/// `Run and Debug`, the Remote Explorer `Connect`) under the Black theme,
/// replacing the legacy VS Code button blue (#0967b8). Same value the remote
/// empty-state card has always used, promoted here as the single source.
pub const PRIMARY_BTN_BG: (u8, u8, u8) = (0x0e, 0x7e, 0x76);

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

/// Rows of dotted contour lines drawn into a card's interior (#312).
///
/// Three curves, phase-shifted, so the field reads as a landscape rather
/// than as one repeated ripple.
const WAVE_CURVES: usize = 3;

/// Paint the ambient dotted wave into the right of a card's interior (#312).
///
/// Only ever writes to cells that are still BLANK, so the card's text always
/// wins: the wave is atmosphere behind the content, and a release note that
/// grows to fill the card simply pushes it out of view rather than being
/// painted over. That is also why it is painted last by the caller, when
/// what the text occupies is already known.
///
/// The curves fade left to right, from nothing at the midpoint to full
/// strength at the card's right edge, so the field never competes with the
/// note text that starts on the left. Teal and orange are the brand
/// gradient's own corners rather than new colours.
pub fn paint_card_wave(buf: &mut Buffer, inner: Rect, theme_ui: impl Fn(Color) -> Color) {
    if inner.width < 24 || inner.height < 3 {
        return;
    }
    let buf_area = buf.area;
    let w = inner.width as f32;
    let h = inner.height as f32;
    // The left half stays clear for the text; the wave lives in the right.
    let start = inner.width / 2;
    for col in start..inner.width {
        let x = inner.x + col;
        if x < buf_area.x || x >= buf_area.x + buf_area.width {
            continue;
        }
        // 0 at the midpoint, 1 at the right edge: the field emerges rather
        // than starting abruptly.
        let ramp = (col - start) as f32 / (inner.width - start).max(1) as f32;
        let phase = col as f32 / w;
        for curve in 0..WAVE_CURVES {
            let c = curve as f32;
            // Each curve peaks at a different place and sits at a different
            // height, which is what makes the three read as one landscape.
            let peak = (phase * 6.0 + c * 0.9).sin() * 0.5 + 0.5;
            let level = h * (0.30 + 0.22 * c) + peak * h * 0.42;
            let row = level.round();
            if !(0.0..h).contains(&row) {
                continue;
            }
            let y = inner.y + row as u16;
            if y < buf_area.y || y >= buf_area.y + buf_area.height {
                continue;
            }
            let cell = &mut buf[(x, y)];
            // Blank only: the text was painted first and keeps its cells.
            if cell.symbol() != " " {
                continue;
            }
            // The near curve carries the orange the wordmark uses; the ones
            // behind it recede into teal.
            let tint = lerp_rgb(GRAD_BL, GRAD_TR, if curve == 0 { 0.85 } else { 0.10 });
            let faded = lerp_rgb(GRAD_BL, tint, 0.25 + 0.75 * ramp);
            cell.set_symbol("\u{b7}");
            cell.set_style(Style::default().fg(theme_ui(rgb_color(faded))));
        }
    }
}

#[cfg(test)]
mod wave_tests {
    use super::*;

    fn blank(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        })
    }

    /// The wave is atmosphere: a cell the card already painted keeps what it
    /// has (#312). This is the whole contract, because the alternative is a
    /// decorative field eating a release note.
    #[test]
    fn the_wave_never_overwrites_a_painted_cell() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 8,
        };
        let mut buf = blank(60, 8);
        // Fill every cell with text, as a card whose notes reach the edges.
        for y in 0..8 {
            buf.set_string(0, y, "x".repeat(60), Style::default());
        }
        let before = buf.clone();
        paint_card_wave(&mut buf, area, |c| c);
        assert_eq!(buf, before, "a full card is left exactly as it was painted");
    }

    /// And it does paint SOMETHING when there is room, or the test above
    /// would pass against a function that does nothing at all.
    #[test]
    fn the_wave_paints_into_the_blank_right_of_a_card() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 8,
        };
        let mut buf = blank(60, 8);
        paint_card_wave(&mut buf, area, |c| c);
        let dots = (0..8)
            .flat_map(|y| (0..60).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() == "\u{b7}")
            .count();
        assert!(dots > 0, "the field is drawn on an empty card");
        // And never into the left half, which the note text owns.
        let left = (0..8)
            .flat_map(|y| (0..30).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() == "\u{b7}")
            .count();
        assert_eq!(left, 0, "the text side stays clear");
    }

    /// A card too small to carry a field gets none rather than a stripe.
    #[test]
    fn a_narrow_card_gets_no_wave() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 8,
        };
        let mut buf = blank(20, 8);
        let before = buf.clone();
        paint_card_wave(&mut buf, area, |c| c);
        assert_eq!(buf, before);
    }
}
