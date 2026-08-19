//! Shared hover-feedback helpers.
//!
//! Clickable surfaces across the side panels (file tree, remote hosts,
//! source-control changes, search results) and the panel-header action pills
//! all want the same affordance: the row or element under the pointer lifts to
//! a subtle background, mirroring VS Code's `list.hoverBackground` /
//! `toolbar.hoverBackground`. Rather than re-derive the hit-test at every call
//! site, each renderer feeds its render-time pointer cell into a
//! `hover_pointer` field (set by the app from `pointer_cell`, exactly like
//! `EditorTabs`) and asks these helpers what to paint.

use ratatui::layout::Rect;
use ratatui::style::Color;

/// VS Code `list.hoverBackground`, fitted to each croft theme. Deliberately
/// subtler than any selection fill so a hovered-but-unselected row never reads
/// as selected. Picked from a true-color mockup: a neutral-navy lift on Croft
/// Dark, a plain dark-grey lift on the OLED-black theme.
const ROW_HOVER_BG_DARK: Color = Color::Rgb(0x2b, 0x31, 0x42);
const ROW_HOVER_BG_BLACK: Color = Color::Rgb(0x1a, 0x1a, 0x1a);
const ROW_HOVER_BG_LIGHT: Color = Color::Rgb(0xf2, 0xf2, 0xf2);

/// True when `pointer` rests on any cell of `rect`. The single hit-test every
/// hover affordance is built on.
pub fn contains(rect: Rect, pointer: Option<(u16, u16)>) -> bool {
    pointer.is_some_and(|(px, py)| {
        px >= rect.x
            && px < rect.x.saturating_add(rect.width)
            && py >= rect.y
            && py < rect.y.saturating_add(rect.height)
    })
}

/// The row-hover background for the active theme, or `None` when the pointer
/// is not over `rect`. Callers paint this only on rows that are not already
/// selected/marked, so it never masks a stronger state. VS Code's light
/// `list.hoverBackground` (#f2f2f2) on light themes; the gradient brand
/// (Croft Black) keeps its grey lift, every other dark theme the navy one.
pub fn row_hover_bg(
    rect: Rect,
    pointer: Option<(u16, u16)>,
    theme: crate::theme::Theme,
) -> Option<Color> {
    if !contains(rect, pointer) {
        return None;
    }
    Some(if theme.is_light() {
        ROW_HOVER_BG_LIGHT
    } else if theme.gradient() {
        ROW_HOVER_BG_BLACK
    } else {
        ROW_HOVER_BG_DARK
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn contains_is_inclusive_of_the_top_left_and_exclusive_of_the_far_edges() {
        let r = rect(5, 3, 10, 1);
        assert!(contains(r, Some((5, 3))), "top-left corner is inside");
        assert!(contains(r, Some((14, 3))), "last column is inside");
        assert!(!contains(r, Some((15, 3))), "one past the width is outside");
        assert!(!contains(r, Some((5, 4))), "one past the height is outside");
        assert!(!contains(r, Some((4, 3))), "one before the left is outside");
    }

    #[test]
    fn contains_is_false_without_a_pointer() {
        assert!(!contains(rect(0, 0, 4, 4), None));
    }

    #[test]
    fn row_hover_bg_is_theme_aware_and_only_fires_over_the_rect() {
        let r = rect(0, 0, 8, 1);
        assert_eq!(
            row_hover_bg(r, Some((2, 0)), crate::theme::Theme::DARK_BLUE),
            Some(ROW_HOVER_BG_DARK)
        );
        assert_eq!(
            row_hover_bg(r, Some((2, 0)), crate::theme::Theme::BLACK),
            Some(ROW_HOVER_BG_BLACK)
        );
        assert_eq!(
            row_hover_bg(r, Some((2, 5)), crate::theme::Theme::DARK_BLUE),
            None
        );
        assert_eq!(row_hover_bg(r, None, crate::theme::Theme::BLACK), None);
    }
}
