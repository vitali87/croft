//! Color theme for the IDE chrome.
//!
//! croft mimics VS Code, where the user picks a "Color Theme". The first-party
//! themes (Croft Black `#000000` and Croft Dark `#1e222e`) ship as data in a
//! bundled extension manifest (`assets/extensions/themes/extension.toml`, baked
//! into the binary), and a third party adds a theme by dropping another
//! `[[themes]]` block into `~/.config/croft/extensions/`. A `Theme` is just the
//! parsed palette; [`Theme::BLACK`] is a const fallback so croft is never
//! themeless even if every manifest is missing or unparseable.
//!
//! The active theme drives every surface that paints an explicit background
//! instead of inheriting the iTerm2 session background: the editor canvases,
//! the baked activity-bar icons, the welcome hero raster, and the `SetColors`
//! sequence. Surfaces that paint `Color::Reset` inherit the session background,
//! so flipping `SetColors` recolors them for free.

use std::sync::OnceLock;

use ratatui::style::Color;

/// A complete IDE color palette. `Copy` and cheap to pass by value: the strings
/// are interned to `&'static` when loaded from a manifest, and the colors are
/// plain bytes. Equality is by value, so a registry-loaded theme compares equal
/// to the matching [`Theme::BLACK`]/[`Theme::DARK_BLUE`] const.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    id: &'static str,
    label: &'static str,
    bg: (u8, u8, u8),
    accent: (u8, u8, u8),
    selection: (u8, u8, u8),
    search: (u8, u8, u8),
    button: (u8, u8, u8),
    gradient: bool,
    osk_key: (u8, u8, u8),
    osk_special: (u8, u8, u8),
    osk_armed: (u8, u8, u8),
}

impl Theme {
    /// Croft Black — the OLED-friendly default, and the const fallback used when
    /// no manifest theme is available. Values mirror the bundled manifest (the
    /// gradient brand consts from `src/gradient.rs` inlined).
    pub const BLACK: Theme = Theme {
        id: "black",
        label: "Croft Black",
        bg: (0x00, 0x00, 0x00),
        accent: (0x4f, 0xb1, 0xa6),
        selection: (0x26, 0x4f, 0x4a),
        search: (0x14, 0x14, 0x14),
        button: (0x0e, 0x7e, 0x76),
        gradient: true,
        osk_key: (0x20, 0x24, 0x2b),
        osk_special: (0x14, 0x16, 0x1b),
        osk_armed: (0x0e, 0x7e, 0x76),
    };

    /// Croft Dark (Blue) — the historical look. Const mirror of the manifest,
    /// for call sites and tests that name it directly.
    pub const DARK_BLUE: Theme = Theme {
        id: "dark-blue",
        label: "Croft Dark (Blue)",
        bg: (0x1e, 0x22, 0x2e),
        accent: (0x4e, 0x9a, 0xff),
        selection: (0x09, 0x4d, 0x77),
        search: (0x23, 0x27, 0x2f),
        button: (0x09, 0x67, 0xb8),
        gradient: false,
        osk_key: (0x3a, 0x40, 0x52),
        osk_special: (0x2c, 0x31, 0x40),
        osk_armed: (0x00, 0x7a, 0xcc),
    };

