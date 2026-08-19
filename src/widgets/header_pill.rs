//! Shared section-header pill button (the small chip-backed codicon affordances
//! VS Code paints at the right of a view's title row). Used by both the Remote
//! Explorer header (`+` / refresh) and the Source Control header (refresh), so
//! the navy-chip / brand-teal theming stays defined in exactly one place.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
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
/// Codicon `cod-new_file` — the document-with-plus glyph VS Code paints on the
/// Explorer title's "New File" action. Verified against Nerd Fonts
/// `glyphnames.json` (`cod-new_file` = U+EA7F); do NOT use the upstream
/// `vscode-codicons/mapping.json` value, which targets a different PUA range.
pub const NEW_FILE_GLYPH: char = '\u{ea7f}';
/// Codicon `cod-new_folder` — the folder-with-plus glyph for "New Folder".
/// Nerd Fonts `cod-new_folder` = U+EA80.
pub const NEW_FOLDER_GLYPH: char = '\u{ea80}';
/// Codicon `cod-collapse_all` — the stacked-rows-with-minus glyph for
/// "Collapse Folders in Explorer". Nerd Fonts `cod-collapse_all` = U+EAC5.
pub const COLLAPSE_ALL_GLYPH: char = '\u{eac5}';
/// The "Views and More Actions" (⋯) affordance VS Code paints at the right of a
/// view-container header, as a space-padded action label so it reads as a real
/// pill inset from the rounded corner — the same `" + "` treatment the terminal
/// pane buttons wear. The glyph is the *midline* horizontal ellipsis (U+22EF
/// `⋯`), NOT the baseline U+2026 `…`: the midline variant centres its dots on
/// the line so they sit level with the EXPLORER caps and the `+`, instead of
/// dropping to the baseline. It is a standard Misc-Math-Symbols codepoint, not a
/// codicon PUA glyph, so it still renders on every terminal/font — the same
/// no-codicon-dependency reasoning behind the U+2715 close glyph.
pub const MORE_LABEL: &str = " \u{22ef} ";

/// Paint a header pill button at cell `(x, y)`. `brand` is true under the
/// Black theme: a chipless teal icon in the VS Code toolbar spirit that grows
/// a faint teal pill only while hovered. Croft Dark keeps the navy pill and
/// brightens it on hover (`toolbar.hoverBackground`).
///
/// The painted footprint is exactly **one cell** — it wraps the single-width
/// codicon so the chip and the glyph coincide perfectly. A wider chip around a
/// 1-cell glyph always left a bare highlight cell beside the icon, which read
/// as a misalignment. Callers keep a wider hit-test rect for a forgiving click
/// target; only the visible highlight is snug.
pub fn render(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    glyph: char,
    theme: crate::theme::Theme,
    hovered: bool,
) {
    let brand = theme.gradient();
    let fill = if brand {
        hovered.then(|| crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
    } else if hovered {
        Some(theme.ui(HEADER_BTN_HOVER_BG))
    } else {
        Some(theme.ui(HEADER_BTN_BG))
    };
    let mut style = Style::default().add_modifier(Modifier::BOLD);
    if let Some(c) = fill {
        style = style.bg(c);
    }
    // VS Code paints view-action icons in a light `icon.foreground`, not an
    // accent hue. The Black theme previously used the brand teal here, which
    // read as an out-of-place dark green; use the same near-white the panel
    // title wears so the icons sit quietly on the header.
    style = if brand {
        style.fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
    } else {
        style.fg(theme.ui(HEADER_BTN_FG))
    };
    buf.set_string(x, y, glyph.to_string(), style);
}

/// The shared style for a header *action* pill — the space-padded `" + "` /
/// `" ⋯ "` affordances that sit on a pane's top border. This is the single
/// source of truth the terminal pane buttons and the Explorer `⋯` button both
/// draw from, so the two can never drift apart again:
///
/// * Croft Dark (`!brand`): white-on-navy chip, bold (the historical look).
/// * Black idle (`brand`, not hovered): a chipless teal icon in the VS Code
///   toolbar spirit (`icon.foreground`), no bold.
/// * Black hover: white on the muted teal `POPUP_SEL_BG` pill standing in for
///   `toolbar.hoverBackground`.
pub fn action_style(theme: crate::theme::Theme, hovered: bool) -> Style {
    let brand = theme.gradient();
    if !brand {
        return Style::default()
            .fg(theme.ui(Color::White))
            .bg(theme.ui(HEADER_BTN_BG))
            .add_modifier(Modifier::BOLD);
    }
    if hovered {
        Style::default()
            .fg(Color::White)
            .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
    } else {
        Style::default().fg(crate::gradient::rgb_color(crate::gradient::INNER_ACCENT))
    }
}

/// Paint a space-padded action label (e.g. `" ⋯ "`) as a header pill whose top
/// edge sits inset one cell from the pane's rounded corner, and return its
/// hit-test rect. This is the design the terminal `+` / `-` buttons use; the
/// Explorer `⋯` shares it verbatim via [`action_style`]. `right_edge` is the
/// pane's right border column (`area.x + area.width - 1`); the label is laid
/// out ending one cell short of it so the rounded corner stays intact.
///
/// Returns `None` when the row is too narrow to seat the padded label without
/// colliding with the corner.
pub fn render_action(
    buf: &mut Buffer,
    right_edge: u16,
    y: u16,
    label: &str,
    theme: crate::theme::Theme,
    pointer: Option<(u16, u16)>,
) -> Option<Rect> {
    let w = label.chars().count() as u16;
    if right_edge < w + 1 {
        return None;
    }
    let x = right_edge - w; // leave the corner cell (right_edge) for the border
    let hovered = pointer.is_some_and(|(px, py)| py == y && px >= x && px < x + w);
    buf.set_string(x, y, label, action_style(theme, hovered));
    Some(Rect {
        x,
        y,
        width: w,
        height: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn croft_dark_highlight_wraps_exactly_the_glyph_cell() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 6,
            height: 1,
        };
        let mut buf = Buffer::empty(area);
        // Croft Dark (brand = false) always paints the navy chip. Render at x=2.
        render(
            &mut buf,
            2,
            0,
            REFRESH_GLYPH,
            crate::theme::Theme::DARK_BLUE,
            false,
        );
        assert_eq!(
            buf[(2, 0)].bg,
            HEADER_BTN_BG,
            "the glyph cell wears the navy chip"
        );
        assert_eq!(buf[(2, 0)].symbol().chars().next(), Some(REFRESH_GLYPH));
        // The whole point of the fix: no bare highlight cell beside the glyph,
        // so the chip and the single-width glyph coincide perfectly.
        assert_eq!(
            buf[(1, 0)].bg,
            Color::Reset,
            "no bare highlight cell left of the glyph"
        );
        assert_eq!(
            buf[(3, 0)].bg,
            Color::Reset,
            "no bare highlight cell right of the glyph"
        );
    }

    #[test]
    fn black_theme_idle_pill_is_chipless() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 6,
            height: 1,
        };
        let mut buf = Buffer::empty(area);
        // Black theme (brand = true), not hovered: a chipless teal glyph.
        render(
            &mut buf,
            2,
            0,
            REFRESH_GLYPH,
            crate::theme::Theme::BLACK,
            false,
        );
        assert_eq!(
            buf[(2, 0)].bg,
            Color::Reset,
            "the idle Black-theme pill grows its teal fill only on hover"
        );
    }
}
