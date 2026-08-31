//! The orange→green corner gradient shared by the welcome "Recent Activity"
//! box and, under the Black theme, the focused-pane border. Defined once here
//! so both surfaces sweep through identical colours.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

pub const GRAD_TL: (u8, u8, u8) = (0x5c, 0xd6, 0xc8);
pub const GRAD_TR: (u8, u8, u8) = (0xec, 0x8c, 0x5a);
pub const GRAD_BR: (u8, u8, u8) = (0x4f, 0xb1, 0xa6);
pub const GRAD_BL: (u8, u8, u8) = (0x35, 0x80, 0x78);

/// Muted dark-teal fill for the selected row of popups/menus under the Black
/// theme, replacing the legacy bright-blue (`#4e9aff`) highlight inherited from
/// the pre-brand VS Code accent. Quiet enough that the gradient border carries
/// the brand identity; white text reads cleanly on top.
pub const POPUP_SEL_BG: (u8, u8, u8) = (0x26, 0x4f, 0x4a);

/// Bright teal/cyan card accent (borders, line-art illustrations, links) on
/// the empty-state cards under the Black theme — the same value the Remote
/// Explorer card has always used locally as `CARD_BORDER`/`LINK_FG`.
pub const CARD_ACCENT: (u8, u8, u8) = (0x3e, 0xd8, 0xc4);

/// Brand-teal fill for primary action buttons (`Initialize Repository`,
/// `Run and Debug`, the Remote Explorer `Connect`) under the Black theme,
/// replacing the legacy VS Code button blue (#0967b8). Same value the remote
/// empty-state card has always used, promoted here as the single source.
pub const PRIMARY_BTN_BG: (u8, u8, u8) = (0x0e, 0x7e, 0x76);

/// VS Code's `panelTitle.activeForeground` (#E7E7E7): the chipless panel
/// header text under the Black theme (e.g. the TERMINAL title), replacing the
/// legacy white-on-navy chip so the gradient border alone carries the brand.
pub const PANEL_TITLE_FG: (u8, u8, u8) = (0xe7, 0xe7, 0xe7);

/// Muted brand teal (the gradient's bottom-right corner) used as the inner
/// stroke accent under the Black theme — input-box focus rings, text cursors,
/// chevrons, and magnifier glyphs — replacing the legacy bright-blue
/// (`#4e9aff`). Quieter than the top-left teal so inner chrome doesn't shout
/// over the gradient border that carries the brand.
pub const INNER_ACCENT: (u8, u8, u8) = (0x4f, 0xb1, 0xa6);

pub fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| ((1.0 - t) * x as f32 + t * y as f32).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

pub fn rgb_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Paint a rounded-rectangle border whose stroke colour interpolates
/// linearly between the four corner colours along each edge. The interior
/// is left untouched so the caller can fill it with content.
///
/// The rect is clipped against the buffer's area, so callers don't have to
/// do the bounds math themselves — passing a rect that runs off the edge
/// (e.g., a 80x25 default startup buffer with a tall recents list) draws
/// nothing instead of panicking inside `set_string`.
pub fn paint_gradient_box(buf: &mut Buffer, rect: Rect) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let buf_area = buf.area;
    if rect.x < buf_area.x
        || rect.y < buf_area.y
        || rect.x + rect.width > buf_area.x + buf_area.width
        || rect.y + rect.height > buf_area.y + buf_area.height
    {
        return;
    }
    let max_x = rect.width - 1;
    let max_y = rect.height - 1;
    for x in 0..rect.width {
        let u = if max_x > 0 {
            x as f32 / max_x as f32
        } else {
            0.0
        };
        let top = lerp_rgb(GRAD_TL, GRAD_TR, u);
        let bot = lerp_rgb(GRAD_BL, GRAD_BR, u);
        let top_ch = if x == 0 {
            "\u{256d}"
        } else if x == max_x {
            "\u{256e}"
        } else {
            "\u{2500}"
        };
        let bot_ch = if x == 0 {
            "\u{2570}"
        } else if x == max_x {
            "\u{256f}"
        } else {
            "\u{2500}"
        };
        buf.set_string(
            rect.x + x,
            rect.y,
            top_ch,
            Style::default().fg(rgb_color(top)),
        );
        buf.set_string(
            rect.x + x,
            rect.y + max_y,
            bot_ch,
            Style::default().fg(rgb_color(bot)),
        );
    }
    for y in 1..max_y {
        let v = if max_y > 0 {
            y as f32 / max_y as f32
        } else {
            0.0
        };
        let left = lerp_rgb(GRAD_TL, GRAD_BL, v);
        let right = lerp_rgb(GRAD_TR, GRAD_BR, v);
        buf.set_string(
            rect.x,
            rect.y + y,
            "\u{2502}",
            Style::default().fg(rgb_color(left)),
        );
        buf.set_string(
            rect.x + max_x,
            rect.y + y,
            "\u{2502}",
            Style::default().fg(rgb_color(right)),
        );
    }
}

