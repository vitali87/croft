//! Screen snapshots ("goldens"): whole frames written as diff-friendly
//! text and compared against text files saved beside this module.
//!
//! A frame is written as a header, every row's symbols, the cells that are
//! not one plain narrow glyph, and every row's style runs with exact
//! colours, so a change to any cell, colour, modifier, glyph or the caret
//! shows as a diff of the first differing lines. On a mismatch the actual
//! text is saved for inspection; blessing rewrites the golden instead of
//! comparing, and a missing golden fails unless blessed.

use super::*;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Position;
use unicode_width::UnicodeWidthStr;

/// The fill a masked cell is serialised with.
const MASK_SYMBOL: &str = "\u{2591}";

/// What a cell that is not one printable glyph is written as in its row,
/// so the row keeps one column per cell. The `cells` section says what the
/// cell really held.
const ODD_SYMBOL: &str = "\u{00a4}";

/// A named region painted over before a frame is compared.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Mask {
    name: &'static str,
    area: Rect,
}

/// The caret as the terminal would show it after the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Caret {
    shown: bool,
    at: Position,
}

/// One drawn frame: what the terminal holds after it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shot {
    buffer: Buffer,
    caret: Caret,
}

/// Paint `masks` over `shot` with the fixed fill.
fn apply_masks(shot: &mut Shot, masks: &[Mask]) {
    for mask in masks {
        for y in mask.area.top()..mask.area.bottom() {
            for x in mask.area.left()..mask.area.right() {
                let cell = &mut shot.buffer[(x, y)];
                cell.reset();
                cell.set_symbol(MASK_SYMBOL);
            }
        }
    }
}

