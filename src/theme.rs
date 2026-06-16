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

/// The active IDE color theme. `Black` (`#000000`) is the default so a fresh
/// install opens on the OLED-friendly background; `DarkBlue` (`#1e222e`) is the
/// historical look, still selectable from the gear menu's theme picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Black,
    DarkBlue,
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

    /// Alpha-composite `fg` over this theme's background at `alpha` (0.0..=1.0),
    /// returning an opaque color. Terminal cells can't render real
    /// transparency, so we pre-blend against the *known* per-theme background.
    /// This is the trick that lets croft reproduce VS Code's translucent
    /// scrollbar slider (a single rgba value that tints whatever is behind it)
    /// with the opaque RGB a terminal can actually emit.
    fn blend_over_bg(self, fg: (u8, u8, u8), alpha: f32) -> Color {
        let (br, bgc, bb) = self.editor_bg_rgb();
        let mix = |f: u8, b: u8| (f as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
        Color::Rgb(mix(fg.0, br), mix(fg.1, bgc), mix(fg.2, bb))
    }

    /// The scrollbar track (the gutter behind the thumb). VS Code paints no
    /// track at all, the slider floats over the content, so we match the
    /// editor background and the lane melts away on every theme.
    pub fn scrollbar_track(self) -> Color {
        self.editor_bg()
    }

    /// The scrollbar thumb when its pane is unfocused. Mirrors VS Code's
    /// resting `scrollbarSlider.background` (#797979 at 40% alpha).
    pub fn scrollbar_thumb(self) -> Color {
        self.blend_over_bg((0x79, 0x79, 0x79), 0.40)
    }

    /// The scrollbar thumb when its pane is focused. croft surfaces several
    /// panes at once, so the active pane gets the more prominent slider, VS
    /// Code's brighter hover value (#646464 at 70% alpha).
    pub fn scrollbar_thumb_focused(self) -> Color {
        self.blend_over_bg((0x64, 0x64, 0x64), 0.70)
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
            "dark-blue" => Theme::DarkBlue,
            _ => Theme::default(),
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
    fn default_is_black() {
        assert_eq!(Theme::default(), Theme::Black);
    }
}
