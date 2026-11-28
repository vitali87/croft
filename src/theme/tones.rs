//! The tones a `surfaces` theme (Croft Loch) draws its chrome from: the
//! canvas, the chrome and raised surfaces, the seams, three text inks, the
//! accent, each derived once from the theme's own colours. The theme's
//! colour getters read them, and the drawing helpers in `widgets/surface.rs`
//! paint with them.

use ratatui::style::Color;

use crate::gradient::{lerp_rgb, rgb_color};
use crate::theme::Theme;

/// The working inks and fills a `surfaces` theme draws its chrome from,
/// derived once from the theme's four surface tones, its canvas and its
/// accent, so a light surfaces theme gets the same hierarchy turned the
/// other way.
///
/// The rules they encode: three text inks (`text`, `muted`, `faint`) and a
/// `strong` one for the single active title in a region; the accent only
/// for what has focus, is selected or is active; a selected row is a
/// `band` with the [`ROW_BAR`] in the accent; a card's edge is the `rim`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tones {
    /// The code canvas (editor and panel background).
    pub canvas: (u8, u8, u8),
    /// Side bar, activity bar, tab strip, panel title row, status bar.
    pub chrome: (u8, u8, u8),
    /// Every popover: palette, pickers, menus, hovers, toasts, dialogs.
    pub raised: (u8, u8, u8),
    /// The seam between two surfaces.
    pub hairline: (u8, u8, u8),
    /// A step past the hairline: the rules inside a card and a resting
    /// field's outline, which must read against the raised body. A card
    /// has no outline of its own: its shadow does the lifting.
    pub rim: (u8, u8, u8),
    /// Body text: names, labels, values.
    pub text: (u8, u8, u8),
    /// The one emphasised label in a region: the focused view's title, the
    /// active tab.
    pub strong: (u8, u8, u8),
    /// Secondary text: inactive titles, paths, hints, idle icons.
    pub muted: (u8, u8, u8),
    /// Tertiary marks: line numbers, separators, dim chevrons.
    pub faint: (u8, u8, u8),
    /// Focus, selection and the active state, and nothing else.
    pub accent: (u8, u8, u8),
    /// White on a dark theme, black on a light one: what "lifting" a
    /// surface blends toward.
    pub lift: (u8, u8, u8),
}

impl Tones {
    /// The canvas as a cell colour.
    pub fn canvas(self) -> Color {
        rgb_color(self.canvas)
    }
    /// The chrome surface as a cell colour.
    pub fn chrome(self) -> Color {
        rgb_color(self.chrome)
    }
    /// The raised surface as a cell colour.
    pub fn raised(self) -> Color {
        rgb_color(self.raised)
    }
    /// The hairline as a cell colour.
    pub fn hairline(self) -> Color {
        rgb_color(self.hairline)
    }
    /// The card rim as a cell colour.
    pub fn rim(self) -> Color {
        rgb_color(self.rim)
    }
    /// Body text as a cell colour.
    pub fn text(self) -> Color {
        rgb_color(self.text)
    }
    /// The strong ink as a cell colour.
    pub fn strong(self) -> Color {
        rgb_color(self.strong)
    }
    /// Muted text as a cell colour.
    pub fn muted(self) -> Color {
        rgb_color(self.muted)
    }
    /// Faint marks as a cell colour.
    pub fn faint(self) -> Color {
        rgb_color(self.faint)
    }
    /// The accent as a cell colour.
    pub fn accent(self) -> Color {
        rgb_color(self.accent)
    }

    /// The selected-row fill on a surface `base`: the accent laid over it
    /// at a quarter strength, quiet enough that body text keeps its colour.
    pub fn band_on(self, base: (u8, u8, u8)) -> Color {
        rgb_color(lerp_rgb(base, self.accent, 0.24))
    }

    /// The selected-row fill on the raised surface (palette, pickers, menus).
    pub fn band(self) -> Color {
        self.band_on(self.raised)
    }

    /// A row the pointer is over, on surface `base`: a small neutral lift.
    pub fn hover_on(self, base: (u8, u8, u8)) -> Color {
        rgb_color(lerp_rgb(base, self.lift, 0.055))
    }

    /// A keycap chip's fill on a row whose fill is `row` (the band when the
    /// row is selected).
    pub fn keycap_on(self, row: Color) -> Color {
        let base = match row {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => self.raised,
        };
        rgb_color(lerp_rgb(base, self.lift, 0.085))
    }
}

/// The [`Tones`] of a `surfaces` theme, or `None` on a boxed one.
pub fn tones(theme: Theme) -> Option<Tones> {
    let s = theme.surfaces()?;
    let canvas = theme.editor_bg_rgb();
    let lift = if theme.is_light() {
        (0x00, 0x00, 0x00)
    } else {
        (0xff, 0xff, 0xff)
    };
    Some(Tones {
        canvas,
        chrome: s.chrome,
        raised: s.raised,
        hairline: s.hairline,
        rim: lerp_rgb(s.raised, lift, 0.11),
        text: lerp_rgb(s.muted, lift, 0.70),
        strong: lerp_rgb(s.muted, lift, 0.90),
        muted: s.muted,
        faint: lerp_rgb(canvas, s.muted, 0.52),
        accent: theme.accent_rgb(),
        lift,
    })
}
