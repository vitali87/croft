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

/// VS Code's default dark terminal ANSI palette (the `terminal.ansi*` colors
/// of the Dark+ theme): black, red, green, yellow, blue, magenta, cyan,
/// white, then the bright variants. The base for every croft theme that does
/// not override it with a 16-entry `ansi` array in its `[[themes]]` manifest
/// block, so terminal panes render the same on every host terminal instead
/// of inheriting whatever palette the host happens to use.
pub const VSCODE_ANSI: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x31, 0x31),
    (0x0d, 0xbc, 0x79),
    (0xe5, 0xe5, 0x10),
    (0x24, 0x72, 0xc8),
    (0xbc, 0x3f, 0xbc),
    (0x11, 0xa8, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x66, 0x66, 0x66),
    (0xf1, 0x4c, 0x4c),
    (0x23, 0xd1, 0x8b),
    (0xf5, 0xf5, 0x43),
    (0x3b, 0x8e, 0xea),
    (0xd6, 0x70, 0xd6),
    (0x29, 0xb8, 0xdb),
    (0xe5, 0xe5, 0xe5),
];

/// The eight syntax-highlight colors a theme paints code with, shared by the
/// tree-sitter highlighter and the LSP semantic-token overlay (both funnel
/// through `highlight::style_for_name`). Grouped by role the way editor themes
/// publish their palettes: comment, keyword, string, constant (numbers /
/// booleans / consts / parameters), function (calls / methods / properties),
/// type (types / namespaces), tag (attributes / JSX tags / builtins), and the
/// default foreground for plain identifiers, operators, and punctuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxPalette {
    pub comment: (u8, u8, u8),
    pub keyword: (u8, u8, u8),
    pub string: (u8, u8, u8),
    pub constant: (u8, u8, u8),
    pub function: (u8, u8, u8),
    pub type_: (u8, u8, u8),
    pub tag: (u8, u8, u8),
    pub fg: (u8, u8, u8),
}