/// Dotted contour lines drawn into a card's interior (#312).
///
/// Three curves, phase-shifted, so the field reads as a landscape rather
/// than one repeated ripple.
const WAVE_CURVES: usize = 3;

/// Only every Nth column carries a dot.
///
/// Sampling one row per column put adjacent columns on the same row often
/// enough that the curves merged into solid dashes of eight to eleven
/// cells - a smear rather than the sparse dotted field the reference shows,
/// and dense enough that three curves read as one mass.
const WAVE_COLUMN_STEP: u16 = 3;

/// Where the field starts, as a fraction of the card's width.
///
/// The midpoint was too far left: it put the leading edge exactly where the
/// longest note lines end, which is what made the field collide with the
/// gaps between words in the first place. Starting later gives the text
/// real clearance and lets the ramp read as an emergence.
const WAVE_START: f32 = 0.68;

/// Paint the ambient dotted wave into the right of a card's interior (#312).
///
/// TO THE RIGHT OF THE TEXT, not merely into blank cells. A per-cell blank
/// test is not enough: the spaces BETWEEN WORDS of a release note are blank
/// cells, so a dot lands between "release" and "notes" and the note reads
/// as though it has been speckled. Each row's last occupied column is found
/// first and nothing is painted at or left of it, so the field can only
/// ever sit past the end of that row's text.
///
/// The card's text is therefore always intact, which is the contract: a
/// release with a lot to say pushes the field out of view rather than being
/// decorated over.
pub fn paint_card_wave(buf: &mut Buffer, inner: Rect, theme_ui: impl Fn(Color) -> Color) {
    if inner.width < 24 || inner.height < 3 {
        return;
    }
    let buf_area = buf.area;
    let w = inner.width as f32;
    let h = inner.height as f32;
    let start = ((w * WAVE_START) as u16).min(inner.width.saturating_sub(1));
    // Per row, the column after that row's last painted cell. A dot must
    // clear it, which is what keeps the field out of the gaps in a note.
    let text_end: Vec<u16> = (0..inner.height)
        .map(|row| {
            let y = inner.y + row;
            if y < buf_area.y || y >= buf_area.y + buf_area.height {
                return inner.width;
            }
            (0..inner.width)
                .rev()
                .find(|&col| {
                    let x = inner.x + col;
                    x >= buf_area.x
                        && x < buf_area.x + buf_area.width
                        && buf[(x, y)].symbol() != " "
                })
                .map_or(0, |c| c + 1)
        })
        .collect();
    for col in (start..inner.width).step_by(WAVE_COLUMN_STEP as usize) {
        let x = inner.x + col;
        if x < buf_area.x || x >= buf_area.x + buf_area.width {
            continue;
        }
        let ramp = (col - start) as f32 / (inner.width - start).max(1) as f32;
        let phase = col as f32 / w;
        // Back to front, so where two curves land on the same cell the NEAR
        // one wins. Painting front to back let a receding curve overwrite
        // the one that should sit in front of it.
        for curve in (0..WAVE_CURVES).rev() {
            let c = curve as f32;
            let peak = (phase * 6.0 + c * 0.9).sin() * 0.5 + 0.5;
            let level = h * (0.30 + 0.22 * c) + peak * h * 0.42;
            let row = level.round();
            if !(0.0..h).contains(&row) {
                continue;
            }
            let row_idx = row as u16;
            let y = inner.y + row_idx;
            if y < buf_area.y || y >= buf_area.y + buf_area.height {
                continue;
            }
            if col < text_end[row_idx as usize] {
                continue;
            }
            let cell = &mut buf[(x, y)];
            if cell.symbol() != " " {
                continue;
            }
            let tint = lerp_rgb(GRAD_BL, GRAD_TR, if curve == 0 { 0.85 } else { 0.10 });
            let faded = lerp_rgb(GRAD_BL, tint, 0.25 + 0.75 * ramp);
            cell.set_symbol("\u{b7}");
            // set_style REPLACES rather than patches: a cell reset by an
            // earlier paint can carry modifiers, and a dot inheriting BOLD
            // or REVERSED from whatever was there is not the same colour
            // the palette chose.
            cell.set_style(Style::reset().fg(theme_ui(rgb_color(faded))));
        }
    }
}