    /// Every available theme in pick-list order, loaded once from the bundled +
    /// user extension manifests, skipping any whose contributing extension is
    /// disabled in the Extensions panel (read once at first access, so a toggle
    /// takes effect on the next launch — matching the other extension toggles).
    /// The gear menu's theme picker renders this.
    pub fn all() -> &'static [Theme] {
        static REGISTRY: OnceLock<Vec<Theme>> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let mut sources: Vec<String> = crate::lsp::manifest::BUNDLED_MANIFESTS
                .iter()
                .map(|s| s.to_string())
                .collect();
            // Under test the registry must be hermetic: reading the developer's
            // real ~/.config/croft (user extensions + disabled set) made this
            // process-wide cache depend on machine state and on which test
            // touched it first. Tests see only the bundled themes.
            let disabled = if cfg!(test) {
                std::collections::BTreeSet::new()
            } else {
                sources.extend(crate::lsp::manifest::read_extension_sources(
                    &crate::lsp::manifest::user_extensions_dir(),
                ));
                crate::prefs::Prefs::load_or_default().disabled_extensions
            };
            let themes: Vec<Theme> = sources
                .iter()
                .filter_map(|s| crate::lsp::manifest::parse(s).ok())
                .filter(|m| !disabled.contains(&m.id))
                .flat_map(|m| m.themes)
                .map(|d| Theme::from_decl(&d))
                .collect();
            // Never empty: fall back to the baked-in const so the picker and
            // `default`/`from_id` always have something even if the themes
            // extension is disabled.
            if themes.is_empty() {
                vec![Theme::BLACK]
            } else {
                themes
            }
        })
    }

    /// Build a theme from a parsed manifest entry, interning its strings and
    /// parsing its `#rrggbb` colors (a bad color degrades to black, not a crash).
    fn from_decl(d: &crate::lsp::manifest::ThemeDecl) -> Theme {
        Theme {
            id: intern(&d.id),
            label: intern(&d.label),
            bg: parse_hex(&d.background),
            accent: parse_hex(&d.accent),
            selection: parse_hex(&d.selection),
            search: parse_hex(&d.search),
            button: parse_hex(&d.button),
            gradient: d.gradient,
            osk_key: parse_hex(&d.osk_key),
            osk_special: parse_hex(&d.osk_special),
            osk_armed: parse_hex(&d.osk_armed),
        }
    }

    /// The default theme: Croft Black when present, else the first registered,
    /// else the const fallback. Used for a fresh install and as the revert
    /// target when the active theme's extension is disabled/removed.
    pub fn default_theme() -> Theme {
        let all = Theme::all();
        all.iter()
            .find(|t| t.id == "black")
            .copied()
            .or_else(|| all.first().copied())
            .unwrap_or(Theme::BLACK)
    }

    /// Resolve a persisted `id` to a theme, falling back to the default for
    /// anything unrecognized (forward/backward-compatible prefs).
    pub fn from_id(id: &str) -> Self {
        Theme::all()
            .iter()
            .find(|t| t.id == id)
            .copied()
            .unwrap_or_else(Theme::default_theme)
    }

    /// Stable on-disk identifier, persisted in the prefs file.
    pub fn id(self) -> &'static str {
        self.id
    }

    /// Human-facing label shown in the theme picker.
    pub fn label(self) -> &'static str {
        self.label
    }

    /// Whether this theme uses the gradient brand chrome (teal accents, the
    /// focused-pane gradient border, popup gradients) vs the flat-accent look.
    /// Replaces the old hardcoded `theme == Black` checks.
    pub fn gradient(self) -> bool {
        self.gradient
    }

    /// Primary accent (selected-row text, active chrome).
    pub fn accent(self) -> Color {
        rgb(self.accent)
    }

    /// Primary accent as raw sRGB bytes, baked into the activity-bar change
    /// badge's inline-image canvas (VS Code `activityBarBadge.background`).
    pub fn accent_rgb(self) -> (u8, u8, u8) {
        self.accent
    }

    /// Selected-row fill in lists/popups.
    pub fn selection(self) -> Color {
        rgb(self.selection)
    }

    /// Soft translucent accent fill behind an active toolbar control (VS Code's
    /// lit-filter highlight): the theme accent blended over the panel
    /// background so a non-default filter/toggle reads as lit without a heavy
    /// opaque box.
    pub fn accent_chip_bg(self) -> Color {
        self.blend_over_bg(self.accent, 0.18)
    }

    /// Filter/search input fill.
    pub fn search_bg(self) -> Color {
        rgb(self.search)
    }

    /// Primary-button / lit-toggle fill.
    pub fn button(self) -> Color {
        rgb(self.button)
    }

    /// On-screen-keyboard normal key cap fill.
    pub fn osk_key_bg(self) -> Color {
        rgb(self.osk_key)
    }

    /// On-screen-keyboard special key cap fill.
    pub fn osk_special_bg(self) -> Color {
        rgb(self.osk_special)
    }

    /// On-screen-keyboard armed (held) key fill.
    pub fn osk_armed_bg(self) -> Color {
        rgb(self.osk_armed)
    }

    /// The editor/panel background as raw sRGB bytes. Used both as the
    /// `SetColors` session fill and as the alpha-blend / canvas-fill target
    /// behind the baked OSC-1337 images so they merge seamlessly.
    pub fn editor_bg_rgb(self) -> (u8, u8, u8) {
        self.bg
    }

    /// The editor/panel background as a ratatui color.
    pub fn editor_bg(self) -> Color {
        rgb(self.bg)
    }

    /// The selection-highlight background as raw sRGB bytes (minimap selection
    /// band).
    pub fn selection_rgb(self) -> (u8, u8, u8) {
        self.selection
    }

    /// Alpha-composite `fg` over this theme's background at `alpha` (0.0..=1.0),
    /// returning an opaque color. Terminal cells can't render real
    /// transparency, so we pre-blend against the known per-theme background —
    /// the trick that reproduces VS Code's translucent scrollbar slider.
    fn blend_over_bg(self, fg: (u8, u8, u8), alpha: f32) -> Color {
        let (br, bgc, bb) = self.bg;
        let mix = |f: u8, b: u8| (f as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
        Color::Rgb(mix(fg.0, br), mix(fg.1, bgc), mix(fg.2, bb))
    }

    /// Background box under a matched bracket pair. VS Code paints
    /// `editorBracketMatch.border` (a subtle outline) which a terminal cell
    /// can't carry, so a faint grey fill blended over each theme's background
    /// stands in, staying legible on both dark themes.
    pub fn bracket_match_bg(self) -> Color {
        self.blend_over_bg((0x9a, 0x9a, 0x9a), 0.35)
    }

    /// Background of the pinned sticky-scroll header rows (VS Code
    /// `editorStickyScroll.background`): the editor background lifted a hair so
    /// the pinned scope headers read as a distinct band above the content.
    pub fn sticky_scroll_bg(self) -> Color {
        self.blend_over_bg((0xff, 0xff, 0xff), 0.06)
    }

    /// Git gutter bar for an added line (VS Code `editorGutter.addedBackground`,
    /// a vivid green). The add/modify/delete decorations are semantic status
    /// colours, identical across both dark themes and legible on either
    /// background, so they are fixed here rather than carried per-theme.
    pub fn git_added(self) -> Color {
        Color::Rgb(0x2e, 0xa0, 0x43)
    }

    /// Git gutter bar for a modified line (VS Code `editorGutter.modifiedBackground`).
    pub fn git_modified(self) -> Color {
        Color::Rgb(0x0c, 0x7d, 0xc4)
    }

    /// Git gutter bar marking a deletion (VS Code `editorGutter.deletedBackground`).
    pub fn git_deleted(self) -> Color {
        Color::Rgb(0xf8, 0x51, 0x49)
    }

    /// The deletion/failure red as raw sRGB bytes, baked into the Testing
    /// failing-count badge canvas. Same value as [`Theme::git_deleted`].
    pub fn git_deleted_rgb(self) -> (u8, u8, u8) {
        (0xf8, 0x51, 0x49)
    }

    /// Explorer name foreground for a git-ignored file or directory (VS Code
    /// `gitDecoration.ignoredResourceForeground`, #8C8C8C on dark themes).
    /// White at 55% over the theme background lands exactly on that grey for
    /// the Black theme and stays a legible muted grey on every dark bg.
    pub fn ignored_fg(self) -> Color {
        self.blend_over_bg((0xff, 0xff, 0xff), 0.55)
    }

    /// The scrollbar track. VS Code paints no track, so we match the editor
    /// background and the lane melts away on every theme.
    pub fn scrollbar_track(self) -> Color {
        self.editor_bg()
    }

    /// The scrollbar thumb when its pane is unfocused (VS Code's resting
    /// `scrollbarSlider.background`, #797979 at 40% alpha).
    pub fn scrollbar_thumb(self) -> Color {
        self.blend_over_bg((0x79, 0x79, 0x79), 0.40)
    }

    /// The scrollbar thumb when its pane is focused (VS Code's brighter hover
    /// value, #646464 at 70% alpha).
    pub fn scrollbar_thumb_focused(self) -> Color {
        self.blend_over_bg((0x64, 0x64, 0x64), 0.70)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::BLACK
    }
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Parse `#rrggbb` (case-insensitive); anything malformed degrades to black so
/// a typo'd theme manifest can't crash croft.
fn parse_hex(s: &str) -> (u8, u8, u8) {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() == 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&h[0..2], 16),
            u8::from_str_radix(&h[2..4], 16),
            u8::from_str_radix(&h[4..6], 16),
        )
    {
        return (r, g, b);
    }
    (0, 0, 0)
}

