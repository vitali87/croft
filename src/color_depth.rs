//! The terminal's colour depth, and the pass that fits a frame to it
//! (#1433). Themes, syntax colours and terminal panes all paint
//! `Color::Rgb`, which ratatui writes as 24-bit SGR (`38;2;r;g;b`). A
//! terminal that only knows the 256-colour palette (Terminal.app before
//! macOS 26, `screen`, PuTTY's defaults) or the 16 ANSI colours (the Linux
//! console) draws those wrong or not at all, so below truecolor every
//! `Rgb` in the frame is mapped to the nearest colour the terminal has.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorDepth {
    /// The `color_depth` setting's value, when it names a depth.
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "truecolor" | "24bit" => Some(Self::TrueColor),
            "256" => Some(Self::Ansi256),
            "16" => Some(Self::Ansi16),
            _ => None,
        }
    }

    /// The depth the terminal advertises through its environment. Truecolor
    /// when `COLORTERM` says so, for a terminal known to draw 24-bit colour
    /// (by `TERM_PROGRAM` or `TERM`, which outlive an SSH hop where
    /// `COLORTERM` often does not), and inside tmux, which converts colours
    /// for the terminal outside it. Otherwise 256 colours for a `256color`
    /// `TERM`, and 16 for anything else.
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Self {
        let var = |name: &str| env(name).unwrap_or_default().to_ascii_lowercase();
        let colorterm = var("COLORTERM");
        if colorterm == "truecolor" || colorterm == "24bit" {
            return Self::TrueColor;
        }
        const TRUECOLOR_PROGRAMS: &[&str] = &[
            "iterm.app",
            "wezterm",
            "ghostty",
            "vscode",
            "tmux",
            "hyper",
            "tabby",
            "warpterminal",
            "rio",
        ];
        if TRUECOLOR_PROGRAMS.contains(&var("TERM_PROGRAM").as_str()) {
            return Self::TrueColor;
        }
        // Windows Terminal names itself only through this variable.
        if env("WT_SESSION").is_some_and(|s| !s.is_empty()) {
            return Self::TrueColor;
        }
        let term = var("TERM");
        const TRUECOLOR_TERMS: &[&str] = &[
            "kitty",
            "ghostty",
            "alacritty",
            "wezterm",
            "foot",
            "contour",
            "rio",
            "direct",
            "truecolor",
            "24bit",
        ];
        if TRUECOLOR_TERMS.iter().any(|t| term.contains(t)) {
            return Self::TrueColor;
        }
        if term.contains("256color") {
            return Self::Ansi256;
        }
        Self::Ansi16
    }

    /// `color` as this depth can show it. Named and indexed colours pass
    /// through, except a 256-palette index on a 16-colour terminal.
    pub fn fit(self, color: Color) -> Color {
        match (self, color) {
            (Self::TrueColor, c) => c,
            (Self::Ansi256, Color::Rgb(r, g, b)) => Color::Indexed(nearest_256(r, g, b)),
            (Self::Ansi16, Color::Rgb(r, g, b)) => nearest_16(r, g, b),
            (Self::Ansi16, Color::Indexed(i)) if i >= 16 => {
                let (r, g, b) = palette_256(i);
                nearest_16(r, g, b)
            }
            (_, c) => c,
        }
    }

    /// Fit every colour of a finished frame to this depth, before ratatui
    /// writes it out. One pass covers the chrome, syntax colours and the
    /// cells terminal panes copied from their programs.
    pub fn fit_buffer(self, buffer: &mut Buffer) {
        if self == Self::TrueColor {
            return;
        }
        for cell in &mut buffer.content {
            cell.fg = self.fit(cell.fg);
            cell.bg = self.fit(cell.bg);
            cell.underline_color = self.fit(cell.underline_color);
        }
    }
}

