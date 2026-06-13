//! Shared section-header pill button (the small chip-backed codicon affordances
//! VS Code paints at the right of a view's title row). Used by both the Remote
//! Explorer header (`+` / refresh) and the Source Control header (refresh), so
//! the navy-chip / brand-teal theming stays defined in exactly one place.

use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier, Style},
};

/// Background colour painted under a header pill button so it reads as a touch
/// target, not a naked single glyph. Matches the chip behind the view title.
pub const HEADER_BTN_BG: Color = Color::Rgb(0x1e, 0x3a, 0x6e);
const HEADER_BTN_FG: Color = Color::Rgb(0xe6, 0xed, 0xf5);
/// Brightened navy the Croft Dark header pill takes while the pointer rests on
/// it (VS Code `toolbar.hoverBackground`). The Black theme instead grows a
/// faint teal pill from nothing, so it has no separate constant here.
pub const HEADER_BTN_HOVER_BG: Color = Color::Rgb(0x2f, 0x5a, 0xa8);

/// Codicon `cod-refresh` — the circular-arrow glyph VS Code paints on a view's
/// refresh affordance. Verified against the Nerd Font cmap.
pub const REFRESH_GLYPH: char = '\u{eb37}';
pub const ADD_GLYPH: char = '\u{ea60}';

/// Paint a header pill button. `brand` is true under the Black theme: a
/// chipless teal icon in the VS Code toolbar spirit that grows a faint teal
/// pill only while hovered. Croft Dark keeps the navy pill and brightens it on
/// hover (`toolbar.hoverBackground`).
pub fn render(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    glyph: char,
    brand: bool,
    hovered: bool,
) {
    let fill = if brand {
        hovered.then(|| crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
    } else if hovered {
        Some(HEADER_BTN_HOVER_BG)
    } else {
        Some(HEADER_BTN_BG)
    };
    let fill_style = fill.map(|c| Style::default().bg(c)).unwrap_or_default();
    for dx in 0..width {
        let cell = &mut buf[(x + dx, y)];
        cell.set_char(' ');
        cell.set_style(fill_style);
    }
    let centre = x + width / 2;
    let mut style = Style::default().add_modifier(Modifier::BOLD);
    if let Some(c) = fill {
        style = style.bg(c);
    }
    style = if brand {
        style.fg(crate::gradient::rgb_color(crate::gradient::INNER_ACCENT))
    } else {
        style.fg(HEADER_BTN_FG)
    };
    buf.set_string(centre, y, glyph.to_string(), style);
}