/// Leak a manifest string to `&'static` (bounded by the installed theme count,
/// loaded once). Same rationale as the LSP manifest interner.
fn intern(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_blue_keeps_the_historical_background() {
        assert_eq!(Theme::DARK_BLUE.editor_bg_rgb(), (0x1e, 0x22, 0x2e));
    }

    #[test]
    fn black_is_true_black() {
        assert_eq!(Theme::BLACK.editor_bg_rgb(), (0x00, 0x00, 0x00));
    }

    #[test]
    fn parse_hex_reads_rrggbb_and_degrades_to_black() {
        assert_eq!(parse_hex("#1e222e"), (0x1e, 0x22, 0x2e));
        assert_eq!(parse_hex("000000"), (0, 0, 0));
        assert_eq!(parse_hex("nonsense"), (0, 0, 0));
    }

    #[test]
    fn bundled_manifest_themes_match_the_const_palettes() {
        // The data move must be byte-for-byte: the baked-in manifest reproduces
        // the historical const palettes exactly.
        assert_eq!(
            Theme::from_id("black"),
            Theme::BLACK,
            "manifest == const BLACK"
        );
        assert_eq!(
            Theme::from_id("dark-blue"),
            Theme::DARK_BLUE,
            "manifest == const DARK_BLUE"
        );
    }

    #[test]
    fn registry_lists_both_first_party_themes() {
        let ids: Vec<&str> = Theme::all().iter().map(|t| t.id()).collect();
        assert!(ids.contains(&"black"));
        assert!(ids.contains(&"dark-blue"));
    }

    #[test]
    fn id_round_trips_through_from_id() {
        for theme in Theme::all() {
            assert_eq!(Theme::from_id(theme.id()), *theme);
        }
    }

    #[test]
    fn unknown_id_falls_back_to_default() {
        assert_eq!(Theme::from_id("chartreuse"), Theme::default_theme());
    }

    #[test]
    fn default_is_black() {
        assert_eq!(Theme::default_theme(), Theme::BLACK);
        assert_eq!(Theme::default(), Theme::BLACK);
    }

    #[test]
    fn black_uses_gradient_brand_chrome_dark_blue_does_not() {
        assert!(Theme::BLACK.gradient());
        assert!(!Theme::DARK_BLUE.gradient());
    }
}
