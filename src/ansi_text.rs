//! SGR span parsing for the rendered ANSI log view (#257).
//!
//! A log file is a stream transcript, not a screen: it has unbounded line
//! width and no cursor. So this parses SGR (colour/attribute) sequences into
//! styled spans over the *stripped* text and discards every other escape —
//! cursor movement, OSC, scroll regions — rather than interpreting them.
//! Driving a real terminal grid instead would impose a fixed width the file
//! does not have.
//!
//! Colours stay symbolic ([`AnsiColor::Indexed`] for the low 16) so the
//! active theme's palette resolves them at paint time and a theme switch
//! recolours the view without reparsing.

/// A colour named by an SGR sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnsiColor {
    /// One of the 16 base slots, resolved through the theme's ANSI palette.
    /// Kept symbolic so a theme switch recolours without a reparse.
    Indexed(u8),
    /// A 256-colour cube / greyscale entry (SGR 38;5;n) outside the low 16.
    Palette256(u8),
    /// A 24-bit colour (SGR 38;2;r;g;b).
    Rgb(u8, u8, u8),
}

/// The style in force for a run of characters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnsiStyle {
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    /// SGR 7. Swapping fg/bg is left to the painter, which knows the
    /// theme's default pair to swap against.
    pub inverse: bool,
}

impl AnsiStyle {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A run of characters sharing one style. `start`/`end` are byte offsets into
/// the STRIPPED line text, so a caller can slice the visible string directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnsiSpan {
    pub start: usize,
    pub end: usize,
    pub style: AnsiStyle,
}

/// One parsed line: the escape-free text plus the spans that colour it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnsiLine {
    pub text: String,
    pub spans: Vec<AnsiSpan>,
}

/// Parse one line, carrying `style` in and out so colours persist across
/// lines the way a terminal does (a log that sets red on one line and resets
/// three lines later paints all three red).
pub fn parse_line(raw: &str, style: &mut AnsiStyle) -> AnsiLine {
    let mut out = AnsiLine::default();
    let mut run_start = 0usize;
    let mut run_style = *style;
    let bytes = raw.as_bytes();
    let mut i = 0usize;

    let close_run = |out: &mut AnsiLine, end: usize, start: usize, st: AnsiStyle| {
        if end > start {
            out.spans.push(AnsiSpan {
                start,
                end,
                style: st,
            });
        }
    };

    while i < bytes.len() {
        if bytes[i] != 0x1b {
            // Copy through to the next ESC in one go; `raw` is valid UTF-8 and
            // ESC never appears inside a multi-byte sequence, so slicing at an
            // ESC boundary is always a char boundary.
            let next = bytes[i..].iter().position(|&b| b == 0x1b);
            let end = next.map(|n| i + n).unwrap_or(bytes.len());
            out.text.push_str(&raw[i..end]);
            i = end;
            continue;
        }
        // An ESC at end-of-line has no sequence to close it: emit it as text
        // rather than swallowing a byte the file really contains.
        let Some(&next) = bytes.get(i + 1) else {
            out.text.push('\u{1b}');
            i += 1;
            continue;
        };
        match next {
            b'[' => {
                // CSI: params until a byte in 0x40..=0x7e ends it.
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                if j >= bytes.len() {
                    // Truncated sequence (the file ends mid-escape): drop it.
                    i = bytes.len();
                    continue;
                }
                if bytes[j] == b'm' {
                    let params = &raw[i + 2..j];
                    let end = out.text.len();
                    close_run(&mut out, end, run_start, run_style);
                    apply_sgr(params, style);
                    run_start = end;
                    run_style = *style;
                }
                // Every other CSI final byte (cursor movement, erase, …) is
                // dropped: a transcript has no cursor to move.
                i = j + 1;
            }
            b']' => {
                // OSC: ends at BEL or ST (ESC \). Dropped entirely — a title
                // or hyperlink has no meaning in a static buffer.
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j;
            }
            // Two-byte escapes (charset selection, RI, …): drop both bytes.
            _ => i += 2,
        }
    }
    let end = out.text.len();
    close_run(&mut out, end, run_start, run_style);
    out
}