/// How a colour is written: `#rrggbb`, `idx<n>` or the ANSI name.
fn colour(c: Color) -> String {
    match c {
        Color::Reset => String::from("reset"),
        Color::Black => String::from("black"),
        Color::Red => String::from("red"),
        Color::Green => String::from("green"),
        Color::Yellow => String::from("yellow"),
        Color::Blue => String::from("blue"),
        Color::Magenta => String::from("magenta"),
        Color::Cyan => String::from("cyan"),
        Color::Gray => String::from("gray"),
        Color::DarkGray => String::from("darkgray"),
        Color::LightRed => String::from("lightred"),
        Color::LightGreen => String::from("lightgreen"),
        Color::LightYellow => String::from("lightyellow"),
        Color::LightBlue => String::from("lightblue"),
        Color::LightMagenta => String::from("lightmagenta"),
        Color::LightCyan => String::from("lightcyan"),
        Color::White => String::from("white"),
        Color::Indexed(i) => format!("idx{i}"),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// Whether `c` is in a Unicode private use area, where Nerd Font icons live.
fn private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// A cell's style as one run key: colours, then modifiers and `skip`.
fn style_key(cell: &Cell) -> String {
    let mut key = format!("fg={} bg={}", colour(cell.fg), colour(cell.bg));
    if cell.underline_color != Color::Reset {
        key.push_str(&format!(" ul={}", colour(cell.underline_color)));
    }
    if !cell.modifier.is_empty() {
        let names: Vec<&str> = cell.modifier.iter_names().map(|(name, _)| name).collect();
        key.push_str(&format!(" mod={}", names.join("+")));
    }
    if cell.skip {
        key.push_str(" skip");
    }
    key
}

/// What a scene is, for the header.
struct Header<'a> {
    scene: &'a str,
    theme: crate::theme::Theme,
    depth: crate::color_depth::ColorDepth,
    /// The first frame that already showed the compared one (1: the first
    /// frame drawn was final).
    settled: usize,
}

/// `shot` as the golden text: header, symbols, odd cells, style runs.
fn serialise(header: &Header, shot: &Shot, masks: &[Mask]) -> String {
    use std::fmt::Write as _;
    let area = shot.buffer.area;
    let depth = match header.depth {
        crate::color_depth::ColorDepth::TrueColor => "truecolor",
        crate::color_depth::ColorDepth::Ansi256 => "256",
        crate::color_depth::ColorDepth::Ansi16 => "16",
    };
    let mut out = String::new();
    let _ = writeln!(out, "croft screen golden v1");
    let _ = writeln!(out, "scene: {}", header.scene);
    let _ = writeln!(out, "size: {}x{}", area.width, area.height);
    let _ = writeln!(out, "theme: {}", header.theme.id());
    let _ = writeln!(out, "colour depth: {depth}");
    let frames = if header.settled == 1 {
        "frame"
    } else {
        "frames"
    };
    let _ = writeln!(out, "settled after: {} {frames}", header.settled);
    if shot.caret.shown {
        let _ = writeln!(
            out,
            "caret: row {} col {}",
            shot.caret.at.y, shot.caret.at.x
        );
    } else {
        let _ = writeln!(out, "caret: hidden");
    }
    for m in masks {
        let _ = writeln!(
            out,
            "mask: {} rows {}..{} cols {}..{}",
            m.name,
            m.area.top(),
            m.area.bottom(),
            m.area.left(),
            m.area.right()
        );
    }
    let _ = writeln!(out, "--- symbols");
    let mut odd = Vec::new();
    for y in 0..area.height {
        let mut row = String::new();
        // Cells a wide glyph to their left already covers: the terminal
        // shows the glyph there, so the row adds nothing for them.
        let mut covered = 0usize;
        for x in 0..area.width {
            let sym = shot.buffer[(x, y)].symbol();
            if covered > 0 {
                covered -= 1;
                if sym != " " {
                    odd.push(format!("r{y:02} c{x:03} hidden {sym:?}"));
                }
                continue;
            }
            let width = sym.width();
            if width == 0 || sym.chars().any(char::is_control) {
                odd.push(format!("r{y:02} c{x:03} odd {sym:?}"));
                row.push_str(ODD_SYMBOL);
                continue;
            }
            row.push_str(sym);
            if width > 1 {
                odd.push(format!("r{y:02} c{x:03} wide {sym:?} {width} cells"));
                covered = width - 1;
            } else if sym.chars().any(private_use) {
                // A Nerd Font icon is one cell to ratatui and to the
                // terminal, but a viewer without the font may draw it
                // narrower or wider, so the row alone could read shifted.
                let points: Vec<String> =
                    sym.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
                odd.push(format!("r{y:02} c{x:03} icon {}", points.join(" ")));
            }
        }
        let _ = writeln!(out, "{y:02}|{row}|");
    }
    let _ = writeln!(out, "--- cells");
    for line in odd {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out, "--- styles");
    for y in 0..area.height {
        let mut x = 0;
        while x < area.width {
            let key = style_key(&shot.buffer[(x, y)]);
            let start = x;
            while x < area.width && style_key(&shot.buffer[(x, y)]) == key {
                x += 1;
            }
            let _ = writeln!(out, "r{y:02} c{start:03}-{:03} {key}", x - 1);
        }
    }
    out
}

/// Where goldens are read from (at run time, so a blessed file needs no
/// rebuild) and where a mismatching scene's actual text is written.
struct Dirs {
    golden: PathBuf,
    actual: PathBuf,
}

/// The first differing lines of `expected` and `actual` (symbol rows,
/// cells and style runs alike), as a unified diff; `None` when they match.
fn first_differences(expected: &str, actual: &str) -> Option<String> {
    const MAX_LINES: usize = 60;
    if expected == actual {
        return None;
    }
    let diff = similar::TextDiff::from_lines(expected, actual);
    let text = diff
        .unified_diff()
        .context_radius(1)
        .header("golden", "actual")
        .to_string();
    let mut lines: Vec<&str> = text.lines().take(MAX_LINES + 1).collect();
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES);
        lines.push("... (more differences follow)");
    }
    Some(lines.join("\n"))
}