impl SyntaxPalette {
    /// The historical Base16-Ocean-Dark palette, inlined from the old hardcoded
    /// `style_for_name` arms. The default for Croft Black / Croft Dark (Blue)
    /// and for any theme that omits per-color `syn_*` fields, so no theme is
    /// ever code-colorless and the built-ins render byte-for-byte as before.
    pub const BASE16: SyntaxPalette = SyntaxPalette {
        comment: (0x65, 0x73, 0x7e),
        keyword: (0xb4, 0x8e, 0xad),
        string: (0xa3, 0xbe, 0x8c),
        constant: (0xd0, 0x87, 0x70),
        function: (0x8f, 0xa1, 0xb3),
        type_: (0xeb, 0xcb, 0x8b),
        tag: (0xbf, 0x61, 0x6a),
        fg: (0xc0, 0xc5, 0xce),
    };
}

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
    /// The 16 ANSI terminal colors (black..bright white) panes render with.
    ansi: [(u8, u8, u8); 16],
    /// The code-highlight palette (defaults to [`SyntaxPalette::BASE16`]).
    syntax: SyntaxPalette,
    /// Tab-strip chrome (the editor tab bar). Explicit for the two built-ins
    /// (byte-for-byte the historical constants); derived from the palette in
    /// [`Theme::from_decl`] for themes that don't declare them.
    tab_strip: (u8, u8, u8),
    tab_inactive: (u8, u8, u8),
    tab_active: (u8, u8, u8),
    tab_hover: (u8, u8, u8),
    tab_pill: (u8, u8, u8),
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
        ansi: VSCODE_ANSI,
        syntax: SyntaxPalette::BASE16,
        tab_strip: (0x1f, 0x24, 0x36),
        tab_inactive: (0x2a, 0x2f, 0x3e),
        // The teal selection fill (gradient::POPUP_SEL_BG) as the active chip,
        // with the brighter teal hover pill; both inlined from the old
        // brand-branch constants in src/widgets/editor.rs.
        tab_active: (0x26, 0x4f, 0x4a),
        tab_hover: (0x2f, 0x35, 0x50),
        tab_pill: (0x3c, 0x8a, 0x7e),
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
        ansi: VSCODE_ANSI,
        syntax: SyntaxPalette::BASE16,
        // The historical navy chip look.
        tab_strip: (0x1f, 0x24, 0x36),
        tab_inactive: (0x2a, 0x2f, 0x3e),
        tab_active: (0x1e, 0x3a, 0x6e),
        tab_hover: (0x34, 0x50, 0x7f),
        tab_pill: (0x4e, 0x9a, 0xff),
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
        let accent = parse_hex(&d.accent);
        let selection = parse_hex(&d.selection);
        let search = parse_hex(&d.search);
        // Tab-strip chrome defaults, for themes that declare no explicit tab
        // colors: the strip wears the theme's secondary panel fill, an
        // inactive tab lifts a hair off it, the active tab wears the
        // selection fill, hover tints the strip toward the accent, and the
        // close pill is the accent itself.
        let tab_strip = hex_or(&d.tab_strip, search);
        // An inactive tab lifts off the strip toward white on a dark strip
        // and toward black on a light one, so light manifests that omit
        // tab colors still get visible tabs.
        let tab_lift = if luma(tab_strip) > 128.0 {
            (0x00, 0x00, 0x00)
        } else {
            (0xff, 0xff, 0xff)
        };
        Theme {
            id: intern(&d.id),
            label: intern(&d.label),
            bg: parse_hex(&d.background),
            accent,
            selection,
            search,
            button: parse_hex(&d.button),
            tab_strip,
            tab_inactive: hex_or(&d.tab_inactive, blend(tab_lift, 0.06, tab_strip)),
            tab_active: hex_or(&d.tab_active, selection),
            tab_hover: hex_or(&d.tab_hover, blend(accent, 0.25, tab_strip)),
            tab_pill: hex_or(&d.tab_close_pill, accent),
            gradient: d.gradient,
            osk_key: parse_hex(&d.osk_key),
            osk_special: parse_hex(&d.osk_special),
            osk_armed: parse_hex(&d.osk_armed),
            ansi: if d.ansi.len() == 16 {
                let mut p = VSCODE_ANSI;
                for (slot, hex) in p.iter_mut().zip(&d.ansi) {
                    *slot = parse_hex(hex);
                }
                p
            } else {
                VSCODE_ANSI
            },
            syntax: SyntaxPalette {
                // Each syn_* is optional: an empty string keeps the Base16
                // default for that role, so a theme can override just a few.
                comment: hex_or(&d.syn_comment, SyntaxPalette::BASE16.comment),
                keyword: hex_or(&d.syn_keyword, SyntaxPalette::BASE16.keyword),
                string: hex_or(&d.syn_string, SyntaxPalette::BASE16.string),
                constant: hex_or(&d.syn_constant, SyntaxPalette::BASE16.constant),
                function: hex_or(&d.syn_function, SyntaxPalette::BASE16.function),
                type_: hex_or(&d.syn_type, SyntaxPalette::BASE16.type_),
                tag: hex_or(&d.syn_tag, SyntaxPalette::BASE16.tag),
                fg: hex_or(&d.syn_fg, SyntaxPalette::BASE16.fg),
            },
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

    /// True when the editor background is light (luma above the midpoint).
    /// The manifest-driven switch every light-vs-dark derivation branches
    /// on, so a third-party light theme inherits the same treatment as the
    /// built-in Croft Light.
    pub fn is_light(self) -> bool {
        luma(self.bg) > 128.0
    }

    /// Adapt a hardcoded dark-chrome color to the active theme. On every
    /// dark theme this is the identity, so wrapping a legacy constant in
    /// `theme.ui(..)` is byte-for-byte invisible there. On a light
    /// background the recurring chrome vocabulary (the historical dark
    /// constants that predate theme-driven popups) resolves to its VS Code
    /// Light Modern counterpart, and anything unmapped falls through to a
    /// luminance flip that keeps the hue but lands legibly on white.
    ///
    /// Call sites keep the historical constant in place (`theme.ui(
    /// Color::Rgb(0x16, 0x18, 0x1f))`), which documents the dark rendering
    /// and concentrates all light-theme knowledge here.
    pub fn ui(self, dark: Color) -> Color {
        if !self.is_light() {
            return dark;
        }
        let Color::Rgb(r, g, b) = dark else {
            // The named ANSI colors that read on dark but wash out on white
            // land on their VS Code light-terminal counterparts; everything
            // else (Reset, the legible named colors, indexed) passes through.
            return match dark {
                Color::White => Color::Rgb(0x1f, 0x1f, 0x1f),
                Color::Yellow | Color::LightYellow => Color::Rgb(0xbf, 0x88, 0x03),
                Color::Green | Color::LightGreen => Color::Rgb(0x10, 0x7c, 0x10),
                Color::Cyan | Color::LightCyan => Color::Rgb(0x05, 0x98, 0xbc),
                Color::Gray => Color::Rgb(0x61, 0x61, 0x61),
                other => other,
            };
        };
        let mapped = match (r, g, b) {
            // -- fills -----------------------------------------------------
            // Popup / widget body -> quickInput.background.
            (0x16, 0x18, 0x1f) => (0xf8, 0xf8, 0xf8),
            (0x1e, 0x1e, 0x1e) => (0xf8, 0xf8, 0xf8),
            // Panel and input fills, darkest first.
            (0x14, 0x14, 0x14) => (0xec, 0xec, 0xec),
            (0x1e, 0x21, 0x2a) => (0xf3, 0xf3, 0xf3),
            (0x1f, 0x24, 0x36) => (0xf3, 0xf3, 0xf3),
            (0x20, 0x24, 0x2b) => (0xe8, 0xe8, 0xe8),
            (0x23, 0x27, 0x2f) => (0xec, 0xec, 0xec),
            (0x2a, 0x2f, 0x3e) => (0xec, 0xec, 0xec),
            (0x2b, 0x31, 0x42) => (0xe0, 0xe0, 0xe0),
            (0x2c, 0x31, 0x40) => (0xe4, 0xe4, 0xe4),
            (0x2f, 0x35, 0x50) => (0xe4, 0xe4, 0xe4),
            (0x34, 0x50, 0x7f) => (0xd0, 0xd0, 0xd0),
            (0x3a, 0x40, 0x52) => (0xd8, 0xd8, 0xd8),
            // Selected list row -> list.activeSelectionBackground.
            (0x1e, 0x3a, 0x6e) => (0xe8, 0xe8, 0xe8),
            // Editor text selection blues -> editor.selectionBackground.
            (0x26, 0x4f, 0x78) => (0xad, 0xd6, 0xff),
            (0x09, 0x4d, 0x77) => (0xad, 0xd6, 0xff),
            (0x07, 0x33, 0x55) => (0xd0, 0xe6, 0xff),
            (0x37, 0x61, 0x8e) => (0xc8, 0xdf, 0xf5),
            // Separators and borders -> widget.border.
            (0x3b, 0x42, 0x52) => (0xe5, 0xe5, 0xe5),
            (0x60, 0x68, 0x78) => (0xc8, 0xc8, 0xc8),
            // -- foregrounds ----------------------------------------------
            (0xff, 0xff, 0xff) => (0x1f, 0x1f, 0x1f),
            (0xec, 0xef, 0xf4) => (0x3b, 0x3b, 0x3b),
            (0xe8, 0xee, 0xf8) => (0x3b, 0x3b, 0x3b),
            (0xe5, 0xe9, 0xf0) => (0x3b, 0x3b, 0x3b),
            (0xcc, 0xcc, 0xcc) => (0x3b, 0x3b, 0x3b),
            (0xd8, 0xde, 0xe9) => (0x42, 0x42, 0x42),
            (0xc5, 0xcd, 0xd9) => (0x50, 0x50, 0x50),
            (0xb4, 0xbe, 0xc8) => (0x50, 0x50, 0x50),
            (0xb0, 0xb8, 0xc8) => (0x50, 0x50, 0x50),
            (0x9d, 0xa5, 0xb4) => (0x61, 0x61, 0x61),
            (0x9a, 0xa4, 0xb2) => (0x61, 0x61, 0x61),
            (0x8e, 0x95, 0xa4) => (0x71, 0x71, 0x71),
            (0x8b, 0x93, 0xa1) => (0x71, 0x71, 0x71),
            (0x7a, 0x82, 0x90) => (0x71, 0x71, 0x71),
            (0xa0, 0xb4, 0xd8) => (0x71, 0x71, 0x71),
            (0x80, 0x88, 0x98) => (0x8e, 0x8e, 0x8e),
            (0x6c, 0x76, 0x86) => (0x8e, 0x8e, 0x8e),
            (0x6c, 0x7d, 0x9c) => (0x8e, 0x8e, 0x8e),
            // Accents: the two recurring blues -> focusBorder.
            (0x4e, 0x9a, 0xff) => (0x00, 0x5f, 0xb8),
            (0x88, 0xc0, 0xd0) => (0x00, 0x5f, 0xb8),
            // -- status hues (dark-tuned pastels -> saturated-on-white) ----
            (0xff, 0xd7, 0x4a) => (0xbf, 0x88, 0x03),
            (0xe5, 0xc0, 0x7b) => (0xbf, 0x88, 0x03),
            (0xeb, 0xcb, 0x8b) => (0xbf, 0x88, 0x03),
            (0xcc, 0xa7, 0x00) => (0xbf, 0x88, 0x03),
            (0xff, 0xa5, 0x00) => (0xc0, 0x6c, 0x00),
            (0xff, 0x9d, 0x2f) => (0xc0, 0x6c, 0x00),
            (0xe0, 0x9a, 0x4e) => (0xc0, 0x6c, 0x00),
            (0xa3, 0xbe, 0x8c) => (0x10, 0x7c, 0x10),
            (0xb6, 0xee, 0xc4) => (0x10, 0x7c, 0x10),
            (0x8c, 0xc2, 0x65) => (0x10, 0x7c, 0x10),
            (0xe7, 0x70, 0x70) => (0xc4, 0x2b, 0x1f),
            (0xf1, 0x4c, 0x4c) => (0xcd, 0x31, 0x31),
            // Fills that carry BLACK text (the editor block cursor, the
            // active search-match band): they must stay light so the black
            // glyph keeps its contrast, not be darkened as if they were
            // foregrounds (review round 1).
            (0xae, 0xc6, 0xff) => (0xad, 0xd6, 0xff),
            (0xff, 0x8c, 0x2a) => (0xff, 0x8c, 0x2a),
            other => light_fallback(other),
        };
        rgb(mapped)
    }

    /// The 16 ANSI terminal colors (black..bright white) the theme's panes
    /// render Named/Indexed 0-15 cell colors with.
    pub fn ansi(self) -> [(u8, u8, u8); 16] {
        self.ansi
    }

    /// The code-highlight palette. The app pushes this into the highlighter
    /// (`highlight::set_syntax_palette`) on every theme switch so tree-sitter
    /// and semantic-token colors follow the active theme.
    pub fn syntax(self) -> SyntaxPalette {
        self.syntax
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

    /// The text color that keeps contrast ON an accent-filled cell (the
    /// hex/sheet/archive cursors paint their glyph over `accent()`). Every
    /// built-in dark theme has a light accent, so black text is unchanged
    /// there; a dark accent (Croft Light's #005fb8) flips to white.
    pub fn accent_contrast_fg(self) -> Color {
        if luma(self.accent) < 128.0 {
            Color::White
        } else {
            Color::Black
        }
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

    /// Depth-cycled foregrounds for bracket-pair colorization (VS Code
    /// `editorBracketHighlight.foreground1..3`). The dark defaults are VS
    /// Code's gold / orchid / blue; a light-background theme gets VS Code
    /// Light's blue / green / brown, picked by the background's luminance so
    /// manifest themes inherit a legible set either way.
    pub fn bracket_pair_color(self, depth: usize) -> Color {
        let cycle: [(u8, u8, u8); 3] = if self.is_light() {
            [(0x04, 0x31, 0xfa), (0x31, 0x93, 0x31), (0x7b, 0x38, 0x14)]
        } else {
            [(0xff, 0xd7, 0x00), (0xda, 0x70, 0xd6), (0x17, 0x9f, 0xff)]
        };
        let (cr, cg, cb) = cycle[depth % cycle.len()];
        Color::Rgb(cr, cg, cb)
    }

    /// Foreground of an unmatched closing bracket (VS Code
    /// `editorBracketHighlight.unexpectedBracket.foreground`).
    pub fn bracket_unexpected_fg(self) -> Color {
        Color::Rgb(0xff, 0x12, 0x12)
    }

    /// Foreground of rendered whitespace glyphs (VS Code
    /// `editorWhitespace.foreground`): dimmer than the indent guides so an
    /// "all" render stays readable, blended over each theme's background.
    pub fn whitespace_fg(self) -> Color {
        self.blend_over_bg((0x96, 0x96, 0x96), 0.30)
    }

    /// Foreground of an indentation guide (VS Code
    /// `editorIndentGuide.background1`, #404040 on the dark default): a grey
    /// blended over each theme's background so it stays a whisper above the
    /// editor surface on dark and light themes alike.
    pub fn indent_guide(self) -> Color {
        self.blend_over_bg((0x88, 0x88, 0x88), 0.38)
    }

    /// Foreground of the active indentation guide — the guide of the block
    /// containing the cursor (VS Code `editorIndentGuide.activeBackground1`,
    /// #707070 on the dark default). Same grey, blended stronger.
    pub fn indent_guide_active(self) -> Color {
        self.blend_over_bg((0x88, 0x88, 0x88), 0.80)
    }

    /// Background tint under read occurrences of the symbol at the caret
    /// (VS Code `editor.wordHighlightBackground`, #575757 at 72% alpha on
    /// dark; a 25% wash on light so black text stays legible, matching VS
    /// Code Light's #57575740), blended over each theme's background.
    pub fn occurrence_bg(self) -> Color {
        let alpha = if self.is_light() { 0.25 } else { 0.72 };
        self.blend_over_bg((0x57, 0x57, 0x57), alpha)
    }

    /// Background tint under write occurrences of the symbol at the caret
    /// (VS Code `editor.wordHighlightStrongBackground`, #004972 at 72% on
    /// dark, a 25% wash on light): the assignment site reads stronger than
    /// the uses.
    pub fn occurrence_write_bg(self) -> Color {
        let alpha = if self.is_light() { 0.25 } else { 0.72 };
        self.blend_over_bg((0x00, 0x49, 0x72), alpha)
    }

    /// Background of the pinned sticky-scroll header rows (VS Code
    /// `editorStickyScroll.background`): the editor background lifted a hair
    /// so the pinned scope headers read as a distinct band above the content
    /// (toward white on dark themes, toward black on light ones).
    pub fn sticky_scroll_bg(self) -> Color {
        let lift = if self.is_light() {
            (0x00, 0x00, 0x00)
        } else {
            (0xff, 0xff, 0xff)
        };
        self.blend_over_bg(lift, 0.06)
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
    /// the Black theme and stays a legible muted grey on every dark bg; a
    /// light background blends toward black instead, landing on the same
    /// #8C8C8C for pure white rather than vanishing into it.
    pub fn ignored_fg(self) -> Color {
        if self.is_light() {
            self.blend_over_bg((0x00, 0x00, 0x00), 0.45)
        } else {
            self.blend_over_bg((0xff, 0xff, 0xff), 0.55)
        }
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

    /// The editor tab bar's background (VS Code
    /// `editorGroupHeader.tabsBackground`), including the gap right of the
    /// last tab.
    pub fn tab_strip_bg(self) -> Color {
        rgb(self.tab_strip)
    }

    /// An inactive tab's body fill.
    pub fn tab_inactive_bg(self) -> Color {
        rgb(self.tab_inactive)
    }

    /// The active tab's body fill.
    pub fn tab_active_bg(self) -> Color {
        rgb(self.tab_active)
    }

    /// An inactive tab's body while the pointer rests on it (VS Code
    /// `tab.hoverBackground`); the active tab is already prominent and never
    /// lifts.
    pub fn tab_hover_bg(self) -> Color {
        rgb(self.tab_hover)
    }

    /// The pill behind the close cross / pin glyph while the pointer is on
    /// that cell (VS Code `toolbar.hoverBackground`). Must stay distinct from
    /// [`Theme::tab_active_bg`] or the hover is invisible on the active tab.
    pub fn tab_close_pill_bg(self) -> Color {
        rgb(self.tab_pill)
    }

    /// [`Theme::ui`] over raw sRGB bytes, for the inline-image bakes that
    /// composite pixels instead of styling cells.
    fn ui_rgb(self, dark: (u8, u8, u8)) -> (u8, u8, u8) {
        match self.ui(rgb(dark)) {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => dark,
        }
    }

    /// Resting ink of an unselected activity-bar icon (VS Code
    /// `activityBar.inactiveForeground`, #616161 on Light Modern).
    pub fn activity_icon_inactive_rgb(self) -> (u8, u8, u8) {
        self.ui_rgb((0x9d, 0xa5, 0xb4))
    }

    /// Ink of the selected activity-bar icon, and of any icon under the
    /// pointer (VS Code `activityBar.foreground`, #1f1f1f on Light Modern).
    /// The dark chrome paints these white, which is exactly why the baked
    /// icons cannot keep a hardcoded tint: on a light bar white ink is
    /// invisible (issue #225).
    pub fn activity_icon_active_rgb(self) -> (u8, u8, u8) {
        self.ui_rgb((0xff, 0xff, 0xff))
    }

    /// The selection bar down the left edge of the selected icon (VS Code
    /// `activityBar.activeBorder`, #005fb8 on Light Modern).
    pub fn activity_icon_pill_rgb(self) -> (u8, u8, u8) {
        self.ui_rgb((0x4e, 0x9a, 0xff))
    }

    /// [`Theme::activity_icon_inactive_rgb`] as a cell color, for the glyph
    /// fallback the image-less terminals render. Both paths draw the same
    /// bar, so they read their ink from the same place.
    pub fn activity_icon_inactive(self) -> Color {
        rgb(self.activity_icon_inactive_rgb())
    }

    /// [`Theme::activity_icon_active_rgb`] as a cell color.
    pub fn activity_icon_active(self) -> Color {
        rgb(self.activity_icon_active_rgb())
    }

    /// [`Theme::activity_icon_pill_rgb`] as a cell color.
    pub fn activity_icon_pill(self) -> Color {
        rgb(self.activity_icon_pill_rgb())
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

/// Perceived luminance (0..=255) of an sRGB tuple.
fn luma((r, g, b): (u8, u8, u8)) -> f32 {
    0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
}

/// The [`Theme::ui`] fallback for colors outside the mapped vocabulary: a
/// light foreground scales toward black (keeping hue) until it reads on
/// white; a dark fill blends to near-white with a whisper of its hue left.
fn light_fallback(c: (u8, u8, u8)) -> (u8, u8, u8) {
    let l = luma(c);
    if l >= 128.0 {
        let k = 80.0 / l;
        let scale = |v: u8| (f32::from(v) * k).round().min(255.0) as u8;
        (scale(c.0), scale(c.1), scale(c.2))
    } else {
        blend(c, 0.12, (0xf5, 0xf5, 0xf5))
    }
}

/// Alpha-composite `fg` over `bg` at `alpha`, as raw bytes. The tuple-level
/// sibling of [`Theme::blend_over_bg`], for derivations at parse time.
fn blend(fg: (u8, u8, u8), alpha: f32, bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let mix = |f: u8, b: u8| (f as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
    (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
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

/// Parse an optional `#rrggbb`, falling back to `default` when the string is
/// empty (the field was omitted in the manifest) — lets a theme override only
/// the syntax colors it cares about and inherit Base16 for the rest.
fn hex_or(s: &str, default: (u8, u8, u8)) -> (u8, u8, u8) {
    if s.is_empty() { default } else { parse_hex(s) }
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
    fn built_in_tab_chrome_is_byte_identical_to_the_legacy_constants() {
        // The tab strip's colors were hardcoded constants in
        // src/widgets/editor.rs before they became theme data; the two
        // built-ins must keep rendering exactly as before the move.
        let b = Theme::BLACK;
        assert_eq!(b.tab_strip_bg(), Color::Rgb(0x1f, 0x24, 0x36));
        assert_eq!(b.tab_inactive_bg(), Color::Rgb(0x2a, 0x2f, 0x3e));
        assert_eq!(b.tab_active_bg(), Color::Rgb(0x26, 0x4f, 0x4a));
        assert_eq!(b.tab_hover_bg(), Color::Rgb(0x2f, 0x35, 0x50));
        assert_eq!(b.tab_close_pill_bg(), Color::Rgb(0x3c, 0x8a, 0x7e));
        let d = Theme::DARK_BLUE;
        assert_eq!(d.tab_strip_bg(), Color::Rgb(0x1f, 0x24, 0x36));
        assert_eq!(d.tab_inactive_bg(), Color::Rgb(0x2a, 0x2f, 0x3e));
        assert_eq!(d.tab_active_bg(), Color::Rgb(0x1e, 0x3a, 0x6e));
        assert_eq!(d.tab_hover_bg(), Color::Rgb(0x34, 0x50, 0x7f));
        assert_eq!(d.tab_close_pill_bg(), Color::Rgb(0x4e, 0x9a, 0xff));
    }

    #[test]
    fn manifest_themes_without_tab_colors_derive_them_from_the_palette() {
        // A theme that declares no tab_* fields still gets full tab chrome:
        // strip from its secondary panel fill, active tab from its selection,
        // close pill from its accent. No theme can fall back to another
        // theme's (navy) chrome.
        let s = Theme::from_id("solarized-dark");
        assert_eq!(s.id(), "solarized-dark");
        assert_eq!(s.tab_strip_bg(), s.search_bg());
        assert_eq!(s.tab_active_bg(), s.selection());
        assert_eq!(s.tab_close_pill_bg(), s.accent());
        assert_ne!(
            s.tab_strip_bg(),
            Theme::DARK_BLUE.tab_strip_bg(),
            "solarized must not wear the built-ins' navy strip"
        );
    }

    #[test]
    fn registry_lists_both_first_party_themes() {
        let ids: Vec<&str> = Theme::all().iter().map(|t| t.id()).collect();
        assert!(ids.contains(&"black"));
        assert!(ids.contains(&"dark-blue"));
    }

    #[test]
    fn editor_inspired_themes_all_load() {
        // Every bundled [[themes]] block must parse (a bad hex or missing field
        // silently drops the theme via `.ok()`), so pin the full roster.
        let ids: Vec<&str> = Theme::all().iter().map(|t| t.id()).collect();
        for id in [
            "one-dark-pro",
            "dracula",
            "monokai",
            "nord",
            "gruvbox-dark",
            "tokyo-night",
            "catppuccin-mocha",
            "solarized-dark",
            "github-dark",
            "darcula",
        ] {
            assert!(ids.contains(&id), "theme `{id}` missing from registry");
        }
        // These are imports of external editor palettes, not croft's own brand:
        // they must use the flat-accent look, never the teal gradient chrome.
        for t in Theme::all() {
            if t.id() != "black" {
                assert!(!t.gradient(), "`{}` must not use gradient chrome", t.id());
            }
        }
    }

    #[test]
    fn imported_themes_carry_their_own_syntax_palette() {
        // The whole point of the fix: switching theme must recolor code. Pin a
        // couple of signature token colors so a regression to Base16 is caught.
        let one_dark = Theme::from_id("one-dark-pro");
        assert_eq!(one_dark.syntax().keyword, (0xc6, 0x78, 0xdd)); // purple
        assert_ne!(one_dark.syntax(), SyntaxPalette::BASE16);

        let dracula = Theme::from_id("dracula");
        assert_eq!(dracula.syntax().string, (0xf1, 0xfa, 0x8c)); // yellow
        assert_ne!(dracula.syntax().keyword, one_dark.syntax().keyword);

        // Built-ins keep the historical Base16 code colors (no regression).
        assert_eq!(Theme::BLACK.syntax(), SyntaxPalette::BASE16);
        assert_eq!(Theme::DARK_BLUE.syntax(), SyntaxPalette::BASE16);
    }

    #[test]
    fn imported_themes_carry_their_own_ansi_terminal_palette() {
        // The terminal palette must follow the theme too, not fall back to the
        // shared VS Code default. Pin a couple of signature ANSI slots.
        let dracula = Theme::from_id("dracula").ansi();
        assert_eq!(dracula[5], (0xff, 0x79, 0xc6)); // magenta = Dracula pink
        assert_ne!(dracula, VSCODE_ANSI);

        let gruvbox = Theme::from_id("gruvbox-dark").ansi();
        assert_eq!(gruvbox[1], (0xcc, 0x24, 0x1d)); // red
        assert_ne!(gruvbox, dracula);

        // Built-ins keep the VS Code default ANSI (no regression).
        assert_eq!(Theme::BLACK.ansi(), VSCODE_ANSI);
    }

    #[test]
    fn omitted_syntax_fields_fall_back_to_base16() {
        assert_eq!(hex_or("", (1, 2, 3)), (1, 2, 3));
        assert_eq!(hex_or("#0a0b0c", (1, 2, 3)), (0x0a, 0x0b, 0x0c));
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

    #[test]
    fn croft_light_ships_the_verified_vscode_light_palette() {
        let l = Theme::from_id("light");
        assert_eq!(l.label(), "Croft Light");
        assert!(l.is_light(), "white background must classify as light");
        assert_eq!(l.editor_bg_rgb(), (0xff, 0xff, 0xff));
        assert_eq!(l.accent_rgb(), (0x00, 0x5f, 0xb8));
        assert!(!l.gradient(), "the teal brand gradient is dark chrome");
        // VS Code Light+ code colors: blue keywords, red strings, green
        // comments, the Light Modern editor foreground.
        assert_eq!(l.syntax().keyword, (0x00, 0x00, 0xff));
        assert_eq!(l.syntax().string, (0xa3, 0x15, 0x15));
        assert_eq!(l.syntax().comment, (0x00, 0x80, 0x00));
        assert_eq!(l.syntax().fg, (0x3b, 0x3b, 0x3b));
        // VS Code's light terminal ANSI palette: the dark green and the
        // grey "white" slots are its signatures.
        assert_eq!(l.ansi()[2], (0x10, 0x7c, 0x10));
        assert_eq!(l.ansi()[7], (0x55, 0x55, 0x55));
        // Tab chrome: the white active tab lifting off the #f8f8f8 strip.
        assert_eq!(l.tab_strip_bg(), Color::Rgb(0xf8, 0xf8, 0xf8));
        assert_eq!(l.tab_active_bg(), Color::Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn only_the_light_theme_classifies_as_light() {
        for t in Theme::all() {
            assert_eq!(
                t.is_light(),
                t.id() == "light",
                "`{}` misclassified by is_light",
                t.id()
            );
        }
    }

    #[test]
    fn the_ui_adapter_is_the_identity_on_every_dark_theme() {
        // The whole sweep's safety net: wrapping a hardcoded chrome color in
        // `theme.ui(..)` must be byte-for-byte invisible on dark themes.
        for t in Theme::all() {
            if t.is_light() {
                continue;
            }
            for c in [
                Color::Rgb(0x16, 0x18, 0x1f),
                Color::Rgb(0xec, 0xef, 0xf4),
                Color::Rgb(0x12, 0x34, 0x56),
                Color::White,
                Color::Reset,
            ] {
                assert_eq!(t.ui(c), c, "`{}` must pass {c:?} through", t.id());
            }
        }
    }

    #[test]
    fn the_ui_adapter_maps_the_dark_chrome_vocabulary_onto_light() {
        let l = Theme::from_id("light");
        // The recurring popup constants land on their Light Modern versions.
        assert_eq!(
            l.ui(Color::Rgb(0x16, 0x18, 0x1f)),
            Color::Rgb(0xf8, 0xf8, 0xf8),
            "popup fill -> quickInput.background"
        );
        assert_eq!(
            l.ui(Color::Rgb(0xec, 0xef, 0xf4)),
            Color::Rgb(0x3b, 0x3b, 0x3b),
            "primary text -> Light Modern foreground"
        );
        assert_eq!(
            l.ui(Color::Rgb(0x4e, 0x9a, 0xff)),
            Color::Rgb(0x00, 0x5f, 0xb8),
            "accent blue -> focusBorder"
        );
        assert_eq!(
            l.ui(Color::Rgb(0x1e, 0x3a, 0x6e)),
            Color::Rgb(0xe8, 0xe8, 0xe8),
            "selected row -> list.activeSelectionBackground"
        );
        assert_eq!(
            l.ui(Color::White),
            Color::Rgb(0x1f, 0x1f, 0x1f),
            "white popup titles go near-black"
        );
        let luma = |c: Color| {
            let Color::Rgb(r, g, b) = c else {
                panic!("adapter must return Rgb, got {c:?}")
            };
            0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
        };
        // Unmapped colors still land legibly: a light foreground darkens,
        // a dark fill lightens to near-white.
        assert!(luma(l.ui(Color::Rgb(0xab, 0xcd, 0xef))) < 128.0);
        assert!(luma(l.ui(Color::Rgb(0x10, 0x20, 0x30))) > 200.0);
        // Fills that carry black text (block cursor, active search match)
        // must stay LIGHT rather than be darkened as foregrounds.
        assert!(luma(l.ui(Color::Rgb(0xae, 0xc6, 0xff))) > 150.0);
        assert!(luma(l.ui(Color::Rgb(0xff, 0x8c, 0x2a))) > 150.0);
    }

    #[test]
    fn accent_contrast_fg_flips_only_on_dark_accents() {
        // Built-in dark themes have light accents: black text, unchanged.
        assert_eq!(Theme::BLACK.accent_contrast_fg(), Color::Black);
        assert_eq!(Theme::DARK_BLUE.accent_contrast_fg(), Color::Black);
        // Croft Light's #005fb8 accent is dark: white text keeps contrast.
        assert_eq!(Theme::from_id("light").accent_contrast_fg(), Color::White);
    }

    #[test]
    fn derived_blends_flip_direction_on_the_light_background() {
        let l = Theme::from_id("light");
        let luma = |c: Color| {
            let Color::Rgb(r, g, b) = c else {
                panic!("expected Rgb, got {c:?}")
            };
            0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
        };
        // The white-blend helpers must darken on white, not brighten into
        // invisibility: ignored files a readable grey, sticky headers a
        // shade below the editor surface, occurrence tints a light wash
        // that keeps black text legible.
        assert!(
            luma(l.ignored_fg()) < 200.0,
            "ignored files must stay visible"
        );
        assert!(luma(l.sticky_scroll_bg()) < 255.0 - 1.0);
        assert!(
            luma(l.occurrence_bg()) > 180.0,
            "occurrence tint must stay a wash"
        );
        // Dark themes keep their historical values.
        assert_eq!(
            Theme::BLACK.ignored_fg(),
            Color::Rgb(0x8c, 0x8c, 0x8c),
            "black ignored grey unchanged"
        );
    }

    /// Issue #225: the activity-bar icons are baked as inline images with a
    /// tint chosen away from the render call site, so the palette has to come
    /// from the theme. On a light bar the dark chrome's white ink would paint
    /// the selected icon invisible.
    #[test]
    fn light_activity_icon_ink_is_vs_code_light_modern() {
        let l = Theme::from_id("light");
        assert_eq!(
            l.activity_icon_active_rgb(),
            (0x1f, 0x1f, 0x1f),
            "selected / hovered icon = activityBar.foreground"
        );
        assert_eq!(
            l.activity_icon_inactive_rgb(),
            (0x61, 0x61, 0x61),
            "resting icon = activityBar.inactiveForeground"
        );
        assert_eq!(
            l.activity_icon_pill_rgb(),
            (0x00, 0x5f, 0xb8),
            "selection bar = activityBar.activeBorder"
        );
        // Every ink must read against the bar it is painted on, and the
        // selected icon must be darker than the resting ones so selection
        // survives without relying on the pill alone.
        let bar = luma(l.editor_bg_rgb());
        for ink in [
            l.activity_icon_active_rgb(),
            l.activity_icon_inactive_rgb(),
            l.activity_icon_pill_rgb(),
        ] {
            assert!(
                bar - luma(ink) > 90.0,
                "{ink:?} must contrast against the light activity bar"
            );
        }
        assert!(
            luma(l.activity_icon_active_rgb()) < luma(l.activity_icon_inactive_rgb()),
            "the selected icon reads stronger than the resting ones"
        );
    }

    #[test]
    fn dark_activity_icon_ink_keeps_the_historical_chrome() {
        for t in Theme::all().iter().filter(|t| !t.is_light()) {
            assert_eq!(t.activity_icon_active_rgb(), (0xff, 0xff, 0xff));
            assert_eq!(t.activity_icon_inactive_rgb(), (0x9d, 0xa5, 0xb4));
            assert_eq!(t.activity_icon_pill_rgb(), (0x4e, 0x9a, 0xff));
        }
    }
}