/// Apply one SGR parameter string to `style`. Unknown parameters are ignored
/// rather than aborting the sequence, matching what terminals do.
fn apply_sgr(params: &str, style: &mut AnsiStyle) {
    // A bare `ESC[m` means reset, same as `ESC[0m`.
    if params.is_empty() {
        style.reset();
        return;
    }
    let parts: Vec<&str> = params.split(';').collect();
    let mut k = 0usize;
    while k < parts.len() {
        // An empty parameter is a zero (`ESC[;31m` is reset then red).
        let n: u16 = if parts[k].is_empty() {
            0
        } else {
            match parts[k].parse() {
                Ok(v) => v,
                Err(_) => {
                    k += 1;
                    continue;
                }
            }
        };
        match n {
            0 => style.reset(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            7 => style.inverse = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            27 => style.inverse = false,
            30..=37 => style.fg = Some(AnsiColor::Indexed((n - 30) as u8)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(AnsiColor::Indexed((n - 40) as u8)),
            49 => style.bg = None,
            // Bright aliases occupy palette slots 8..15.
            90..=97 => style.fg = Some(AnsiColor::Indexed((n - 90 + 8) as u8)),
            100..=107 => style.bg = Some(AnsiColor::Indexed((n - 100 + 8) as u8)),
            38 | 48 => {
                let (colour, consumed) = extended_color(&parts[k + 1..]);
                if let Some(c) = colour {
                    if n == 38 {
                        style.fg = Some(c);
                    } else {
                        style.bg = Some(c);
                    }
                }
                k += consumed;
            }
            _ => {}
        }
        k += 1;
    }
}

/// Parse the tail of a `38`/`48` sequence: `5;n` (palette) or `2;r;g;b`
/// (truecolor). Returns the colour and how many extra parameters it ate, so a
/// malformed sequence consumes what it can instead of desynchronising the
/// rest of the parameter list.
fn extended_color(rest: &[&str]) -> (Option<AnsiColor>, usize) {
    let num = |s: &str| s.parse::<u16>().ok();
    match rest.first().and_then(|s| num(s)) {
        Some(5) => match rest.get(1).and_then(|s| num(s)) {
            Some(v) if v < 256 => {
                let v = v as u8;
                // The low 16 stay symbolic so the theme palette owns them.
                let c = if v < 16 {
                    AnsiColor::Indexed(v)
                } else {
                    AnsiColor::Palette256(v)
                };
                (Some(c), 2)
            }
            _ => (None, 1),
        },
        Some(2) => {
            let r = rest.get(1).and_then(|s| num(s));
            let g = rest.get(2).and_then(|s| num(s));
            let b = rest.get(3).and_then(|s| num(s));
            match (r, g, b) {
                (Some(r), Some(g), Some(b)) if r < 256 && g < 256 && b < 256 => {
                    (Some(AnsiColor::Rgb(r as u8, g as u8, b as u8)), 4)
                }
                _ => (None, 1),
            }
        }
        _ => (None, 0),
    }
}

/// Whether `sample` looks like ANSI-coloured output: an SGR sequence in the
/// sniffed prefix. Deliberately narrower than "contains ESC" — a file full of
/// cursor movement is a terminal recording, not a colour log, and rendering
/// it as if the escapes were decoration would mislead.
pub fn looks_like_ansi(sample: &str) -> bool {
    let b = sample.as_bytes();
    for i in 0..b.len() {
        if b[i] != 0x1b || b.get(i + 1) != Some(&b'[') {
            continue;
        }
        let mut j = i + 2;
        while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
            j += 1;
        }
        if b.get(j) == Some(&b'm') {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> AnsiLine {
        let mut st = AnsiStyle::default();
        parse_line(raw, &mut st)
    }

    #[test]
    fn plain_text_yields_one_unstyled_span() {
        let l = parse("hello");
        assert_eq!(l.text, "hello");
        assert_eq!(l.spans.len(), 1);
        assert_eq!(l.spans[0].style, AnsiStyle::default());
    }

    #[test]
    fn basic_colors_map_to_palette_slots_and_text_is_stripped() {
        let l = parse("\u{1b}[31mred\u{1b}[0m plain");
        assert_eq!(l.text, "red plain", "escapes never reach the text");
        assert_eq!(l.spans[0].style.fg, Some(AnsiColor::Indexed(1)));
        assert_eq!(&l.text[l.spans[0].start..l.spans[0].end], "red");
        assert_eq!(l.spans[1].style, AnsiStyle::default());
    }

    #[test]
    fn bright_aliases_land_in_the_upper_palette_half() {
        let l = parse("\u{1b}[91mbright\u{1b}[0m");
        assert_eq!(l.spans[0].style.fg, Some(AnsiColor::Indexed(9)));
        let l = parse("\u{1b}[103mbg\u{1b}[0m");
        assert_eq!(l.spans[0].style.bg, Some(AnsiColor::Indexed(11)));
    }

    #[test]
    fn palette_and_truecolor_parse_with_their_parameters_consumed() {
        let l = parse("\u{1b}[38;5;208morange\u{1b}[0m");
        assert_eq!(l.spans[0].style.fg, Some(AnsiColor::Palette256(208)));
        let l = parse("\u{1b}[38;2;10;20;30mrgb\u{1b}[0m");
        assert_eq!(l.spans[0].style.fg, Some(AnsiColor::Rgb(10, 20, 30)));
        // The low 16 stay symbolic even when spelled the 256-colour way, so
        // the theme palette still owns them.
        let l = parse("\u{1b}[38;5;3myellow\u{1b}[0m");
        assert_eq!(l.spans[0].style.fg, Some(AnsiColor::Indexed(3)));
    }

    #[test]
    fn a_truecolor_sequence_does_not_swallow_following_attributes() {
        // The r;g;b parameters must be consumed, or the `1` below would be
        // read as a colour component and bold would be lost.
        let l = parse("\u{1b}[38;2;1;2;3;1mx\u{1b}[0m");
        assert_eq!(l.spans[0].style.fg, Some(AnsiColor::Rgb(1, 2, 3)));
        assert!(l.spans[0].style.bold, "the trailing 1 is still bold");
    }

    #[test]
    fn attributes_set_and_clear_independently() {
        let l = parse("\u{1b}[1;3;4ma\u{1b}[23mb\u{1b}[24mc\u{1b}[22md");
        assert!(l.spans[0].style.bold && l.spans[0].style.italic && l.spans[0].style.underline);
        assert!(!l.spans[1].style.italic && l.spans[1].style.underline);
        assert!(!l.spans[2].style.underline && l.spans[2].style.bold);
        assert!(!l.spans[3].style.bold);
    }

    #[test]
    fn non_sgr_escapes_are_stripped_not_rendered() {
        // Cursor movement, erase-line, and OSC titles carry no meaning in a
        // static buffer; none of them may leak into the text.
        let l = parse("\u{1b}[2Ka\u{1b}[10;20Hb\u{1b}]0;title\u{7}c");
        assert_eq!(l.text, "abc");
        let l = parse("\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\");
        assert_eq!(l.text, "link", "OSC-8 hyperlinks strip to their label");
    }

    #[test]
    fn style_carries_across_lines_like_a_terminal() {
        let mut st = AnsiStyle::default();
        let a = parse_line("\u{1b}[31mred starts", &mut st);
        let b = parse_line("still red", &mut st);
        assert_eq!(a.spans[0].style.fg, Some(AnsiColor::Indexed(1)));
        assert_eq!(
            b.spans[0].style.fg,
            Some(AnsiColor::Indexed(1)),
            "no reset means the next line stays red"
        );
        let c = parse_line("\u{1b}[0mplain", &mut st);
        assert_eq!(c.spans[0].style.fg, None);
    }

    #[test]
    fn malformed_and_truncated_sequences_do_not_panic_or_eat_text() {
        assert_eq!(parse("\u{1b}[999999999999m x").text, " x");
        assert_eq!(parse("\u{1b}[38;5mtruncated").text, "truncated");
        assert_eq!(parse("\u{1b}[38;2;1;2mshort").text, "short");
        // A trailing ESC is real content, not the start of a sequence.
        assert_eq!(parse("tail\u{1b}").text, "tail\u{1b}");
        // Unterminated CSI at EOF: dropped, and nothing after it exists.
        assert_eq!(parse("head\u{1b}[31").text, "head");
    }

    #[test]
    fn multibyte_text_survives_slicing_at_escape_boundaries() {
        let l = parse("héllo \u{1b}[31mwörld 🙂\u{1b}[0m ok");
        assert_eq!(l.text, "héllo wörld 🙂 ok");
        // Spans are byte offsets into the stripped text, so slicing them must
        // land on char boundaries.
        for s in &l.spans {
            assert!(l.text.is_char_boundary(s.start) && l.text.is_char_boundary(s.end));
        }
        assert_eq!(&l.text[l.spans[1].start..l.spans[1].end], "wörld 🙂");
    }

    #[test]
    fn bare_and_empty_sgr_parameters_reset() {
        let mut st = AnsiStyle {
            bold: true,
            fg: Some(AnsiColor::Indexed(2)),
            ..Default::default()
        };
        parse_line("\u{1b}[mx", &mut st);
        assert_eq!(st, AnsiStyle::default(), "ESC[m is a reset");
        let l = parse("\u{1b}[1m\u{1b}[;31mx");
        assert_eq!(
            l.spans.last().unwrap().style.fg,
            Some(AnsiColor::Indexed(1))
        );
        assert!(
            !l.spans.last().unwrap().style.bold,
            "the empty parameter reset bold before red applied"
        );
    }

    #[test]
    fn sniffing_requires_an_sgr_sequence_not_just_an_escape() {
        assert!(looks_like_ansi("plain \u{1b}[31mred"));
        assert!(looks_like_ansi("\u{1b}[1;32mok"));
        assert!(
            !looks_like_ansi("\u{1b}[2J\u{1b}[10;1H"),
            "a screen recording is not a colour log"
        );
        assert!(!looks_like_ansi("no escapes here"));
        assert!(!looks_like_ansi("\u{1b}[31"), "truncated is not a match");
    }
}