/// Compare `actual` with scene `name`'s golden in `dirs`, or rewrite the
/// golden when `bless`.
fn compare(dirs: &Dirs, name: &str, actual: &str, bless: bool) -> Result<(), String> {
    let path = dirs.golden.join(format!("{name}.txt"));
    if bless {
        std::fs::create_dir_all(&dirs.golden).unwrap();
        std::fs::write(&path, actual).unwrap();
        return Ok(());
    }
    let expected = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "no golden for scene {name:?} at {} ({e}); run with CROFT_BLESS=1 to write it",
            path.display()
        )
    })?;
    let Some(diff) = first_differences(&expected, actual) else {
        return Ok(());
    };
    let out = dirs.actual.join(format!("{name}.txt"));
    std::fs::create_dir_all(&dirs.actual).unwrap();
    std::fs::write(&out, actual).unwrap();
    Err(format!(
        "scene {name:?} no longer matches its golden {}.\nThe actual frame is in {}; \
         if the change is intended, rerun with CROFT_BLESS=1.\n{diff}",
        path.display(),
        out.display(),
    ))
}

/// A hand-built shot of `width` x 1 cells for the harness's own tests.
fn shot_of(width: u16, paint: impl FnOnce(&mut Buffer)) -> Shot {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    paint(&mut buffer);
    Shot {
        buffer,
        caret: Caret {
            shown: false,
            at: Position::new(0, 0),
        },
    }
}

/// `shot` serialised under a fixed header.
fn text_of(shot: &Shot, masks: &[Mask]) -> String {
    let header = Header {
        scene: "harness",
        theme: crate::theme::Theme::default(),
        depth: crate::color_depth::ColorDepth::TrueColor,
        settled: 1,
    };
    serialise(&header, shot, masks)
}

/// The lines of `section` (`symbols`, `cells` or `styles`) in `text`.
fn section<'a>(text: &'a str, section: &str) -> Vec<&'a str> {
    let start = format!("--- {section}");
    text.lines()
        .skip_while(|l| *l != start)
        .skip(1)
        .take_while(|l| !l.starts_with("--- "))
        .collect()
}

#[test]
fn golden_rows_keep_one_column_per_cell_around_wide_and_icon_glyphs() {
    let red = Style::default().fg(Color::Red);
    let shot = shot_of(12, |b| {
        b.set_string(0, 0, "a\u{4e2d}b\u{1f980}\u{e7a8}", Style::default());
        b.set_string(7, 0, "d", red);
        b[(8, 0)].set_symbol("");
        b.set_string(9, 0, "e", red);
    });
    let text = text_of(&shot, &[]);
    // Each wide glyph is followed by nothing for the cell it covers, so the
    // row is exactly 12 columns wide and `d` and `e` stay where they were
    // drawn; the empty cell is written as one placeholder column.
    let row = section(&text, "symbols")[0];
    assert_eq!(row, "00|a\u{4e2d}b\u{1f980}\u{e7a8}d\u{a4}e  |");
    assert_eq!(UnicodeWidthStr::width(&row[3..row.len() - 1]), 12);
    assert_eq!(
        section(&text, "cells"),
        vec![
            "r00 c001 wide \"\u{4e2d}\" 2 cells",
            "r00 c004 wide \"\u{1f980}\" 2 cells",
            "r00 c006 icon U+E7A8",
            "r00 c008 odd \"\"",
        ]
    );
    let styles = section(&text, "styles");
    assert!(
        styles.contains(&"r00 c007-007 fg=red bg=reset"),
        "{styles:?}"
    );
    assert!(
        styles.contains(&"r00 c009-009 fg=red bg=reset"),
        "{styles:?}"
    );
}

