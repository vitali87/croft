//! Color theme for the IDE chrome.
//!
//! croft mimics VS Code, where the user picks a "Color Theme". For now two
//! ship: the original dark-blue (`#1e222e`, the One-Dark-ish base croft has
//! always used) and a pure-black (`#000000`) OLED theme. The active theme
//! drives every surface that paints an explicit background instead of
//! inheriting the iTerm2 session background: the editor's image/sheet/diff
//! canvases, the baked activity-bar icon PNGs, the welcome hero raster, and
//! the `SetColors=bg=…` sequence that paints the terminal itself.
//!
//! Surfaces that paint `Color::Reset` (the normal editor body, gutters, most
//! panels) inherit the session background, so flipping the `SetColors` value
//! recolors them for free — only the explicit-fill surfaces consult the theme.

use ratatui::style::Color;

/// The active IDE color theme. `DarkBlue` is the historical default so a
/// fresh install (and every test that hard-codes `1e222e`) keeps its look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    DarkBlue,
    Black,
}

impl Theme {
    /// All themes in pick-list order. The gear menu's theme picker renders
    /// this slice, so adding a variant here surfaces it in the UI.
    pub const ALL: [Theme; 2] = [Theme::DarkBlue, Theme::Black];

    /// The editor/panel background as raw sRGB bytes. Used both as the
    /// `SetColors` session fill and as the alpha-blend / canvas-fill target
    /// behind the baked OSC-1337 images so they merge seamlessly with the
    /// surrounding panes.
    pub fn editor_bg_rgb(self) -> (u8, u8, u8) {
        match self {
            Theme::DarkBlue => (0x1e, 0x22, 0x2e),
            Theme::Black => (0x00, 0x00, 0x00),
        }
    }

    /// The editor/panel background as a ratatui color.
    pub fn editor_bg(self) -> Color {
        let (r, g, b) = self.editor_bg_rgb();
        Color::Rgb(r, g, b)
    }

    /// Stable on-disk identifier, persisted in the prefs file.
    pub fn id(self) -> &'static str {
        match self {
            Theme::DarkBlue => "dark-blue",
            Theme::Black => "black",
        }
    }

    /// Parse an `id()` back into a theme, falling back to the default for
    /// anything unrecognized (forward/backward-compatible prefs).
    pub fn from_id(id: &str) -> Self {
        match id {
            "black" => Theme::Black,
            _ => Theme::DarkBlue,
        }
    }

    /// Human-facing label shown in the theme picker.
    pub fn label(self) -> &'static str {
        match self {
            Theme::DarkBlue => "Croft Dark (Blue)",
            Theme::Black => "Croft Black",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_blue_keeps_the_historical_background() {
        assert_eq!(Theme::DarkBlue.editor_bg_rgb(), (0x1e, 0x22, 0x2e));
    }

    #[test]
    fn black_is_true_black() {
        assert_eq!(Theme::Black.editor_bg_rgb(), (0x00, 0x00, 0x00));
    }

    #[test]
    fn id_round_trips_through_from_id() {
        for theme in Theme::ALL {
            assert_eq!(Theme::from_id(theme.id()), theme);
        }
    }

    #[test]
    fn unknown_id_falls_back_to_default() {
        assert_eq!(Theme::from_id("chartreuse"), Theme::default());
    }

    #[test]
    fn default_is_dark_blue() {
        assert_eq!(Theme::default(), Theme::DarkBlue);
    }
}