#[cfg(test)]
mod wave_tests {
    use super::*;

    fn blank(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        })
    }

    fn area(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    fn dots_at(buf: &Buffer, w: u16, h: u16) -> Vec<(u16, u16)> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() == "\u{b7}")
            .collect()
    }

    /// The wave must not land in the gaps BETWEEN WORDS of a release note.
    ///
    /// The first version of this test filled every cell with `x`, which
    /// cannot exercise the defect it is named for: a per-cell blank check
    /// passes trivially when there are no blanks. Real notes are words
    /// separated by spaces, and those spaces are blank cells - so the field
    /// speckled the text it was supposed to sit beside. The fixture is now
    /// prose, and the rule is per ROW: nothing at or left of that row's
    /// last painted cell.
    #[test]
    fn the_wave_never_lands_inside_a_line_of_text() {
        let (w, h) = (76u16, 8u16);
        let mut buf = blank(w, h);
        let note = "fix: a release note with many spaces between its words";
        buf.set_string(0, 2, note, Style::default());
        buf.set_string(
            0,
            3,
            "and a second line that also has spaces",
            Style::default(),
        );
        paint_card_wave(&mut buf, area(w, h), |c| c);

        for (row, text) in [(2u16, note), (3, "and a second line that also has spaces")] {
            let end = text.chars().count() as u16;
            for (x, y) in dots_at(&buf, w, h) {
                assert!(
                    y != row || x >= end,
                    "a dot landed at column {x} of row {row}, inside {end}-cell text"
                );
            }
            // And the text itself is byte-for-byte what was painted.
            let painted: String = (0..end).map(|x| buf[(x, row)].symbol()).collect();
            assert_eq!(painted, text, "row {row} was altered");
        }
    }

    /// And it does paint, or the test above would pass against a function
    /// that does nothing at all.
    #[test]
    fn the_wave_paints_into_the_blank_right_of_a_card() {
        let (w, h) = (76u16, 8u16);
        let mut buf = blank(w, h);
        paint_card_wave(&mut buf, area(w, h), |c| c);
        let dots = dots_at(&buf, w, h);
        assert!(!dots.is_empty(), "the field is drawn on an empty card");
        let start = (w as f32 * WAVE_START) as u16;
        assert!(
            dots.iter().all(|&(x, _)| x >= start),
            "nothing lands left of the fade point at column {start}"
        );
    }

    /// Sparse, not a smear. Sampling every column put adjacent dots on the
    /// same row often enough that the curves merged into solid dashes.
    #[test]
    fn the_field_is_sparse_enough_to_read_as_dots() {
        let (w, h) = (76u16, 8u16);
        let mut buf = blank(w, h);
        paint_card_wave(&mut buf, area(w, h), |c| c);
        for y in 0..h {
            let mut run = 0u16;
            for x in 0..w {
                if buf[(x, y)].symbol() == "\u{b7}" {
                    run += 1;
                    assert!(
                        run <= 1,
                        "row {y} has a run of {run} dots, which reads as a dash"
                    );
                } else {
                    run = 0;
                }
            }
        }
    }

    /// A card too small to carry a field gets none rather than a stripe.
    #[test]
    fn a_narrow_card_gets_no_wave() {
        let mut buf = blank(20, 8);
        let before = buf.clone();
        paint_card_wave(&mut buf, area(20, 8), |c| c);
        assert_eq!(buf, before);
    }
}