#[test]
fn golden_lists_a_hidden_symbol_under_a_wide_glyph() {
    let shot = shot_of(4, |b| {
        b.set_string(0, 0, "\u{4e2d}", Style::default());
        b[(1, 0)].set_symbol("x");
    });
    let text = text_of(&shot, &[]);
    assert_eq!(section(&text, "symbols")[0], "00|\u{4e2d}  |");
    assert!(section(&text, "cells").contains(&"r00 c001 hidden \"x\""));
}

#[test]
fn golden_style_runs_write_every_colour_and_modifier_exactly() {
    let shot = shot_of(6, |b| {
        b.set_string(
            0,
            0,
            "ab",
            Style::default()
                .fg(Color::Rgb(0x12, 0xab, 0xef))
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        );
        b.set_string(2, 0, "c", Style::default().fg(Color::DarkGray));
        b.set_string(
            3,
            0,
            "d",
            Style::default()
                .fg(Color::Rgb(0x12, 0xab, 0xef))
                .underline_color(Color::LightMagenta)
                .add_modifier(Modifier::UNDERLINED),
        );
        b[(4, 0)].skip = true;
    });
    let text = text_of(&shot, &[]);
    assert_eq!(
        section(&text, "styles"),
        vec![
            "r00 c000-001 fg=#12abef bg=idx236 mod=BOLD+ITALIC",
            "r00 c002-002 fg=darkgray bg=reset",
            "r00 c003-003 fg=#12abef bg=reset ul=lightmagenta mod=UNDERLINED",
            "r00 c004-004 fg=reset bg=reset skip",
            "r00 c005-005 fg=reset bg=reset",
        ]
    );
}

#[test]
fn golden_masks_fill_their_region_and_are_named_in_the_header() {
    let mut shot = shot_of(6, |b| {
        b.set_string(0, 0, "v1.2.3", Style::default().fg(Color::Red));
    });
    let masks = [Mask {
        name: "version",
        area: Rect::new(1, 0, 4, 1),
    }];
    apply_masks(&mut shot, &masks);
    let text = text_of(&shot, &masks);
    assert!(
        text.contains("\nmask: version rows 0..1 cols 1..5\n"),
        "{text}"
    );
    assert_eq!(
        section(&text, "symbols")[0],
        "00|v\u{2591}\u{2591}\u{2591}\u{2591}3|"
    );
    assert!(section(&text, "styles").contains(&"r00 c001-004 fg=reset bg=reset"));
}

#[test]
fn golden_comparison_fails_on_one_changed_cell_or_colour_and_saves_the_actual() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = Dirs {
        golden: tmp.path().join("golden"),
        actual: tmp.path().join("actual"),
    };
    let paint = |b: &mut Buffer| {
        b.set_string(0, 0, "fn main", Style::default().fg(Color::Rgb(1, 2, 3)));
    };
    let saved = text_of(&shot_of(8, paint), &[]);
    let missing = compare(&dirs, "scene", &saved, false).unwrap_err();
    assert!(missing.contains("CROFT_BLESS=1"), "{missing}");
    compare(&dirs, "scene", &saved, true).unwrap();
    compare(&dirs, "scene", &saved, false).unwrap();

    let mut glyph = shot_of(8, paint);
    glyph.buffer[(3, 0)].set_symbol("M");
    let err = compare(&dirs, "scene", &text_of(&glyph, &[]), false).unwrap_err();
    assert!(
        err.contains("-00|fn main |") && err.contains("+00|fn Main |"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(dirs.actual.join("scene.txt")).unwrap(),
        text_of(&glyph, &[])
    );

    let mut colour = shot_of(8, paint);
    colour.buffer[(3, 0)].fg = Color::Rgb(1, 2, 4);
    let err = compare(&dirs, "scene", &text_of(&colour, &[]), false).unwrap_err();
    assert!(err.contains("+r00 c003-003 fg=#010204 bg=reset"), "{err}");
    assert!(err.contains("-r00 c000-006 fg=#010203 bg=reset"), "{err}");
}