/// The xterm colour cube's six levels per channel.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2) as u32;
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// The index of the closest colour among the 6×6×6 cube (16-231) and the
/// grey ramp (232-255). The 16 system colours are left out: terminals
/// theme them, so their RGB is not known.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| {
        (0..6)
            .min_by_key(|&i| (i32::from(CUBE[i]) - i32::from(v)).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let cube = (CUBE[ri], CUBE[gi], CUBE[bi]);
    let avg = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    let grey_step = ((avg.saturating_sub(8) + 5) / 10).min(23) as u8;
    let grey_level = 8 + 10 * grey_step;
    let grey = (grey_level, grey_level, grey_level);
    if distance((r, g, b), grey) < distance((r, g, b), cube) {
        232 + grey_step
    } else {
        cube_index as u8
    }
}

/// The RGB of a 256-palette index from 16 up.
fn palette_256(i: u8) -> (u8, u8, u8) {
    if i >= 232 {
        let v = 8 + 10 * (i - 232);
        return (v, v, v);
    }
    let i = i.saturating_sub(16) as usize;
    (CUBE[i / 36], CUBE[(i / 6) % 6], CUBE[i % 6])
}

/// The closest of the 16 ANSI colours, by xterm's default values for them.
fn nearest_16(r: u8, g: u8, b: u8) -> Color {
    const ANSI: [(Color, (u8, u8, u8)); 16] = [
        (Color::Black, (0, 0, 0)),
        (Color::Red, (205, 0, 0)),
        (Color::Green, (0, 205, 0)),
        (Color::Yellow, (205, 205, 0)),
        (Color::Blue, (0, 0, 238)),
        (Color::Magenta, (205, 0, 205)),
        (Color::Cyan, (0, 205, 205)),
        (Color::Gray, (229, 229, 229)),
        (Color::DarkGray, (127, 127, 127)),
        (Color::LightRed, (255, 0, 0)),
        (Color::LightGreen, (0, 255, 0)),
        (Color::LightYellow, (255, 255, 0)),
        (Color::LightBlue, (92, 92, 255)),
        (Color::LightMagenta, (255, 0, 255)),
        (Color::LightCyan, (0, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];
    ANSI.iter()
        .min_by_key(|(_, rgb)| distance((r, g, b), *rgb))
        .map_or(Color::Reset, |(c, _)| *c)
}

#[cfg(test)]
mod tests {
    use super::ColorDepth;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn colorterm_truecolor_is_truecolor() {
        for value in ["truecolor", "24bit", "TrueColor"] {
            assert_eq!(
                ColorDepth::detect(env(&[("COLORTERM", value), ("TERM", "xterm-256color")])),
                ColorDepth::TrueColor,
                "{value}"
            );
        }
    }

    #[test]
    fn a_256color_term_without_colorterm_is_256() {
        assert_eq!(
            ColorDepth::detect(env(&[("TERM", "xterm-256color")])),
            ColorDepth::Ansi256
        );
        assert_eq!(
            ColorDepth::detect(env(&[
                ("TERM", "xterm-256color"),
                ("TERM_PROGRAM", "Apple_Terminal")
            ])),
            ColorDepth::Ansi256
        );
        assert_eq!(
            ColorDepth::detect(env(&[("TERM", "screen-256color")])),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn the_linux_console_and_unknown_terms_are_16() {
        assert_eq!(
            ColorDepth::detect(env(&[("TERM", "linux")])),
            ColorDepth::Ansi16
        );
        assert_eq!(ColorDepth::detect(env(&[])), ColorDepth::Ansi16);
    }

    /// Negative: terminals that draw 24-bit colour but do not set
    /// `COLORTERM` (it is often lost over SSH) keep truecolor, and so does
    /// tmux, which converts colours itself.
    #[test]
    fn known_truecolor_terminals_keep_truecolor_without_colorterm() {
        for vars in [
            [("TERM", "xterm-ghostty"), ("TERM_PROGRAM", "")],
            [("TERM", "xterm-kitty"), ("TERM_PROGRAM", "")],
            [("TERM", "alacritty"), ("TERM_PROGRAM", "")],
            [("TERM", "xterm-256color"), ("TERM_PROGRAM", "iTerm.app")],
            [("TERM", "xterm-256color"), ("TERM_PROGRAM", "WezTerm")],
            [("TERM", "xterm-256color"), ("TERM_PROGRAM", "vscode")],
            [("TERM", "tmux-256color"), ("TERM_PROGRAM", "tmux")],
            [("TERM", "xterm-256color"), ("WT_SESSION", "abc")],
        ] {
            assert_eq!(
                ColorDepth::detect(env(&vars)),
                ColorDepth::TrueColor,
                "{vars:?}"
            );
        }
    }

    #[test]
    fn the_setting_names_a_depth() {
        assert_eq!(
            ColorDepth::from_setting("truecolor"),
            Some(ColorDepth::TrueColor)
        );
        assert_eq!(ColorDepth::from_setting("256"), Some(ColorDepth::Ansi256));
        assert_eq!(ColorDepth::from_setting("16"), Some(ColorDepth::Ansi16));
        assert_eq!(ColorDepth::from_setting("auto"), None);
        assert_eq!(ColorDepth::from_setting("88"), None);
    }

    /// The issue's cases: the editor greys land on the grey ramp and black
    /// on the cube's black.
    #[test]
    fn at_256_colours_rgb_maps_to_the_nearest_palette_index() {
        let d = ColorDepth::Ansi256;
        assert_eq!(d.fit(Color::Rgb(0x1e, 0x1e, 0x1e)), Color::Indexed(234));
        assert_eq!(d.fit(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(d.fit(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(d.fit(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(d.fit(Color::Rgb(0x56, 0x9c, 0xd6)), Color::Indexed(74));
    }

    #[test]
    fn at_16_colours_rgb_and_high_indexes_map_to_ansi_names() {
        let d = ColorDepth::Ansi16;
        assert_eq!(d.fit(Color::Rgb(0x1e, 0x1e, 0x1e)), Color::Black);
        assert_eq!(d.fit(Color::Rgb(250, 10, 10)), Color::LightRed);
        assert_eq!(d.fit(Color::Indexed(231)), Color::White);
        assert_eq!(d.fit(Color::Indexed(1)), Color::Indexed(1));
    }

    /// Negative: named, indexed and reset colours are what the terminal
    /// already has, so they pass through untouched.
    #[test]
    fn named_and_reset_colours_pass_through() {
        for d in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
            for c in [Color::Reset, Color::Red, Color::Indexed(7)] {
                assert_eq!(d.fit(c), c, "{d:?} {c:?}");
            }
        }
        assert_eq!(
            ColorDepth::Ansi256.fit(Color::Indexed(200)),
            Color::Indexed(200)
        );
    }

    #[test]
    fn a_frame_below_truecolor_holds_no_rgb() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        for cell in &mut buf.content {
            cell.fg = Color::Rgb(0x1e, 0x1e, 0x1e);
            cell.bg = Color::Rgb(0, 0, 0);
            cell.underline_color = Color::Rgb(255, 0, 0);
        }
        ColorDepth::Ansi256.fit_buffer(&mut buf);
        for cell in &buf.content {
            assert_eq!(cell.fg, Color::Indexed(234));
            assert_eq!(cell.bg, Color::Indexed(16));
            assert_eq!(cell.underline_color, Color::Indexed(196));
        }
    }

    /// Negative: at truecolor the frame is left exactly as drawn.
    #[test]
    fn a_truecolor_frame_is_unchanged() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        buf.content[0].fg = Color::Rgb(1, 2, 3);
        let before = buf.clone();
        ColorDepth::TrueColor.fit_buffer(&mut buf);
        assert_eq!(buf, before);
    }
}
